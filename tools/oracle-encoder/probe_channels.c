/*
 * probe_channels — which (channels, rate, base_quality) requests does
 * libvorbis 1.3.7's vorbis_encode_init_vbr accept, and which mode template
 * does it pick for each?
 *
 * Usage:
 *   probe_channels grid
 *       channels 0..=300 x edge rates x a quality grid; prints one line
 *       per run of identical outcomes and a summary.
 *   probe_channels rates <channels> <q>...
 *       every rate 1..=200001 for one channel count; prints the template
 *       ranges.
 *   probe_channels one <channels> <rate> <q>
 *       one request: "ok <template>" or "err <code>".
 *
 * #includes vorbisenc.c (like dump_quality_tables) so the template a
 * request lands on can be named by pointer identity against setup_list.
 * Built by build_quality_dumpers.sh with the oracle's pinned flags.
 */
#include <stdio.h>

#include "vorbisenc.c"

#include <stdlib.h>
#include <string.h>

static const char *template_name(const void *p) {
    static const struct {
        const void *p;
        const char *name;
    } names[] = {
        {&ve_setup_44_stereo, "44_stereo"},     {&ve_setup_44_51, "44_51"},
        {&ve_setup_44_uncoupled, "44_uncoupled"}, {&ve_setup_32_stereo, "32_stereo"},
        {&ve_setup_32_uncoupled, "32_uncoupled"}, {&ve_setup_22_stereo, "22_stereo"},
        {&ve_setup_22_uncoupled, "22_uncoupled"}, {&ve_setup_16_stereo, "16_stereo"},
        {&ve_setup_16_uncoupled, "16_uncoupled"}, {&ve_setup_11_stereo, "11_stereo"},
        {&ve_setup_11_uncoupled, "11_uncoupled"}, {&ve_setup_8_stereo, "8_stereo"},
        {&ve_setup_8_uncoupled, "8_uncoupled"},   {&ve_setup_X_stereo, "X_stereo"},
        {&ve_setup_X_uncoupled, "X_uncoupled"},   {&ve_setup_XX_stereo, "XX_stereo"},
        {&ve_setup_XX_uncoupled, "XX_uncoupled"},
    };
    size_t i;
    for (i = 0; i < sizeof names / sizeof names[0]; i++)
        if (names[i].p == p) return names[i].name;
    return "?";
}

/* "ok <template>" or "err <code>" for one request. */
static void probe(int ch, long rate, float q, char *out, size_t len) {
    vorbis_info vi;
    int ret;
    vorbis_info_init(&vi);
    ret = vorbis_encode_init_vbr(&vi, ch, rate, q);
    if (ret == 0) {
        codec_setup_info *ci = vi.codec_setup;
        snprintf(out, len, "ok %s", template_name(ci->hi.setup));
        vorbis_info_clear(&vi);
    } else {
        /* vorbis_encode_init_vbr already cleared vi on failure. */
        snprintf(out, len, "err %d", ret);
    }
}

static const long grid_rates[] = {
    1,     7999,  8000,  8999,  9000,   14999,  15000,  18999,  19000,  25999,  26000,
    32000, 39999, 40000, 44100, 48000,  50000,  50001,  64000,  69999,  70000,  70001,
    88200, 96000, 192000, 200000, 200001,
};
static const float grid_q[] = {-0.1f, -0.05f, 0.0f, 0.1f, 0.3f, 0.5f, 0.7f, 0.9f, 1.0f};

int main(int argc, char **argv) {
    char cur[64], prev[64];
    if (argc >= 2 && strcmp(argv[1], "grid") == 0) {
        size_t r, k;
        int ch;
        long accepted = 0, rejected = 0;
        for (r = 0; r < sizeof grid_rates / sizeof grid_rates[0]; r++) {
            for (k = 0; k < sizeof grid_q / sizeof grid_q[0]; k++) {
                int run_start = 0;
                prev[0] = 0;
                for (ch = 0; ch <= 301; ch++) {
                    if (ch <= 300) {
                        probe(ch, grid_rates[r], grid_q[k], cur, sizeof cur);
                        if (cur[0] == 'o') accepted++;
                        else rejected++;
                    }
                    if (ch == 301 || (ch > 0 && strcmp(cur, prev) != 0)) {
                        if (ch > 0)
                            printf("rate %ld q %g ch %d..=%d: %s\n", grid_rates[r],
                                   (double)grid_q[k], run_start, ch - 1, prev);
                        run_start = ch;
                    }
                    strcpy(prev, cur);
                }
            }
        }
        printf("accepted %ld rejected %ld\n", accepted, rejected);
        return 0;
    }
    if (argc >= 4 && strcmp(argv[1], "rates") == 0) {
        int ch = atoi(argv[2]), a;
        for (a = 3; a < argc; a++) {
            float q = strtof(argv[a], NULL);
            long rate, run_start = 1;
            prev[0] = 0;
            for (rate = 1; rate <= 200002; rate++) {
                if (rate <= 200001) probe(ch, rate, q, cur, sizeof cur);
                if (rate == 200002 || (rate > 1 && strcmp(cur, prev) != 0)) {
                    printf("ch %d q %g rate %ld..=%ld: %s\n", ch, (double)q, run_start, rate - 1,
                           prev);
                    run_start = rate;
                }
                strcpy(prev, cur);
            }
        }
        return 0;
    }
    if (argc == 5 && strcmp(argv[1], "one") == 0) {
        probe(atoi(argv[2]), atol(argv[3]), strtof(argv[4], NULL), cur, sizeof cur);
        printf("%s\n", cur);
        return 0;
    }
    fprintf(stderr, "usage: probe_channels grid | rates <ch> <q>... | one <ch> <rate> <q>\n");
    return 2;
}
