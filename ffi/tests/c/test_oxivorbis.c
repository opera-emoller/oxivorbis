/*
 * C test of the oxivorbis C API. Built and run by ffi/tests/c_api.rs:
 *
 *     test_oxivorbis <out-dir>
 *
 * Encodes the same deterministic input through every encoding function and
 * writes each stream to <out-dir>/<name>.ogg; the Rust side compares every
 * file byte for byte with oxivorbis::encode / encode_f32 on the same input
 * and options. Also exercises the error paths (bad rate, quality, channel
 * count, NULL pointers, calls after finish, a failing write callback).
 * Exit status 0 iff every check passed. Built with -fsanitize=address and
 * run under leaks/valgrind in CI, so every allocation must be released.
 *
 * Keep the generators and options here in sync with c_api.rs.
 */
/* MSVC CRT: fopen is standard C99, not "deprecated". */
#ifdef _WIN32
#define _CRT_SECURE_NO_WARNINGS
#endif

#include "oxivorbis.h"

#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int failures = 0;

#define CHECK(cond)                                                        \
    do {                                                                   \
        if (!(cond)) {                                                     \
            fprintf(stderr, "%s:%d: CHECK failed: %s\n", __FILE__, __LINE__, \
                    #cond);                                                \
            failures++;                                                    \
        }                                                                  \
    } while (0)

#define CHECK_STATUS(call, want)                                           \
    do {                                                                   \
        oxv_status got_ = (call);                                          \
        if (got_ != (want)) {                                              \
            fprintf(stderr, "%s:%d: %s returned %d (%s), want %d\n",       \
                    __FILE__, __LINE__, #call, (int)got_,                  \
                    oxv_status_message(got_), (int)(want));                \
            failures++;                                                    \
        }                                                                  \
    } while (0)

#define CHECK_OK(call) CHECK_STATUS(call, OXV_STATUS_OK)

/* ---- deterministic input (mirrored in c_api.rs) ------------------------ */

enum {
    STEREO_FRAMES = 44100 + 4321,
    MONO_FRAMES = 15000,
    EDGE_FRAMES = 3000,
    SURROUND_FRAMES = 30000
};

/* Interleaved i16: a square wave per channel plus LCG noise. */
static void gen_i16(int16_t *out, size_t frames, unsigned channels, uint32_t seed) {
    uint32_t state = seed;
    size_t i;
    unsigned c;
    for (i = 0; i < frames; i++) {
        for (c = 0; c < channels; c++) {
            int32_t square = ((i / (50 + 17 * c)) % 2) ? 6000 : -6000;
            int32_t noise;
            state = state * 1103515245u + 12345u;
            noise = (int32_t)((state >> 16) & 0x3fff) - 0x2000;
            out[i * channels + c] = (int16_t)(square + noise);
        }
    }
}

/* f32 input that isn't just i16/32768: scaled by 1.1 (so some samples
 * exceed 1.0), with the same float operations as the Rust side. */
static void to_f32(const int16_t *in, float *out, size_t n) {
    size_t i;
    for (i = 0; i < n; i++) {
        out[i] = (float)in[i] / 32768.0f * 1.1f;
    }
}

/* ---- helpers ------------------------------------------------------------ */

typedef struct {
    uint8_t *data;
    size_t len, cap;
} bytes;

static void bytes_append(bytes *b, const uint8_t *p, size_t n) {
    if (n == 0) {
        return;
    }
    if (b->len + n > b->cap) {
        size_t cap = b->cap ? b->cap : 4096;
        uint8_t *grown;
        while (cap < b->len + n) {
            cap *= 2;
        }
        grown = (uint8_t *)realloc(b->data, cap);
        if (!grown) {
            fprintf(stderr, "out of memory\n");
            exit(2);
        }
        b->data = grown;
        b->cap = cap;
    }
    memcpy(b->data + b->len, p, n);
    b->len += n;
}

static const char *out_dir;

static void save(const char *name, const uint8_t *data, size_t len) {
    char path[4096];
    FILE *f;
    snprintf(path, sizeof path, "%s/%s.ogg", out_dir, name);
    f = fopen(path, "wb");
    if (!f) {
        fprintf(stderr, "cannot create %s\n", path);
        failures++;
        return;
    }
    if (len && fwrite(data, 1, len, f) != len) {
        fprintf(stderr, "cannot write %s\n", path);
        failures++;
    }
    fclose(f);
}

/* Options "A": q-1, serial 42, custom vendor, comments incl. a key that
 * loses its '=' and one with an empty key (skipped). */
static oxv_options *options_a(void) {
    oxv_options *o = oxv_options_new();
    CHECK(o != NULL);
    CHECK_OK(oxv_options_set_quality(o, -1.0f));
    CHECK_OK(oxv_options_set_serial(o, 42));
    CHECK_OK(oxv_options_set_vendor(o, "c-test vendor"));
    CHECK_OK(oxv_options_add_comment(o, "TITLE", "Test=Title"));
    CHECK_OK(oxv_options_add_comment(o, "A=B", "v"));
    CHECK_OK(oxv_options_add_comment(o, "", "skipped"));
    return o;
}

/* Options "B": q7.5, serial 0xDEADBEEF, a UTF-8 comment value. */
static oxv_options *options_b(void) {
    oxv_options *o = oxv_options_new();
    CHECK(o != NULL);
    CHECK_OK(oxv_options_set_quality(o, 7.5f));
    CHECK_OK(oxv_options_set_serial(o, 0xDEADBEEFu));
    CHECK_OK(oxv_options_add_comment(o, "ARTIST", "\xc3\x9c" "n" "\xc3\xaf" "c" "\xc3\xb6" "d" "\xc3\xa9"));
    return o;
}

static int16_t stereo[STEREO_FRAMES * 2];
static float stereo_f[STEREO_FRAMES * 2];
static int16_t left[STEREO_FRAMES], right[STEREO_FRAMES];
static float left_f[STEREO_FRAMES], right_f[STEREO_FRAMES];
static int16_t mono[MONO_FRAMES];
static float mono_f[MONO_FRAMES];
/* 5.1 in Vorbis order: L, C, R, rear L, rear R, LFE. */
static int16_t surround[SURROUND_FRAMES * 6];
static float surround_f[SURROUND_FRAMES * 6];

static void deinterleave(void) {
    size_t i;
    for (i = 0; i < STEREO_FRAMES; i++) {
        left[i] = stereo[2 * i];
        right[i] = stereo[2 * i + 1];
        left_f[i] = stereo_f[2 * i];
        right_f[i] = stereo_f[2 * i + 1];
    }
}

/* ---- version / status ----------------------------------------------------- */

static void test_version(void) {
    const char *v = oxv_version();
    int i;
    CHECK(v != NULL && strlen(v) >= 5 && strchr(v, '.') != NULL);
    CHECK(oxv_abi_version() == OXV_ABI_VERSION);
    for (i = 0; i <= OXV_STATUS_PANIC; i++) {
        const char *m = oxv_status_message(i);
        CHECK(m != NULL && strcmp(m, "unknown status") != 0);
    }
    CHECK(strcmp(oxv_status_message(OXV_STATUS_OK), "ok") == 0);
    CHECK(strcmp(oxv_status_message(-1), "unknown status") == 0);
    CHECK(strcmp(oxv_status_message(1000000), "unknown status") == 0);
}

/* ---- one-shot ------------------------------------------------------------- */

static void test_oneshot(void) {
    oxv_buffer buf = {NULL, 0};
    oxv_options *a = options_a();
    int16_t edge[EDGE_FRAMES];

    /* Defaults (NULL options), stereo i16 at 44.1 kHz. */
    CHECK_OK(oxv_encode_i16(stereo, STEREO_FRAMES * 2, 44100, 2, NULL, &buf));
    CHECK(buf.data != NULL && buf.len > 58 && memcmp(buf.data, "OggS", 4) == 0);
    save("oneshot_i16", buf.data, buf.len);
    oxv_buffer_free(&buf);
    CHECK(buf.data == NULL && buf.len == 0);
    oxv_buffer_free(&buf); /* second free: no-op */
    oxv_buffer_free(NULL);

    /* Options A, mono f32 at 22.05 kHz. */
    CHECK_OK(oxv_encode_f32(mono_f, MONO_FRAMES, 22050, 1, a, &buf));
    save("oneshot_f32", buf.data, buf.len);
    oxv_buffer_free(&buf);

    /* Rate edges. */
    gen_i16(edge, EDGE_FRAMES, 1, 99);
    CHECK_OK(oxv_encode_i16(edge, EDGE_FRAMES, OXV_SAMPLE_RATE_MIN, 1, NULL, &buf));
    save("rate_min", buf.data, buf.len);
    oxv_buffer_free(&buf);
    CHECK_OK(oxv_encode_i16(edge, EDGE_FRAMES, OXV_SAMPLE_RATE_MAX, 1, NULL, &buf));
    save("rate_max", buf.data, buf.len);
    oxv_buffer_free(&buf);

    /* Empty input, NULL pcm allowed with 0 samples. */
    CHECK_OK(oxv_encode_i16(NULL, 0, 48000, 2, NULL, &buf));
    save("empty", buf.data, buf.len);
    oxv_buffer_free(&buf);

    /* Errors: out reset to {NULL, 0}. */
    buf.data = (uint8_t *)stereo;
    buf.len = 123;
    CHECK_STATUS(oxv_encode_i16(stereo, 2, 0, 2, NULL, &buf), OXV_STATUS_UNSUPPORTED);
    CHECK(buf.data == NULL && buf.len == 0);
    CHECK_STATUS(oxv_encode_i16(stereo, 2, OXV_SAMPLE_RATE_MAX + 1, 2, NULL, &buf),
                 OXV_STATUS_UNSUPPORTED);
    CHECK_STATUS(oxv_encode_f32(stereo_f, 2, 44100, 0, NULL, &buf), OXV_STATUS_UNSUPPORTED);
    CHECK_STATUS(oxv_encode_f32(stereo_f, 2, 44100, 256, NULL, &buf), OXV_STATUS_UNSUPPORTED);
    CHECK_STATUS(oxv_encode_i16(NULL, 2, 44100, 2, NULL, &buf), OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_encode_i16(stereo, 2, 44100, 2, NULL, NULL), OXV_STATUS_INVALID_ARGUMENT);
    CHECK(buf.data == NULL && buf.len == 0);

    oxv_options_free(a);
}

/* ---- options errors ------------------------------------------------------- */

static void test_options_errors(void) {
    oxv_options *o = oxv_options_new();
    CHECK(o != NULL);
    CHECK_STATUS(oxv_options_set_quality(o, NAN), OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_options_set_quality(o, 10.01f), OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_options_set_quality(o, -1.01f), OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_options_set_quality(o, INFINITY), OXV_STATUS_INVALID_ARGUMENT);
    CHECK_OK(oxv_options_set_quality(o, 10.0f));
    CHECK_OK(oxv_options_set_quality(o, -1.0f));
    CHECK_STATUS(oxv_options_set_quality(NULL, 5.0f), OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_options_set_serial(NULL, 1), OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_options_set_vendor(o, NULL), OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_options_set_vendor(NULL, "x"), OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_options_set_vendor(o, "bad \xff utf-8"), OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_options_add_comment(o, NULL, "v"), OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_options_add_comment(o, "K", NULL), OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_options_add_comment(o, "K", "\xc3"), OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_options_add_comment(NULL, "K", "v"), OXV_STATUS_INVALID_ARGUMENT);
    /* Bitrate arguments a mode doesn't take, unknown modes. */
    CHECK_STATUS(oxv_options_set_bitrate(NULL, OXV_BITRATE_MODE_AVERAGE, 128, 0, 0),
                 OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_options_set_bitrate(o, 99, 128, 0, 0), OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_options_set_bitrate(o, -1, 128, 0, 0), OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_options_set_bitrate(o, OXV_BITRATE_MODE_VBR, 128, 0, 0),
                 OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_options_set_bitrate(o, OXV_BITRATE_MODE_NOMINAL, 128, 0, 160),
                 OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_options_set_bitrate(o, OXV_BITRATE_MODE_LIMITS, 128, 96, 0),
                 OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_options_set_bitrate(o, OXV_BITRATE_MODE_QUALITY_LIMITS, 1, 0, 160),
                 OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_options_set_bitrate_tuning(NULL, -1, NAN, NAN),
                 OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_options_set_bitrate_tuning(o, (int64_t)UINT32_MAX + 1, NAN, NAN),
                 OXV_STATUS_INVALID_ARGUMENT);
    CHECK_OK(oxv_options_set_bitrate_tuning(o, UINT32_MAX, NAN, NAN));
    CHECK_OK(oxv_options_set_bitrate_tuning(o, -1, NAN, NAN));
    /* Rejected calls left the options at VBR. */
    CHECK_OK(oxv_options_check(o, 96000, 2, NULL, 0));
    oxv_options_free(o);
    oxv_options_free(NULL);
}

/* ---- bitrate modes and 5.1 ------------------------------------------------ */

static int append_cb(void *user_data, const uint8_t *data, size_t len);

/* Options "C": quality 6 (for QUALITY_LIMITS), serial 7. */
static oxv_options *options_c(int mode, uint32_t kbps, uint32_t min, uint32_t max) {
    oxv_options *o = oxv_options_new();
    CHECK(o != NULL);
    CHECK_OK(oxv_options_set_quality(o, 6.0f));
    CHECK_OK(oxv_options_set_serial(o, 7));
    CHECK_OK(oxv_options_set_bitrate(o, mode, kbps, min, max));
    return o;
}

static void test_bitrate(void) {
    oxv_options *o;
    oxv_encoder *enc = NULL;
    oxv_writer *w = NULL;
    oxv_buffer buf = {NULL, 0};
    bytes out = {NULL, 0, 0};
    const uint8_t *p;
    size_t n, i, chunk;
    uint32_t lo = 1, hi = 1;
    char msg[256];

    /* Supported ranges. */
    CHECK_OK(oxv_bitrate_supported_kbps(44100, 2, &lo, &hi));
    CHECK(lo == 45 && hi == 500);
    CHECK_OK(oxv_bitrate_supported_kbps(48000, 6, &lo, &hi));
    CHECK(lo == 84 && hi == 1440);
    CHECK_STATUS(oxv_bitrate_supported_kbps(96000, 2, &lo, &hi), OXV_STATUS_UNSUPPORTED);
    CHECK(lo == 0 && hi == 0);
    CHECK_STATUS(oxv_bitrate_supported_kbps(44100, 0, &lo, &hi), OXV_STATUS_UNSUPPORTED);
    CHECK_STATUS(oxv_bitrate_supported_kbps(44100, 2, NULL, &hi), OXV_STATUS_INVALID_ARGUMENT);

    /* "CBR" 128 kb/s stereo, streamed in odd chunks. */
    o = options_c(OXV_BITRATE_MODE_AVERAGE, 128, 128, 128);
    CHECK_OK(oxv_encoder_new(44100, 2, o, &enc));
    oxv_options_free(o);
    for (i = 0; i < STEREO_FRAMES * 2; i += chunk) {
        chunk = STEREO_FRAMES * 2 - i < 3333 ? STEREO_FRAMES * 2 - i : 3333;
        CHECK_OK(oxv_encoder_write_i16(enc, stereo + i, chunk, &p, &n));
        bytes_append(&out, p, n);
    }
    CHECK_OK(oxv_encoder_finish(enc, &p, &n));
    bytes_append(&out, p, n);
    save("managed_cbr_encoder", out.data, out.len);
    oxv_encoder_free(enc);
    out.len = 0;

    /* ABR 5.1 at 48 kHz with reservoir tuning, one-shot f32. */
    o = options_c(OXV_BITRATE_MODE_AVERAGE, 384, 0, 0);
    CHECK_OK(oxv_options_set_bitrate_tuning(o, -1, 0.3, NAN));
    CHECK_OK(oxv_encode_f32(surround_f, SURROUND_FRAMES * 6, 48000, 6, o, &buf));
    save("managed_51_abr_oneshot", buf.data, buf.len);
    oxv_buffer_free(&buf);
    oxv_options_free(o);

    /* Quality 6 capped at 160 kb/s, through the callback writer. */
    o = options_c(OXV_BITRATE_MODE_QUALITY_LIMITS, 0, 0, 160);
    CHECK_OK(oxv_writer_new(44100, 2, o, append_cb, &out, &w));
    oxv_options_free(o);
    CHECK_OK(oxv_writer_write_i16(w, stereo, STEREO_FRAMES * 2));
    CHECK_OK(oxv_writer_finish(w));
    save("managed_qlimits_writer", out.data, out.len);
    oxv_writer_free(w);
    out.len = 0;

    /* -b 96 (nominal, unmanaged) then back to VBR: plain VBR again. */
    o = options_c(OXV_BITRATE_MODE_NOMINAL, 96, 0, 0);
    CHECK_OK(oxv_options_set_bitrate(o, OXV_BITRATE_MODE_VBR, 0, 0, 0));
    CHECK_OK(oxv_encode_i16(stereo, STEREO_FRAMES * 2, 44100, 2, o, &buf));
    save("managed_back_to_vbr", buf.data, buf.len);
    oxv_buffer_free(&buf);
    oxv_options_free(o);

    /* One set of options, valid at 44.1 kHz and not at 96 kHz: every
     * constructor reports UNSUPPORTED there, and still works at 44.1. */
    o = options_c(OXV_BITRATE_MODE_AVERAGE, 160, 0, 0);
    enc = (oxv_encoder *)&enc;
    CHECK_STATUS(oxv_encoder_new(96000, 2, o, &enc), OXV_STATUS_UNSUPPORTED);
    CHECK(enc == NULL);
    w = (oxv_writer *)&w;
    CHECK_STATUS(oxv_writer_new(96000, 2, o, append_cb, &out, &w), OXV_STATUS_UNSUPPORTED);
    CHECK(w == NULL && out.len == 0);
    CHECK_STATUS(oxv_encode_i16(stereo, 2000, 96000, 2, o, &buf), OXV_STATUS_UNSUPPORTED);
    CHECK(buf.data == NULL && buf.len == 0);
    CHECK_STATUS(oxv_encode_f32(stereo_f, 2000, 7999, 2, o, &buf), OXV_STATUS_UNSUPPORTED);
    CHECK_STATUS(oxv_options_check(o, 96000, 2, msg, sizeof msg), OXV_STATUS_UNSUPPORTED);
    CHECK(strstr(msg, "no bitrate setups at 96000 Hz stereo") != NULL);
    CHECK_OK(oxv_options_check(o, 44100, 2, msg, sizeof msg));
    CHECK(msg[0] == '\0');
    CHECK_OK(oxv_encoder_new(44100, 2, o, &enc));
    oxv_encoder_free(enc);
    /* The channel count matters too: 1000 kb/s fits 5.1, not stereo. */
    CHECK_OK(oxv_options_set_bitrate(o, OXV_BITRATE_MODE_AVERAGE, 1000, 0, 0));
    CHECK_OK(oxv_options_check(o, 48000, 6, NULL, 0));
    CHECK_STATUS(oxv_options_check(o, 48000, 2, msg, sizeof msg), OXV_STATUS_UNSUPPORTED);
    CHECK(strstr(msg, "supported: 45..=500 kb/s") != NULL);
    /* Truncated messages stay NUL-terminated. */
    msg[5] = 'x';
    CHECK_STATUS(oxv_options_check(o, 48000, 2, msg, 5), OXV_STATUS_UNSUPPORTED);
    CHECK(strlen(msg) == 4 && msg[5] == 'x');
    CHECK_STATUS(oxv_options_check(o, 0, 2, msg, sizeof msg), OXV_STATUS_UNSUPPORTED);
    CHECK(strstr(msg, "sample rate") != NULL);
    /* Inconsistent requests and tuning are reported at creation, too. */
    CHECK_OK(oxv_options_set_bitrate(o, OXV_BITRATE_MODE_AVERAGE, 128, 160, 0));
    CHECK_STATUS(oxv_encoder_new(44100, 2, o, &enc), OXV_STATUS_UNSUPPORTED);
    CHECK_OK(oxv_options_set_bitrate(o, OXV_BITRATE_MODE_LIMITS, 0, 0, 0));
    CHECK_STATUS(oxv_options_check(o, 44100, 2, msg, sizeof msg), OXV_STATUS_UNSUPPORTED);
    CHECK(strstr(msg, "invalid bitrate request") != NULL);
    CHECK_OK(oxv_options_set_bitrate(o, OXV_BITRATE_MODE_AVERAGE, 128, 0, 0));
    CHECK_OK(oxv_options_set_bitrate_tuning(o, -1, 1.5, NAN));
    CHECK_STATUS(oxv_encoder_new(44100, 2, o, &enc), OXV_STATUS_UNSUPPORTED);
    oxv_options_free(o);
    free(out.data);
}

/* 5.1 VBR: streamed i16 in chunks that split frames, and planar through
 * the writer. */
static void test_surround(void) {
    static int16_t planes_i16[6][SURROUND_FRAMES];
    oxv_encoder *enc = NULL;
    oxv_writer *w = NULL;
    bytes out = {NULL, 0, 0};
    const uint8_t *p;
    const int16_t *planes[6];
    size_t n, i, chunk;
    unsigned c;

    CHECK_OK(oxv_encoder_new(48000, 6, NULL, &enc));
    for (i = 0; i < SURROUND_FRAMES * 6; i += chunk) {
        chunk = SURROUND_FRAMES * 6 - i < 1003 ? SURROUND_FRAMES * 6 - i : 1003;
        CHECK_OK(oxv_encoder_write_i16(enc, surround + i, chunk, &p, &n));
        bytes_append(&out, p, n);
    }
    CHECK_OK(oxv_encoder_finish(enc, &p, &n));
    bytes_append(&out, p, n);
    save("encoder_51", out.data, out.len);
    oxv_encoder_free(enc);
    out.len = 0;

    for (i = 0; i < SURROUND_FRAMES; i++) {
        for (c = 0; c < 6; c++) {
            planes_i16[c][i] = surround[i * 6 + c];
        }
    }
    CHECK_OK(oxv_writer_new(44100, 6, NULL, append_cb, &out, &w));
    for (i = 0; i < SURROUND_FRAMES; i += chunk) {
        chunk = SURROUND_FRAMES - i < 2500 ? SURROUND_FRAMES - i : 2500;
        for (c = 0; c < 6; c++) {
            planes[c] = planes_i16[c] + i;
        }
        CHECK_OK(oxv_writer_write_planar_i16(w, planes, 6, chunk));
        CHECK_STATUS(oxv_writer_write_planar_i16(w, planes, 2, chunk),
                     OXV_STATUS_INVALID_ARGUMENT);
    }
    CHECK_OK(oxv_writer_finish(w));
    save("writer_planar_51", out.data, out.len);
    oxv_writer_free(w);
    free(out.data);
}

/* ---- streaming encoder ---------------------------------------------------- */

static void test_encoder_new_errors(void) {
    oxv_encoder *enc = (oxv_encoder *)&enc; /* non-NULL sentinel */
    CHECK_STATUS(oxv_encoder_new(0, 2, NULL, &enc), OXV_STATUS_UNSUPPORTED);
    CHECK(enc == NULL);
    CHECK_STATUS(oxv_encoder_new(200001, 2, NULL, &enc), OXV_STATUS_UNSUPPORTED);
    CHECK_STATUS(oxv_encoder_new(44100, 0, NULL, &enc), OXV_STATUS_UNSUPPORTED);
    CHECK_STATUS(oxv_encoder_new(44100, OXV_CHANNELS_MAX + 1, NULL, &enc),
                 OXV_STATUS_UNSUPPORTED);
    CHECK_OK(oxv_encoder_new(8000, OXV_CHANNELS_MAX, NULL, &enc));
    oxv_encoder_free(enc);
    CHECK_STATUS(oxv_encoder_new(44100, 0x10001, NULL, &enc), OXV_STATUS_UNSUPPORTED);
    CHECK_STATUS(oxv_encoder_new(44100, 2, NULL, NULL), OXV_STATUS_INVALID_ARGUMENT);
    CHECK(enc == NULL);
    oxv_encoder_free(NULL);
}

/* Interleaved i16, options B, chunks of 1001 samples (odd, so chunks split
 * stereo frames). Error calls in between must not disturb the stream. */
static void test_encoder_i16(void) {
    oxv_options *b = options_b();
    oxv_encoder *enc = NULL;
    bytes out = {NULL, 0, 0};
    const uint8_t *p;
    size_t n, i, chunk;
    const int16_t *planes[3];

    CHECK_OK(oxv_encoder_new(44100, 2, b, &enc));
    oxv_options_free(b); /* the encoder copied it */
    for (i = 0; i < STEREO_FRAMES * 2; i += chunk) {
        chunk = STEREO_FRAMES * 2 - i < 1001 ? STEREO_FRAMES * 2 - i : 1001;
        CHECK_OK(oxv_encoder_write_i16(enc, stereo + i, chunk, &p, &n));
        CHECK(p != NULL);
        bytes_append(&out, p, n);

        if (i == 0) {
            /* Rejected calls: nothing encoded, outputs reset. */
            const uint8_t *q = p;
            size_t m = 99;
            CHECK_STATUS(oxv_encoder_write_i16(enc, NULL, 4, &q, &m),
                         OXV_STATUS_INVALID_ARGUMENT);
            CHECK(q == NULL && m == 0);
            CHECK_STATUS(oxv_encoder_write_i16(NULL, stereo, 4, &q, &m),
                         OXV_STATUS_INVALID_ARGUMENT);
            CHECK_STATUS(oxv_encoder_write_i16(enc, stereo, 4, NULL, &m),
                         OXV_STATUS_INVALID_ARGUMENT);
            CHECK_STATUS(oxv_encoder_write_i16(enc, stereo, 4, &q, NULL),
                         OXV_STATUS_INVALID_ARGUMENT);
            planes[0] = left;
            planes[1] = right;
            planes[2] = left;
            CHECK_STATUS(oxv_encoder_write_planar_i16(enc, planes, 1, 10, &q, &m),
                         OXV_STATUS_INVALID_ARGUMENT);
            CHECK_STATUS(oxv_encoder_write_planar_i16(enc, planes, 3, 10, &q, &m),
                         OXV_STATUS_INVALID_ARGUMENT);
            CHECK_STATUS(oxv_encoder_write_planar_i16(enc, NULL, 2, 10, &q, &m),
                         OXV_STATUS_INVALID_ARGUMENT);
            planes[1] = NULL;
            CHECK_STATUS(oxv_encoder_write_planar_i16(enc, planes, 2, 10, &q, &m),
                         OXV_STATUS_INVALID_ARGUMENT);
            /* Zero samples with NULL pcm is a valid no-op. */
            CHECK_OK(oxv_encoder_write_i16(enc, NULL, 0, &q, &m));
            CHECK(q != NULL && m == 0);
        }
    }
    CHECK_OK(oxv_encoder_finish(enc, &p, &n));
    CHECK(p != NULL && n > 0);
    bytes_append(&out, p, n);
    save("encoder_i16", out.data, out.len);

    /* After finish. */
    {
        const uint8_t *q;
        size_t m;
        CHECK_STATUS(oxv_encoder_write_i16(enc, stereo, 2, &q, &m), OXV_STATUS_INVALID_STATE);
        CHECK_STATUS(oxv_encoder_write_f32(enc, stereo_f, 2, &q, &m), OXV_STATUS_INVALID_STATE);
        CHECK_STATUS(oxv_encoder_finish(enc, &q, &m), OXV_STATUS_INVALID_STATE);
        CHECK(q == NULL && m == 0);
        CHECK_STATUS(oxv_encoder_finish(enc, NULL, &m), OXV_STATUS_INVALID_ARGUMENT);
        CHECK_STATUS(oxv_encoder_finish(NULL, &q, &m), OXV_STATUS_INVALID_ARGUMENT);
    }
    /* The finish bytes stay valid until free. */
    CHECK(memcmp(p, "OggS", 4) == 0);
    oxv_encoder_free(enc);
    free(out.data);
}

/* Interleaved f32, defaults, chunks of 777 samples. */
static void test_encoder_f32(void) {
    oxv_encoder *enc = NULL;
    bytes out = {NULL, 0, 0};
    const uint8_t *p;
    size_t n, i, chunk;

    CHECK_OK(oxv_encoder_new(44100, 2, NULL, &enc));
    for (i = 0; i < STEREO_FRAMES * 2; i += chunk) {
        chunk = STEREO_FRAMES * 2 - i < 777 ? STEREO_FRAMES * 2 - i : 777;
        CHECK_OK(oxv_encoder_write_f32(enc, stereo_f + i, chunk, &p, &n));
        bytes_append(&out, p, n);
    }
    CHECK_OK(oxv_encoder_finish(enc, &p, &n));
    bytes_append(&out, p, n);
    save("encoder_f32", out.data, out.len);
    oxv_encoder_free(enc);
    free(out.data);
}

/* Planar f32 and i16, options A, 500-frame chunks. */
static void test_encoder_planar(int f32) {
    oxv_options *a = options_a();
    oxv_encoder *enc = NULL;
    bytes out = {NULL, 0, 0};
    const uint8_t *p;
    size_t n, i, chunk;

    CHECK_OK(oxv_encoder_new(44100, 2, a, &enc));
    for (i = 0; i < STEREO_FRAMES; i += chunk) {
        chunk = STEREO_FRAMES - i < 500 ? STEREO_FRAMES - i : 500;
        if (f32) {
            const float *planes[2];
            planes[0] = left_f + i;
            planes[1] = right_f + i;
            CHECK_OK(oxv_encoder_write_planar_f32(enc, planes, 2, chunk, &p, &n));
        } else {
            const int16_t *planes[2];
            planes[0] = left + i;
            planes[1] = right + i;
            CHECK_OK(oxv_encoder_write_planar_i16(enc, planes, 2, chunk, &p, &n));
        }
        bytes_append(&out, p, n);
    }
    CHECK_OK(oxv_encoder_finish(enc, &p, &n));
    bytes_append(&out, p, n);
    save(f32 ? "encoder_planar_f32" : "encoder_planar_i16", out.data, out.len);
    oxv_encoder_free(enc);
    oxv_options_free(a);
    free(out.data);
}

/* An encoder freed without finishing must not leak. */
static void test_encoder_abandoned(void) {
    oxv_encoder *enc = NULL;
    const uint8_t *p;
    size_t n;
    CHECK_OK(oxv_encoder_new(48000, 1, NULL, &enc));
    CHECK_OK(oxv_encoder_write_i16(enc, mono, MONO_FRAMES, &p, &n));
    oxv_encoder_free(enc);
}

/* ---- callback writer ------------------------------------------------------ */

static int append_cb(void *user_data, const uint8_t *data, size_t len) {
    bytes_append((bytes *)user_data, data, len);
    return 0;
}

static int fail_cb(void *user_data, const uint8_t *data, size_t len) {
    (void)data;
    (void)len;
    ++*(int *)user_data;
    return -1;
}

/* kind: 0 = i16, 1 = f32, 2 = planar f32, 3 = planar i16; options B. */
static void test_writer(int kind) {
    static const char *names[] = {"writer_i16", "writer_f32", "writer_planar_f32",
                                  "writer_planar_i16"};
    oxv_options *b = options_b();
    oxv_writer *w = NULL;
    bytes out = {NULL, 0, 0};
    size_t i, chunk;

    CHECK_OK(oxv_writer_new(44100, 2, b, append_cb, &out, &w));
    oxv_options_free(b);
    CHECK(out.len == 0); /* nothing written before the first call */
    if (kind <= 1) {
        for (i = 0; i < STEREO_FRAMES * 2; i += chunk) {
            chunk = STEREO_FRAMES * 2 - i < 4097 ? STEREO_FRAMES * 2 - i : 4097;
            if (kind == 0) {
                CHECK_OK(oxv_writer_write_i16(w, stereo + i, chunk));
            } else {
                CHECK_OK(oxv_writer_write_f32(w, stereo_f + i, chunk));
            }
        }
    } else {
        for (i = 0; i < STEREO_FRAMES; i += chunk) {
            chunk = STEREO_FRAMES - i < 2000 ? STEREO_FRAMES - i : 2000;
            if (kind == 2) {
                const float *planes[2];
                planes[0] = left_f + i;
                planes[1] = right_f + i;
                CHECK_OK(oxv_writer_write_planar_f32(w, planes, 2, chunk));
                CHECK_STATUS(oxv_writer_write_planar_f32(w, planes, 1, chunk),
                             OXV_STATUS_INVALID_ARGUMENT);
            } else {
                const int16_t *planes[2];
                planes[0] = left + i;
                planes[1] = right + i;
                CHECK_OK(oxv_writer_write_planar_i16(w, planes, 2, chunk));
                CHECK_STATUS(oxv_writer_write_planar_i16(w, NULL, 2, chunk),
                             OXV_STATUS_INVALID_ARGUMENT);
            }
        }
    }
    CHECK_OK(oxv_writer_finish(w));
    CHECK_STATUS(oxv_writer_finish(w), OXV_STATUS_INVALID_STATE);
    CHECK_STATUS(oxv_writer_write_i16(w, stereo, 2), OXV_STATUS_INVALID_STATE);
    save(names[kind], out.data, out.len);
    oxv_writer_free(w);
    free(out.data);
}

static void test_writer_errors(void) {
    oxv_writer *w = (oxv_writer *)&w;
    int calls = 0;

    CHECK_STATUS(oxv_writer_new(44100, 2, NULL, NULL, NULL, &w), OXV_STATUS_INVALID_ARGUMENT);
    CHECK(w == NULL);
    CHECK_STATUS(oxv_writer_new(44100, 2, NULL, fail_cb, &calls, NULL),
                 OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_writer_new(0, 2, NULL, fail_cb, &calls, &w), OXV_STATUS_UNSUPPORTED);
    CHECK_STATUS(oxv_writer_new(44100, 3000, NULL, fail_cb, &calls, &w),
                 OXV_STATUS_UNSUPPORTED);
    CHECK_STATUS(oxv_writer_write_i16(NULL, stereo, 2), OXV_STATUS_INVALID_ARGUMENT);
    CHECK_STATUS(oxv_writer_finish(NULL), OXV_STATUS_INVALID_ARGUMENT);
    oxv_writer_free(NULL);

    /* A failing callback: the first write delivers the header pages, so it
     * fails; the writer is then unusable. */
    CHECK_OK(oxv_writer_new(44100, 2, NULL, fail_cb, &calls, &w));
    CHECK_STATUS(oxv_writer_write_i16(w, stereo, 2), OXV_STATUS_IO);
    CHECK(calls == 1);
    CHECK_STATUS(oxv_writer_write_i16(w, stereo, 2), OXV_STATUS_INVALID_STATE);
    CHECK_STATUS(oxv_writer_finish(w), OXV_STATUS_INVALID_STATE);
    CHECK(calls == 1);
    oxv_writer_free(w);

    /* Failure at finish. */
    calls = 0;
    CHECK_OK(oxv_writer_new(22050, 1, NULL, fail_cb, &calls, &w));
    CHECK_STATUS(oxv_writer_finish(w), OXV_STATUS_IO);
    CHECK_STATUS(oxv_writer_finish(w), OXV_STATUS_INVALID_STATE);
    oxv_writer_free(w);

    /* Freed unfinished: no leak, no callback. */
    calls = 0;
    CHECK_OK(oxv_writer_new(22050, 1, NULL, fail_cb, &calls, &w));
    oxv_writer_free(w);
    CHECK(calls == 0);
}

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: %s <out-dir>\n", argv[0]);
        return 2;
    }
    out_dir = argv[1];

    gen_i16(stereo, STEREO_FRAMES, 2, 1);
    to_f32(stereo, stereo_f, STEREO_FRAMES * 2);
    gen_i16(mono, MONO_FRAMES, 1, 7);
    to_f32(mono, mono_f, MONO_FRAMES);
    gen_i16(surround, SURROUND_FRAMES, 6, 11);
    to_f32(surround, surround_f, SURROUND_FRAMES * 6);
    deinterleave();

    test_version();
    test_options_errors();
    test_oneshot();
    test_encoder_new_errors();
    test_encoder_i16();
    test_encoder_f32();
    test_encoder_planar(1);
    test_encoder_planar(0);
    test_encoder_abandoned();
    test_writer(0);
    test_writer(1);
    test_writer(2);
    test_writer(3);
    test_writer_errors();
    test_surround();
    test_bitrate();

    if (failures) {
        fprintf(stderr, "%d check(s) failed\n", failures);
        return 1;
    }
    printf("all C API checks passed (oxivorbis %s)\n", oxv_version());
    return 0;
}
