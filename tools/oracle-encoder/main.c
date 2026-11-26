/*
 * oracle-encoder — own-built libvorbis Q5 encoder, pinned compile flags.
 *
 * Reads interleaved PCM from stdin, writes Ogg Vorbis to stdout.
 * Args: <rate> <channels> [base_quality [format]]   e.g. ./oracle-encoder 44100 1
 *       base_quality: libvorbis -0.1..1.0 scale (q-1..q10), default 0.5.
 *       format: s16 (default; i16 LE, fed as sample/32768.0f) or f32
 *               (f32 LE, fed to vorbis_analysis_buffer unchanged).
 *
 * Static-linked against ~/Documents/src/libvorbis/lib (all .c files) with:
 *     -O0 -ffp-contract=off -std=c99
 * so its byte output is fully deterministic and matches lewtoff's no-FMA
 * arithmetic. Use this as the parity oracle in tests/parity.rs instead of
 * system ffmpeg.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <unistd.h>

#include "vorbis/codec.h"
#include "vorbis/vorbisenc.h"

/* Use 64-sample analysis chunk to match ffmpeg/lewtoff's pre-extrapolation
 * trigger point (n_lpc=2112). Different chunk sizes give different
 * libvorbis _preextrapolate_helper firing positions and therefore different
 * LPC-predicted virtual samples in the first short block. */
#define READ_CHUNK 64

static void die(const char *msg) {
    fprintf(stderr, "oracle-encoder: %s\n", msg);
    exit(1);
}

int main(int argc, char **argv) {
    if (argc < 3 || argc > 5)
        die("usage: oracle-encoder <rate> <channels> [base_quality [s16|f32]]");
    long rate = atol(argv[1]);
    int channels = atoi(argv[2]);
    if (rate <= 0 || channels < 1 || channels > 2) die("invalid rate/channels");
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
    if (argc == 5) {
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
    if (vorbis_encode_init_vbr(&vi, channels, rate, quality) != 0)
        die("vorbis_encode_init_vbr failed");

    vorbis_comment vc;
    vorbis_comment_init(&vc);

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
