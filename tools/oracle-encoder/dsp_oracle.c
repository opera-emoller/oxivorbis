/*
 * dsp_oracle — ground truth for oxivorbis's DSP layer (MDCT, real FFT,
 * window tables / window application), straight from the pristine
 * vendored libvorbis 1.3.7 sources (tools/libvorbis-1.3.7).
 *
 * Two modes:
 *
 *   dsp_oracle tables <dir>
 *       Writes the raw lookup tables libvorbis builds at runtime, as
 *       little-endian binaries, for tools/gen-tables to embed:
 *         mdct_<n>_trig.bin   (n + n/4 floats)  mdct_init(n)
 *         mdct_<n>_bitrev.bin (n/4 ints)
 *         mdct_<n>_scale.bin  (1 float)
 *         drft_<n>_trig.bin   (3n floats)       drft_init(n) trigcache
 *         drft_<n>_split.bin  (32 ints)         drft_init(n) splitcache
 *         vwin_<bs>.bin       (bs/2 floats)     vwin for blocksize bs
 *
 *   dsp_oracle manifest
 *       Prints (stdout) one line per case: `<name> <count> <fnv1a64-hex>`.
 *       The hash is FNV-1a 64 over the little-endian f32/i32 bytes. Covers
 *       every table above plus mdct_forward / drft_forward /
 *       _vorbis_apply_window outputs on seeded LCG input. The committed
 *       copy lives at tools/oracle-encoder/dsp_manifest.txt and is checked
 *       by oxivorbis's always-on unit tests (`dsp_manifest_*` in src/mdct.rs,
 *       src/drft.rs, src/window.rs).
 *
 * The LCG and float mapping are reproduced exactly on the Rust side
 * (`lcg_input` in the test helpers): state = state*1664525 + 1013904223
 * (mod 2^32), sample = ((state >> 8) / 2^23 - 1) * 0.5 — every step exact
 * in f32.
 *
 * Build: ./tools/oracle-encoder/build_dsp_oracle.sh  (-O0 -ffp-contract=off)
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "mdct.h"
#include "smallft.h"
#include "window.h"

/* ---------- hashing / io ---------- */

static uint64_t fnv(const void *p, size_t bytes) {
    const unsigned char *b = p;
    uint64_t h = 0xcbf29ce484222325ULL;
    for (size_t i = 0; i < bytes; i++) {
        h ^= b[i];
        h *= 0x100000001b3ULL;
    }
    return h;
}

static void emit(const char *name, const void *p, size_t count, size_t elem) {
    printf("%s %zu %016llx\n", name, count, (unsigned long long)fnv(p, count * elem));
}

static void write_bin(const char *dir, const char *name, const void *p, size_t bytes) {
    char path[1024];
    snprintf(path, sizeof(path), "%s/%s", dir, name);
    FILE *f = fopen(path, "wb");
    if (!f) {
        fprintf(stderr, "cannot open %s\n", path);
        exit(1);
    }
    fwrite(p, 1, bytes, f);
    fclose(f);
}

/* ---------- deterministic input ---------- */

static void lcg_fill(float *d, int n, uint32_t seed) {
    uint32_t s = seed;
    for (int i = 0; i < n; i++) {
        s = s * 1664525u + 1013904223u;
        d[i] = ((float)(s >> 8) / 8388608.f - 1.f) * .5f;
    }
}

static int ilog(unsigned v) {
    int r = 0;
    while (v) {
        r++;
        v >>= 1;
    }
    return r;
}

/* vwin index libvorbis uses for blocksize bs (block.c: ov_ilog(bs)-7). */
static int winno_for(long bs) { return ilog((unsigned)bs) - 7; }

static const int MDCT_SIZES[] = {128, 256, 512, 1024, 2048, 4096};
static const int DRFT_SIZES[] = {256, 512, 1024, 2048, 4096};
static const int WIN_SIZES[] = {256, 512, 1024, 2048, 4096};
#define COUNT(a) ((int)(sizeof(a) / sizeof((a)[0])))

/* ---------- modes ---------- */

static void do_tables(const char *dir) {
    char name[128];
    for (int k = 0; k < COUNT(MDCT_SIZES); k++) {
        int n = MDCT_SIZES[k];
        mdct_lookup m;
        mdct_init(&m, n);
        snprintf(name, sizeof(name), "mdct_%d_trig.bin", n);
        write_bin(dir, name, m.trig, sizeof(float) * (n + n / 4));
        snprintf(name, sizeof(name), "mdct_%d_bitrev.bin", n);
        write_bin(dir, name, m.bitrev, sizeof(int) * (n / 4));
        snprintf(name, sizeof(name), "mdct_%d_scale.bin", n);
        write_bin(dir, name, &m.scale, sizeof(float));
        mdct_clear(&m);
    }
    for (int k = 0; k < COUNT(DRFT_SIZES); k++) {
        int n = DRFT_SIZES[k];
        drft_lookup l;
        drft_init(&l, n);
        snprintf(name, sizeof(name), "drft_%d_trig.bin", n);
        write_bin(dir, name, l.trigcache, sizeof(float) * 3 * n);
        snprintf(name, sizeof(name), "drft_%d_split.bin", n);
        write_bin(dir, name, l.splitcache, sizeof(int) * 32);
        drft_clear(&l);
    }
    for (int k = 0; k < COUNT(WIN_SIZES); k++) {
        int bs = WIN_SIZES[k];
        snprintf(name, sizeof(name), "vwin_%d.bin", bs);
        write_bin(dir, name, _vorbis_window_get(winno_for(bs)), sizeof(float) * (bs / 2));
    }
    fprintf(stderr, "dsp_oracle: wrote tables to %s\n", dir);
}

static void do_manifest(void) {
    char name[128];

    /* tables */
    for (int k = 0; k < COUNT(MDCT_SIZES); k++) {
        int n = MDCT_SIZES[k];
        mdct_lookup m;
        mdct_init(&m, n);
        snprintf(name, sizeof(name), "table.mdct_trig.%d", n);
        emit(name, m.trig, n + n / 4, sizeof(float));
        snprintf(name, sizeof(name), "table.mdct_bitrev.%d", n);
        emit(name, m.bitrev, n / 4, sizeof(int));
        snprintf(name, sizeof(name), "table.mdct_scale.%d", n);
        emit(name, &m.scale, 1, sizeof(float));
        mdct_clear(&m);
    }
    for (int k = 0; k < COUNT(DRFT_SIZES); k++) {
        int n = DRFT_SIZES[k];
        drft_lookup l;
        drft_init(&l, n);
        /* drft_forward passes trigcache+n as the twiddle table (wa). */
        snprintf(name, sizeof(name), "table.drft_wa.%d", n);
        emit(name, l.trigcache + n, 2 * n, sizeof(float));
        snprintf(name, sizeof(name), "table.drft_split.%d", n);
        emit(name, l.splitcache, 32, sizeof(int));
        drft_clear(&l);
    }
    for (int k = 0; k < COUNT(WIN_SIZES); k++) {
        int bs = WIN_SIZES[k];
        snprintf(name, sizeof(name), "table.vwin.%d", bs);
        emit(name, _vorbis_window_get(winno_for(bs)), bs / 2, sizeof(float));
    }

    /* mdct_forward on seeded input */
    for (int k = 0; k < COUNT(MDCT_SIZES); k++) {
        int n = MDCT_SIZES[k];
        mdct_lookup m;
        mdct_init(&m, n);
        float *in = malloc(sizeof(float) * n);
        float *out = malloc(sizeof(float) * n / 2);
        for (uint32_t seed = 1; seed <= 3; seed++) {
            lcg_fill(in, n, seed * 7919u + (uint32_t)n);
            mdct_forward(&m, in, out);
            snprintf(name, sizeof(name), "mdct.%d.seed%u", n, seed);
            emit(name, out, n / 2, sizeof(float));
        }
        free(in);
        free(out);
        mdct_clear(&m);
    }

    /* drft_forward on seeded input */
    for (int k = 0; k < COUNT(DRFT_SIZES); k++) {
        int n = DRFT_SIZES[k];
        drft_lookup l;
        drft_init(&l, n);
        float *d = malloc(sizeof(float) * n);
        for (uint32_t seed = 1; seed <= 3; seed++) {
            lcg_fill(d, n, seed * 104729u + (uint32_t)n);
            drft_forward(&l, d);
            snprintf(name, sizeof(name), "drft.%d.seed%u", n, seed);
            emit(name, d, n, sizeof(float));
        }
        free(d);
        drft_clear(&l);
    }

    /* _vorbis_apply_window for every blocksize pair a template uses, every
     * lW/W/nW combo (equal pairs: the single-block 8/11/16 kHz modes) */
    static const long PAIRS[][2] = {{256, 2048}, {512, 4096}, {512, 1024}, {512, 512}, {1024, 1024}};
    for (int p = 0; p < (int)(sizeof(PAIRS) / sizeof(PAIRS[0])); p++) {
        long bs[2] = {PAIRS[p][0], PAIRS[p][1]};
        int winno[2] = {winno_for(bs[0]), winno_for(bs[1])};
        float *d = malloc(sizeof(float) * bs[1]);
        for (int W = 0; W < 2; W++)
            for (int lW = 0; lW < 2; lW++)
                for (int nW = 0; nW < 2; nW++) {
                    long n = bs[W];
                    lcg_fill(d, (int)n, 31337u + (uint32_t)(W * 4 + lW * 2 + nW));
                    _vorbis_apply_window(d, winno, bs, lW, W, nW);
                    snprintf(name, sizeof(name), "window.%ld_%ld.lW%d_W%d_nW%d", bs[0], bs[1],
                             lW, W, nW);
                    emit(name, d, n, sizeof(float));
                }
        free(d);
    }
}

int main(int argc, char **argv) {
    if (argc == 3 && strcmp(argv[1], "tables") == 0) {
        do_tables(argv[2]);
        return 0;
    }
    if (argc == 2 && strcmp(argv[1], "manifest") == 0) {
        do_manifest();
        return 0;
    }
    fprintf(stderr, "usage: %s tables <dir> | %s manifest\n", argv[0], argv[0]);
    return 2;
}
