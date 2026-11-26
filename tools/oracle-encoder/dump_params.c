/*
 * dump_params — prints every quality-dependent parameter that
 * vorbis_encode_init_vbr() derives, for one (rate, channels) and a list of
 * base_quality values, in a stable line-oriented text format.
 *
 * Used by the oracle-gated test in src/quality.rs, which renders its own
 * QualityConfig in the identical format and diffs line-by-line.
 *
 * Usage: dump_params <rate> <channels> <q_bits_hex>...
 *   each q is the IEEE-754 bit pattern of the f32 base_quality, in hex
 *   (e.g. 3f000000 for 0.5), so no decimal round-trip is involved.
 *
 * Format (all floats/doubles as raw hex bits, f32 = 8 digits, f64 = 16):
 *   q <bits>
 *   base_setting <f64>
 *   ...
 *   end
 * Lines mirror QualityConfig::oracle_text() in src/quality.rs.
 *
 * Linked against the vendored libvorbis with the same flags as the oracle
 * encoder; see build_quality_dumpers.sh. libvorbis' vendored debug prints go
 * to stderr and are ignored.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>

#include "vorbis/codec.h"
#include "vorbis/vorbisenc.h"
#include "codec_internal.h"

static uint32_t fb(float f) {
    uint32_t u;
    memcpy(&u, &f, 4);
    return u;
}
static unsigned long long db(double d) {
    unsigned long long u;
    memcpy(&u, &d, 8);
    return u;
}

static void farr(const char *name, const float *a, int n) {
    int i;
    printf("%s", name);
    for (i = 0; i < n; i++) printf(" %08x", fb(a[i]));
    printf("\n");
}
static void iarr(const char *name, const int *a, int n) {
    int i;
    printf("%s", name);
    for (i = 0; i < n; i++) printf(" %d", a[i]);
    printf("\n");
}

static void dump_one(long rate, int ch, float q, uint32_t qbits) {
    vorbis_info vi;
    codec_setup_info *ci;
    int b, j;
    char name[64];

    printf("q %08x\n", qbits);
    vorbis_info_init(&vi);
    if (vorbis_encode_init_vbr(&vi, ch, rate, q) != 0) {
        printf("error\nend\n");
        return;
    }
    ci = vi.codec_setup;

    printf("base_setting %016llx\n", db(ci->hi.base_setting));
    printf("lowpass_khz %016llx\n", db(ci->hi.lowpass_kHz));
    printf("blocksizes %ld %ld\n", ci->blocksizes[0], ci->blocksizes[1]);
    printf("bitrate %ld %ld %ld\n", vi.bitrate_upper, vi.bitrate_nominal, vi.bitrate_lower);
    printf("psys %d\n", ci->psys);

    {
        vorbis_info_psy_global *g = &ci->psy_g_param;
        printf("g.eighth_octave_lines %d\n", g->eighth_octave_lines);
        farr("g.preecho_thresh", g->preecho_thresh, VE_BANDS);
        farr("g.postecho_thresh", g->postecho_thresh, VE_BANDS);
        printf("g.stretch_penalty %08x\n", fb(g->stretch_penalty));
        printf("g.preecho_minenergy %08x\n", fb(g->preecho_minenergy));
        printf("g.ampmax_att_per_sec %08x\n", fb(g->ampmax_att_per_sec));
        iarr("g.coupling_pkHz", g->coupling_pkHz, PACKETBLOBS);
        iarr("g.coupling_pointlimit0", g->coupling_pointlimit[0], PACKETBLOBS);
        iarr("g.coupling_pointlimit1", g->coupling_pointlimit[1], PACKETBLOBS);
        iarr("g.coupling_prepointamp", g->coupling_prepointamp, PACKETBLOBS);
        iarr("g.coupling_postpointamp", g->coupling_postpointamp, PACKETBLOBS);
        iarr("g.sliding_lowpass0", g->sliding_lowpass[0], PACKETBLOBS);
        iarr("g.sliding_lowpass1", g->sliding_lowpass[1], PACKETBLOBS);
    }

    for (b = 0; b < 4; b++) {
        vorbis_info_psy *p = ci->psy_param[b];
        if (!p) {
            printf("p%d missing\n", b);
            continue;
        }
#define F(field) printf("p%d." #field " %08x\n", b, fb(p->field))
#define I(field) printf("p%d." #field " %d\n", b, p->field)
        I(blockflag);
        F(ath_adjatt);
        F(ath_maxatt);
        snprintf(name, sizeof name, "p%d.tone_masteratt", b);
        farr(name, p->tone_masteratt, P_NOISECURVES);
        F(tone_centerboost);
        F(tone_decay);
        F(tone_abs_limit);
        snprintf(name, sizeof name, "p%d.toneatt", b);
        farr(name, p->toneatt, P_BANDS);
        I(noisemaskp);
        F(noisemaxsupp);
        F(noisewindowlo);
        F(noisewindowhi);
        I(noisewindowlomin);
        I(noisewindowhimin);
        I(noisewindowfixed);
        for (j = 0; j < P_NOISECURVES; j++) {
            snprintf(name, sizeof name, "p%d.noiseoff%d", b, j);
            farr(name, p->noiseoff[j], P_BANDS);
        }
        snprintf(name, sizeof name, "p%d.noisecompand", b);
        farr(name, p->noisecompand, NOISE_COMPAND_LEVELS);
        F(max_curve_dB);
        I(normal_p);
        I(normal_start);
        I(normal_partition);
        printf("p%d.normal_thresh %016llx\n", b, db(p->normal_thresh));
#undef F
#undef I
    }
    printf("end\n");
    vorbis_info_clear(&vi);
}

int main(int argc, char **argv) {
    int i;
    long rate;
    int ch;
    if (argc < 4) {
        fprintf(stderr, "usage: dump_params <rate> <channels> <q_bits_hex>...\n");
        return 2;
    }
    rate = atol(argv[1]);
    ch = atoi(argv[2]);
    for (i = 3; i < argc; i++) {
        uint32_t bits = (uint32_t)strtoul(argv[i], NULL, 16);
        float q;
        memcpy(&q, &bits, 4);
        dump_one(rate, ch, q, bits);
    }
    fflush(stdout);
    return 0;
}
