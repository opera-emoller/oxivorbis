/*
 * oracle-encoder — own-built libvorbis encoder, pinned compile flags.
 *
 * Reads interleaved PCM from stdin, writes Ogg Vorbis to stdout.
 * Args: [bitrate options] <rate> <channels> [base_quality [format [comment]]]
 *       e.g. ./oracle-encoder 44100 1; any channel count libvorbis
 *       accepts (1..=255), interleaved in Vorbis channel order.
 *       base_quality: libvorbis -0.1..1.0 scale (q-1..q10), default 0.5.
 *       format: s16 (default; i16 LE, fed as sample/32768.0f) or f32
 *               (f32 LE, fed to vorbis_analysis_buffer unchanged).
 *       comment: optional "KEY=value" user comment (vorbis_comment_add);
 *               none by default. Used by tools/gen-header-vectors.
 *
 * Bitrate options (any of them switches from vorbis_encode_init_vbr to
 * oggenc 1.4.3's setup sequence in oggenc/encode.c, replayed in
 * setup_like_oggenc below):
 *       -b KBPS      oggenc -b (nominal bitrate)
 *       -m KBPS      oggenc -m (hard minimum; implies --managed)
 *       -M KBPS      oggenc -M (hard maximum; implies --managed)
 *       --managed    oggenc --managed
 *       -q           oggenc -q given: quality mode at base_quality
 *       -A KEY=VAL   oggenc --advanced-encode-option KEY=VAL, any of
 *                    oggenc's: bitrate_average_damping, bitrate_average,
 *                    bit_reservoir_bias, bit_reservoir_bits,
 *                    bitrate_hard_min, bitrate_hard_max, lowpass_frequency,
 *                    impulse_noisetune, and disable_coupling (no value)
 *
 * Compiled from the pristine libvorbis 1.3.7 and libogg 1.3.5 sources in
 * tools/libvorbis-1.3.7 and tools/libogg-1.3.5 by build.sh with
 *     -O0 -ffp-contract=off
 * so its byte output is fully deterministic and matches oxivorbis's no-FMA
 * arithmetic. It is the parity oracle for tests/parity.rs and the fuzzer.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <unistd.h>

#include "vorbis/codec.h"
#include "vorbis/vorbisenc.h"

/* Use 64-sample analysis chunk to match ffmpeg/oxivorbis's pre-extrapolation
 * trigger point (n_lpc=2112). Different chunk sizes give different
 * libvorbis _preextrapolate_helper firing positions and therefore different
 * LPC-predicted virtual samples in the first short block. */
#define READ_CHUNK 64

static void die(const char *msg) {
    fprintf(stderr, "oracle-encoder: %s\n", msg);
    exit(1);
}

#define MAX_ADV 16

/* oggenc's oe_enc_opt fields that drive the libvorbis setup. */
struct bitrate_opts {
    int any;          /* a bitrate option was given */
    int bitrate;      /* -b, kbps; -1 = unset */
    int min_bitrate;  /* -m */
    int max_bitrate;  /* -M */
    int managed;
    int quality_set;
    int adv_count;
    char *adv_arg[MAX_ADV];
    char *adv_val[MAX_ADV];
};

static int parse_kbps(const char *s) {
    char *end = NULL;
    long v = strtol(s, &end, 10);
    if (end == s || *end != '\0' || v < -1 || v > 2147483) die("invalid kbps");
    return (int)v;
}

/* oggenc/encode.c set_advanced_encoder_options. */
static void set_advanced_encoder_options(const struct bitrate_opts *o, vorbis_info *vi) {
    int manage = 0, i;
    struct ovectl_ratemanage2_arg ai;
    vorbis_encode_ctl(vi, OV_ECTL_RATEMANAGE2_GET, &ai);
    for (i = 0; i < o->adv_count; i++) {
        const char *a = o->adv_arg[i], *v = o->adv_val[i];
        if (!strcmp(a, "bitrate_average_damping")) {
            ai.bitrate_average_damping = strtod(v, NULL);
            manage = 1;
        } else if (!strcmp(a, "bitrate_average")) {
            ai.bitrate_average_kbps = strtol(v, NULL, 10);
            manage = 1;
        } else if (!strcmp(a, "bit_reservoir_bias")) {
            ai.bitrate_limit_reservoir_bias = strtod(v, NULL);
            manage = 1;
        } else if (!strcmp(a, "bit_reservoir_bits")) {
            ai.bitrate_limit_reservoir_bits = strtol(v, NULL, 10);
            manage = 1;
        } else if (!strcmp(a, "bitrate_hard_min")) {
            ai.bitrate_limit_min_kbps = strtol(v, NULL, 10);
            manage = 1;
        } else if (!strcmp(a, "bitrate_hard_max")) {
            ai.bitrate_limit_max_kbps = strtol(v, NULL, 10);
            manage = 1;
        } else if (!strcmp(a, "disable_coupling")) {
            int val = 0;
            vorbis_encode_ctl(vi, OV_ECTL_COUPLING_SET, &val);
        } else if (!strcmp(a, "impulse_noisetune")) {
            double val = strtod(v, NULL);
            vorbis_encode_ctl(vi, OV_ECTL_IBLOCK_SET, &val);
        } else if (!strcmp(a, "lowpass_frequency")) {
            double prev, val = strtod(v, NULL);
            vorbis_encode_ctl(vi, OV_ECTL_LOWPASS_GET, &prev);
            vorbis_encode_ctl(vi, OV_ECTL_LOWPASS_SET, &val);
        } else {
            die("unknown advanced option");
        }
    }
    if (manage && vorbis_encode_ctl(vi, OV_ECTL_RATEMANAGE2_SET, &ai))
        die("advanced rate management parameters rejected");
}

/* oggenc/encode.c oe_encode, from "if(!opt->managed && ..." to
 * vorbis_encode_setup_init, with oggenc's error exits turned into die().
 * One deliberate difference: in quality mode with limits, oggenc first
 * passes the quality's nominal bitrate as the average, so libvorbis
 * refuses limits that don't bracket it (oggenc -q 6 -M 128 fails), and
 * then replaces it by -1. This passes -1 right away, which leaves the
 * same setup whenever oggenc succeeds, and also allows any limits. */
static void setup_like_oggenc(struct bitrate_opts *opt, vorbis_info *vi, int channels, long rate,
                              float quality) {
    if (!opt->managed && (opt->min_bitrate >= 0 || opt->max_bitrate >= 0))
        die("Requesting a minimum or maximum bitrate requires --managed");
    if (opt->bitrate < 0 && opt->min_bitrate < 0 && opt->max_bitrate < 0) opt->quality_set = 1;

    if (opt->quality_set > 0) {
        if (vorbis_encode_setup_vbr(vi, channels, rate, quality))
            die("Mode initialisation failed: invalid parameters for quality");
        if (opt->max_bitrate > 0 || opt->min_bitrate > 0) {
            long bitrate;
            struct ovectl_ratemanage2_arg ai;
            vorbis_encode_ctl(vi, OV_ECTL_RATEMANAGE2_GET, &ai);
            {
                vorbis_info vi2;
                vorbis_info_init(&vi2);
                vorbis_encode_setup_vbr(&vi2, channels, rate, quality);
                vorbis_encode_setup_init(&vi2);
                bitrate = vi2.bitrate_nominal;
                vorbis_info_clear(&vi2);
            }
            ai.bitrate_average_kbps = -1; /* oggenc: bitrate/1000 (see above) */
            ai.bitrate_average_damping = 1.5;
            ai.bitrate_limit_reservoir_bits = bitrate * 2;
            ai.bitrate_limit_reservoir_bias = .1;
            ai.bitrate_limit_min_kbps = opt->min_bitrate;
            ai.bitrate_limit_max_kbps = opt->max_bitrate;
            ai.management_active = 1;
            if (vorbis_encode_ctl(vi, OV_ECTL_RATEMANAGE2_SET, &ai) != 0)
                die("Failed to set bitrate min/max in quality mode");
        }
    } else {
        if (vorbis_encode_setup_managed(vi, channels, rate,
                                        opt->max_bitrate > 0 ? opt->max_bitrate * 1000 : -1,
                                        opt->bitrate * 1000,
                                        opt->min_bitrate > 0 ? opt->min_bitrate * 1000 : -1))
            die("Mode initialisation failed: invalid parameters for bitrate");
    }

    if (opt->managed && opt->bitrate < 0) {
        struct ovectl_ratemanage2_arg ai;
        vorbis_encode_ctl(vi, OV_ECTL_RATEMANAGE2_GET, &ai);
        ai.bitrate_average_kbps = -1;
        vorbis_encode_ctl(vi, OV_ECTL_RATEMANAGE2_SET, &ai);
    } else if (!opt->managed) {
        /* Turn off management entirely (if it was turned on). */
        vorbis_encode_ctl(vi, OV_ECTL_RATEMANAGE2_SET, NULL);
    }

    set_advanced_encoder_options(opt, vi);

    if (vorbis_encode_setup_init(vi)) die("vorbis_encode_setup_init failed");
}

int main(int argc, char **argv) {
    struct bitrate_opts bopt = {0, -1, -1, -1, 0, 0, 0, {0}, {0}};
    while (argc > 1 && argv[1][0] == '-' && argv[1][1] != '\0' &&
           !(argv[1][1] >= '0' && argv[1][1] <= '9')) {
        const char *o = argv[1];
        int take = 1;
        bopt.any = 1;
        if (!strcmp(o, "--managed")) {
            bopt.managed = 1;
        } else if (!strcmp(o, "-q")) {
            bopt.quality_set = 1;
        } else if (argc > 2 && !strcmp(o, "-b")) {
            bopt.bitrate = parse_kbps(argv[2]);
            take = 2;
        } else if (argc > 2 && !strcmp(o, "-m")) {
            bopt.min_bitrate = parse_kbps(argv[2]);
            bopt.managed = 1;
            take = 2;
        } else if (argc > 2 && !strcmp(o, "-M")) {
            bopt.max_bitrate = parse_kbps(argv[2]);
            bopt.managed = 1;
            take = 2;
        } else if (argc > 2 && !strcmp(o, "-A")) {
            char *eq = strchr(argv[2], '=');
            if (bopt.adv_count == MAX_ADV) die("too many -A options");
            if (eq) *eq = '\0';
            bopt.adv_arg[bopt.adv_count] = argv[2];
            bopt.adv_val[bopt.adv_count] = eq ? eq + 1 : "";
            bopt.adv_count++;
            take = 2;
        } else {
            die("unknown option");
        }
        argc -= take;
        argv += take;
    }
    if (argc < 3 || argc > 6)
        die("usage: oracle-encoder [-b K] [-m K] [-M K] [--managed] [-q] [-A KEY=VAL]... "
            "<rate> <channels> [base_quality [s16|f32 [comment]]]");
    long rate = atol(argv[1]);
    int channels = atoi(argv[2]);
    /* libvorbis itself rejects more than 255 channels (OV_EINVAL). */
    if (rate <= 0 || channels < 1) die("invalid rate/channels");
    /* Optional libvorbis base_quality (-0.1..1.0 == q-1..q10); default q5. */
    float quality = 0.5f;
    if (argc >= 4) {
        char *end = NULL;
        quality = strtof(argv[3], &end);
        if (end == argv[3] || *end != '\0' || quality < -0.1f || quality > 1.0f)
            die("invalid base_quality (expected -0.1..1.0)");
    }

    /* Optional input format; default s16 keeps the original usage. */
    int is_f32 = 0;
    if (argc >= 5) {
        if (strcmp(argv[4], "f32") == 0) is_f32 = 1;
        else if (strcmp(argv[4], "s16") != 0) die("invalid format (expected s16 or f32)");
    }
    size_t sample_size = is_f32 ? sizeof(float) : sizeof(int16_t);

    /* Read all PCM bytes from stdin. */
    size_t cap = 1 << 17;
    unsigned char *raw = malloc(cap);
    size_t nbytes = 0;
    for (;;) {
        if (nbytes + 4096 > cap) {
            cap *= 2;
            raw = realloc(raw, cap);
        }
        ssize_t got = read(0, raw + nbytes, 4096);
        if (got < 0) die("read failed");
        if (got == 0) break;
        nbytes += (size_t)got;
    }
    size_t total = nbytes / sample_size;
    long frames = (long)(total / channels);
    /* Samples as libvorbis sees them (host is little-endian). */
    float *samples = malloc((total ? total : 1) * sizeof(float));
    for (size_t i = 0; i < total; i++) {
        if (is_f32) {
            float f;
            memcpy(&f, raw + i * sizeof(float), sizeof(float));
            samples[i] = f;
        } else {
            int16_t v;
            memcpy(&v, raw + i * sizeof(int16_t), sizeof(int16_t));
            samples[i] = v / 32768.0f;
        }
    }
    free(raw);

    /* libvorbis init at the requested quality (default q5). */
    vorbis_info vi;
    vorbis_info_init(&vi);
    if (bopt.any)
        setup_like_oggenc(&bopt, &vi, channels, rate, quality);
    else if (vorbis_encode_init_vbr(&vi, channels, rate, quality) != 0)
        die("vorbis_encode_init_vbr failed");

    vorbis_comment vc;
    vorbis_comment_init(&vc);
    if (argc == 6) vorbis_comment_add(&vc, argv[5]);

    vorbis_dsp_state vd;
    vorbis_block vb;
    vorbis_analysis_init(&vd, &vi);
    vorbis_block_init(&vd, &vb);

    /* Write headers. */
    ogg_stream_state os;
    /* Stable serial for repeatability. The Rust parity test extracts
     * bytes 14..18 of our output and uses that serial in encode_with_serial. */
    ogg_stream_init(&os, 0xC0FFEE42);

    ogg_packet hdr, hdr_comm, hdr_code;
    vorbis_analysis_headerout(&vd, &vc, &hdr, &hdr_comm, &hdr_code);
    ogg_stream_packetin(&os, &hdr);
    ogg_stream_packetin(&os, &hdr_comm);
    ogg_stream_packetin(&os, &hdr_code);

    ogg_page og;
    while (ogg_stream_flush(&os, &og)) {
        fwrite(og.header, 1, og.header_len, stdout);
        fwrite(og.body, 1, og.body_len, stdout);
    }

    /* Feed PCM. */
    long fed = 0;
    int eos = 0;
    while (!eos) {
        if (fed < frames) {
            long this_chunk = READ_CHUNK;
            if (fed + this_chunk > frames) this_chunk = frames - fed;
            float **buffer = vorbis_analysis_buffer(&vd, this_chunk);
            for (long i = 0; i < this_chunk; i++) {
                for (int c = 0; c < channels; c++) {
                    buffer[c][i] = samples[(fed + i) * channels + c];
                }
            }
            vorbis_analysis_wrote(&vd, this_chunk);
            fed += this_chunk;
        } else {
            vorbis_analysis_wrote(&vd, 0);
        }

        ogg_packet op;
        while (vorbis_analysis_blockout(&vd, &vb) == 1) {
            vorbis_analysis(&vb, NULL);
            vorbis_bitrate_addblock(&vb);
            while (vorbis_bitrate_flushpacket(&vd, &op)) {
                ogg_stream_packetin(&os, &op);
                while (ogg_stream_pageout(&os, &og)) {
                    fwrite(og.header, 1, og.header_len, stdout);
                    fwrite(og.body, 1, og.body_len, stdout);
                    if (ogg_page_eos(&og)) eos = 1;
                }
            }
        }
    }
    while (ogg_stream_flush(&os, &og)) {
        fwrite(og.header, 1, og.header_len, stdout);
        fwrite(og.body, 1, og.body_len, stdout);
    }

    ogg_stream_clear(&os);
    vorbis_block_clear(&vb);
    vorbis_dsp_clear(&vd);
    vorbis_comment_clear(&vc);
    vorbis_info_clear(&vi);
    free(samples);
    return 0;
}
