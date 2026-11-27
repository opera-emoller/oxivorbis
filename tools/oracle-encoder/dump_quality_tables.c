/*
 * dump_quality_tables — prints the libvorbis 1.3.7 mode-template tables that
 * drive quality-dependent psy / global / ID-header setup, for every 1-2
 * channel ve_setup_data_template in vorbisenc.c's setup_list, as Rust source
 * on stdout. The output is src/tables/quality.rs.
 *
 * Why generate instead of transcribing: psych_44.h contains rows with
 * missing commas (e.g. `-7  -3` parses as the single value -10), and the
 * Q5 output depends on that. Compiling the real headers is the only way to
 * be sure the Rust tables hold the values libvorbis actually uses.
 *
 * Each table is emitted once, under its C name, with its full C length
 * (sizeof). The templates refer to tables by pointer; every pointer is
 * resolved against the registry below by identity, and an unregistered
 * pointer aborts generation, so a template can never silently point at
 * the wrong data. Only the symbol names are listed here; no value is.
 *
 * This file #includes vorbisenc.c itself so that all its private typedefs
 * (att3, noise3, adj_stereo, ...) and static tables are in scope. It must be
 * linked with the rest of the vendored libvorbis sources (minus vorbisenc.c);
 * see build_quality_dumpers.sh. Regenerate via `just regen-quality-tables`.
 *
 * Float values are printed with %.9g (f32) / %.17g (f64), which round-trips
 * exactly through Rust's correctly-rounded literal parser; the oracle-gated
 * test `quality_oracle_grid` confirms the end-to-end bits.
 */
#include <stdio.h>

#include "vorbisenc.c"

#include <ctype.h>

static void pf32(float v) {
    /* %.9g always round-trips an IEEE single. Append the suffix so integral
     * values (e.g. "12") still parse as float literals. */
    printf("%.9g_f32", (double)v);
}
static void pf64(double v) { printf("%.17g_f64", v); }

#define N(a) ((int)(sizeof(a) / sizeof((a)[0])))

/* ---------------------------------------------------------------- */
/* Table registry                                                     */
/* ---------------------------------------------------------------- */

enum kind { K_F64, K_I32, K_ATT3, K_ADJ, K_NOISE3, K_GUARD, K_COMPAND, K_GLOBAL, K_STEREO, NK };

typedef struct {
    const void *p;
    const char *cname;
    int n;
    int used;
} reg_t;

#define MAXREG 64
static reg_t regs[NK][MAXREG];
static int nregs[NK];

static void reg(int k, const char *cname, const void *p, int n) {
    if (nregs[k] >= MAXREG) {
        fprintf(stderr, "dump_quality_tables: registry full\n");
        exit(1);
    }
    regs[k][nregs[k]].p = p;
    regs[k][nregs[k]].cname = cname;
    regs[k][nregs[k]].n = n;
    nregs[k]++;
}
#define REG(k, sym) reg(k, #sym, (const void *)(sym), N(sym))

static void register_tables(void) {
    /* setup_*.h */
    REG(K_F64, rate_mapping_44_stereo);
    REG(K_F64, rate_mapping_44_un);
    REG(K_F64, quality_mapping_44);
    REG(K_I32, blocksize_short_44);
    REG(K_I32, blocksize_long_44);
    REG(K_F64, _psy_compand_short_mapping);
    REG(K_F64, _psy_compand_long_mapping);
    REG(K_F64, _global_mapping_44);
    REG(K_F64, rate_mapping_32);
    REG(K_F64, rate_mapping_32_un);
    REG(K_F64, _psy_lowpass_32);
    REG(K_F64, rate_mapping_22);
    REG(K_F64, rate_mapping_22_uncoupled);
    REG(K_F64, _psy_lowpass_22);
    REG(K_I32, blocksize_16_short);
    REG(K_I32, blocksize_16_long);
    REG(K_F64, rate_mapping_16);
    REG(K_F64, rate_mapping_16_uncoupled);
    REG(K_F64, _global_mapping_16);
    REG(K_F64, quality_mapping_16);
    REG(K_F64, _psy_compand_16_mapping);
    REG(K_I32, blocksize_11);
    REG(K_F64, rate_mapping_11);
    REG(K_F64, rate_mapping_11_uncoupled);
    REG(K_F64, quality_mapping_11);
    REG(K_I32, blocksize_8);
    REG(K_F64, rate_mapping_8);
    REG(K_F64, rate_mapping_8_uncoupled);
    REG(K_F64, quality_mapping_8);
    REG(K_F64, _psy_compand_8_mapping);
    REG(K_F64, _global_mapping_8);
    REG(K_F64, rate_mapping_X);
    /* psych_44.h */
    REG(K_GLOBAL, _psy_global_44);
    REG(K_COMPAND, _psy_compand_44);
    REG(K_ADJ, _vp_tonemask_adj_longblock);
    REG(K_ADJ, _vp_tonemask_adj_otherblock);
    REG(K_NOISE3, _psy_noisebias_trans);
    REG(K_NOISE3, _psy_noisebias_long);
    REG(K_NOISE3, _psy_noisebias_impulse);
    REG(K_NOISE3, _psy_noisebias_padding);
    REG(K_GUARD, _psy_noiseguards_44);
    REG(K_I32, _psy_tone_suppress);
    REG(K_I32, _psy_tone_0dB);
    REG(K_I32, _psy_noise_suppress);
    REG(K_I32, _psy_ath_floater);
    REG(K_I32, _psy_ath_abs);
    REG(K_STEREO, _psy_stereo_modes_44);
    REG(K_ATT3, _psy_tone_masteratt_44);
    REG(K_F64, _psy_lowpass_44);
    REG(K_I32, _noise_start_short_44);
    REG(K_I32, _noise_start_long_44);
    REG(K_I32, _noise_part_short_44);
    REG(K_I32, _noise_part_long_44);
    REG(K_F64, _noise_thresh_44);
    REG(K_F64, _noise_thresh_5only);
    /* psych_8.h */
    REG(K_ATT3, _psy_tone_masteratt_8);
    REG(K_ADJ, _vp_tonemask_adj_8);
    REG(K_NOISE3, _psy_noisebias_8);
    REG(K_STEREO, _psy_stereo_modes_8);
    REG(K_GUARD, _psy_noiseguards_8);
    REG(K_COMPAND, _psy_compand_8);
    REG(K_F64, _psy_lowpass_8);
    REG(K_I32, _noise_start_8);
    REG(K_I32, _noise_part_8);
    REG(K_I32, _psy_ath_floater_8);
    REG(K_I32, _psy_ath_abs_8);
    /* psych_11.h */
    REG(K_F64, _psy_lowpass_11);
    REG(K_ATT3, _psy_tone_masteratt_11);
    REG(K_ADJ, _vp_tonemask_adj_11);
    REG(K_NOISE3, _psy_noisebias_11);
    REG(K_F64, _noise_thresh_11);
    /* psych_16.h */
    REG(K_STEREO, _psy_stereo_modes_16);
    REG(K_F64, _psy_lowpass_16);
    REG(K_ATT3, _psy_tone_masteratt_16);
    REG(K_ADJ, _vp_tonemask_adj_16);
    REG(K_NOISE3, _psy_noisebias_16_short);
    REG(K_NOISE3, _psy_noisebias_16_impulse);
    REG(K_NOISE3, _psy_noisebias_16);
    REG(K_GUARD, _psy_noiseguards_16);
    REG(K_F64, _noise_thresh_16);
    REG(K_I32, _noise_start_16);
    REG(K_I32, _noise_part_16);
    REG(K_I32, _psy_ath_floater_16);
    REG(K_I32, _psy_ath_abs_16);
}

/* Rust name of a C table: leading underscores dropped, upper-cased. */
static const char *rname(const char *cname) {
    static char buf[8][96];
    static int slot;
    char *b = buf[slot++ & 7];
    int i = 0;
    while (*cname == '_') cname++;
    for (; *cname && i < 95; cname++) b[i++] = (char)toupper((unsigned char)*cname);
    b[i] = 0;
    return b;
}

static reg_t *find(int k, const void *p) {
    int i;
    for (i = 0; i < nregs[k]; i++)
        if (regs[k][i].p == p) return &regs[k][i];
    return NULL;
}

/* Resolve a template pointer; NULL stays NULL ("None"). `need` is the
 * minimum length the encoder setup indexes (checked, so the Rust side can
 * index the same way without going out of bounds where C would not). */
static const char *ref(int k, const void *p, int need, const char *what) {
    reg_t *r;
    if (!p) return NULL;
    r = find(k, p);
    if (!r) {
        fprintf(stderr, "dump_quality_tables: unregistered table for %s\n", what);
        exit(1);
    }
    if (r->n < need) {
        fprintf(stderr, "dump_quality_tables: %s (%s) has %d entries, need %d\n", what,
                r->cname, r->n, need);
        exit(1);
    }
    r->used = 1;
    return strdup(rname(r->cname));
}

/* ---------------------------------------------------------------- */
/* Table emission                                                     */
/* ---------------------------------------------------------------- */

static void emit_table(int k, const reg_t *r) {
    int i, j, m;
    const char *n = rname(r->cname);
    printf("/// `%s`.\n", r->cname);
    switch (k) {
    case K_F64: {
        const double *a = r->p;
        printf("pub(crate) static %s: [f64; %d] = [", n, r->n);
        for (i = 0; i < r->n; i++) {
            if (i) printf(", ");
            pf64(a[i]);
        }
        printf("];\n\n");
        break;
    }
    case K_I32: {
        const int *a = r->p;
        printf("pub(crate) static %s: [i32; %d] = [", n, r->n);
        for (i = 0; i < r->n; i++) printf("%s%d", i ? ", " : "", a[i]);
        printf("];\n\n");
        break;
    }
    case K_ATT3: {
        const att3 *a = r->p;
        printf("pub(crate) static %s: [Att3; %d] = [\n", n, r->n);
        for (i = 0; i < r->n; i++) {
            printf("    Att3 {\n        att: [%d, %d, %d],\n        boost: ", a[i].att[0],
                   a[i].att[1], a[i].att[2]);
            pf32(a[i].boost);
            printf(",\n        decay: ");
            pf32(a[i].decay);
            printf(",\n    },\n");
        }
        printf("];\n\n");
        break;
    }
    case K_ADJ: {
        const vp_adjblock *a = r->p;
        printf("pub(crate) static %s: [[i32; %d]; %d] = [\n", n, P_BANDS, r->n);
        for (i = 0; i < r->n; i++) {
            printf("    [");
            for (j = 0; j < P_BANDS; j++) printf("%s%d", j ? ", " : "", a[i].block[j]);
            printf("],\n");
        }
        printf("];\n\n");
        break;
    }
    case K_NOISE3: {
        const noise3 *a = r->p;
        printf("pub(crate) static %s: [[[i32; %d]; %d]; %d] = [\n", n, P_BANDS, P_NOISECURVES,
               r->n);
        for (i = 0; i < r->n; i++) {
            printf("    [\n");
            for (j = 0; j < P_NOISECURVES; j++) {
                printf("        [");
                for (m = 0; m < P_BANDS; m++) printf("%s%d", m ? ", " : "", a[i].data[j][m]);
                printf("],\n");
            }
            printf("    ],\n");
        }
        printf("];\n\n");
        break;
    }
    case K_GUARD: {
        const noiseguard *a = r->p;
        printf("pub(crate) static %s: [[i32; 3]; %d] = [\n", n, r->n);
        for (i = 0; i < r->n; i++) printf("    [%d, %d, %d],\n", a[i].lo, a[i].hi, a[i].fixed);
        printf("];\n\n");
        break;
    }
    case K_COMPAND: {
        const compandblock *a = r->p;
        printf("pub(crate) static %s: [[i32; %d]; %d] = [\n", n, NOISE_COMPAND_LEVELS, r->n);
        for (i = 0; i < r->n; i++) {
            printf("    [");
            for (j = 0; j < NOISE_COMPAND_LEVELS; j++) printf("%s%d", j ? ", " : "", a[i].data[j]);
            printf("],\n");
        }
        printf("];\n\n");
        break;
    }
    case K_GLOBAL: {
        const vorbis_info_psy_global *a = r->p;
        printf("pub(crate) static %s: [PsyGlobalTemplate; %d] = [\n", n, r->n);
        for (i = 0; i < r->n; i++) {
            const vorbis_info_psy_global *g = &a[i];
            printf("    PsyGlobalTemplate {\n");
            printf("        eighth_octave_lines: %d,\n", g->eighth_octave_lines);
            printf("        preecho_thresh: [");
            for (j = 0; j < VE_BANDS; j++) {
                if (j) printf(", ");
                pf32(g->preecho_thresh[j]);
            }
            printf("],\n        postecho_thresh: [");
            for (j = 0; j < VE_BANDS; j++) {
                if (j) printf(", ");
                pf32(g->postecho_thresh[j]);
            }
            printf("],\n        stretch_penalty: ");
            pf32(g->stretch_penalty);
            printf(",\n        preecho_minenergy: ");
            pf32(g->preecho_minenergy);
            printf(",\n        ampmax_att_per_sec: ");
            pf32(g->ampmax_att_per_sec);
            printf(",\n        coupling_pkhz: [");
            for (j = 0; j < PACKETBLOBS; j++) printf("%s%d", j ? ", " : "", g->coupling_pkHz[j]);
            printf("],\n        coupling_pointlimit: [");
            for (m = 0; m < 2; m++) {
                printf("%s[", m ? ", " : "");
                for (j = 0; j < PACKETBLOBS; j++)
                    printf("%s%d", j ? ", " : "", g->coupling_pointlimit[m][j]);
                printf("]");
            }
            printf("],\n        coupling_prepointamp: [");
            for (j = 0; j < PACKETBLOBS; j++)
                printf("%s%d", j ? ", " : "", g->coupling_prepointamp[j]);
            printf("],\n        coupling_postpointamp: [");
            for (j = 0; j < PACKETBLOBS; j++)
                printf("%s%d", j ? ", " : "", g->coupling_postpointamp[j]);
            printf("],\n        sliding_lowpass: [");
            for (m = 0; m < 2; m++) {
                printf("%s[", m ? ", " : "");
                for (j = 0; j < PACKETBLOBS; j++)
                    printf("%s%d", j ? ", " : "", g->sliding_lowpass[m][j]);
                printf("]");
            }
            printf("],\n    },\n");
        }
        printf("];\n\n");
        break;
    }
    case K_STEREO: {
        const adj_stereo *a = r->p;
        printf("pub(crate) static %s: [AdjStereo; %d] = [\n", n, r->n);
        for (i = 0; i < r->n; i++) {
            const adj_stereo *s = &a[i];
            printf("    AdjStereo {\n        pre: [");
            for (j = 0; j < PACKETBLOBS; j++) printf("%s%d", j ? ", " : "", s->pre[j]);
            printf("],\n        post: [");
            for (j = 0; j < PACKETBLOBS; j++) printf("%s%d", j ? ", " : "", s->post[j]);
            printf("],\n        khz: [");
            for (j = 0; j < PACKETBLOBS; j++) {
                if (j) printf(", ");
                pf32(s->kHz[j]);
            }
            printf("],\n        lowpass_khz: [");
            for (j = 0; j < PACKETBLOBS; j++) {
                if (j) printf(", ");
                pf32(s->lowpasskHz[j]);
            }
            printf("],\n    },\n");
        }
        printf("];\n\n");
        break;
    }
    }
}

/* ---------------------------------------------------------------- */
/* Templates                                                          */
/* ---------------------------------------------------------------- */

/* setup_list order minus ve_setup_44_51 (6 channels only); must match
 * tools/gen-setup-templates/gen.c, which a unit test cross-checks. */
static const ve_setup_data_template *const templates[] = {
    &ve_setup_44_stereo, &ve_setup_44_uncoupled, &ve_setup_32_stereo,
    &ve_setup_32_uncoupled, &ve_setup_22_stereo, &ve_setup_22_uncoupled,
    &ve_setup_16_stereo, &ve_setup_16_uncoupled, &ve_setup_11_stereo,
    &ve_setup_11_uncoupled, &ve_setup_8_stereo, &ve_setup_8_uncoupled,
    &ve_setup_X_stereo, &ve_setup_X_uncoupled, &ve_setup_XX_stereo,
    &ve_setup_XX_uncoupled,
};
static const char *const template_names[] = {
    "44_STEREO", "44_UNCOUPLED", "32_STEREO", "32_UNCOUPLED", "22_STEREO", "22_UNCOUPLED",
    "16_STEREO", "16_UNCOUPLED", "11_STEREO", "11_UNCOUPLED", "8_STEREO", "8_UNCOUPLED",
    "X_STEREO",  "X_UNCOUPLED",  "XX_STEREO", "XX_UNCOUPLED",
};
#define NTEMPLATES N(templates)

typedef struct {
    const char *f[40];
} resolved;
static resolved res[NTEMPLATES];

static void check_setup_list(void) {
    int i, t = 0;
    for (i = 0; setup_list[i]; i++) {
        if (setup_list[i] == &ve_setup_44_51) {
            if (setup_list[i]->coupling_restriction != 6) exit(1);
            continue;
        }
        if (t >= NTEMPLATES || setup_list[i] != templates[t]) {
            fprintf(stderr, "dump_quality_tables: templates[] is not setup_list\n");
            exit(1);
        }
        t++;
    }
    if (t != NTEMPLATES) exit(1);
}

enum {
    F_RATE, F_QUAL, F_BSS, F_BSL, F_MASTERATT, F_0DB, F_TSUPP, F_ADJ_IMP, F_ADJ_LONG,
    F_ADJ_OTHER, F_GUARDS, F_NB_IMP, F_NB_PAD, F_NB_TRANS, F_NB_LONG, F_NSUPP, F_COMPAND,
    F_CMAP_S, F_CMAP_L, F_NS0, F_NS1, F_NP0, F_NP1, F_NTHRESH, F_ATHF, F_ATHA, F_LOWPASS,
    F_GLOBAL, F_GMAP, F_STEREO, NF
};

/* Entries a remapped interpolation (global psych, compand) can read:
 * x[is]..x[is+1] lead to in[(int)v] and in[(int)v+1], except that an
 * integral v > 0 reads in[v-1] and in[v]. */
static int remap_need(const double *x, int m) {
    double mx = 0;
    int i;
    for (i = 0; i <= m; i++)
        if (x[i] > mx) mx = x[i];
    return (int)ceil(mx) + 1;
}

static void resolve(int t) {
    const ve_setup_data_template *s = templates[t];
    int m = s->mappings, is, single = 0, i;
    const char **f = res[t].f;
    /* Tables interpolated between [is] and [is+1] need mappings+1
     * entries; those indexed by [is] alone need mappings. */
    for (is = 0; is < m; is++)
        if (s->blocksize_short[is] == s->blocksize_long[is]) single = 1;
    f[F_RATE] = ref(K_F64, s->rate_mapping, m + 1, "rate_mapping");
    f[F_QUAL] = ref(K_F64, s->quality_mapping, m + 1, "quality_mapping");
    f[F_BSS] = ref(K_I32, s->blocksize_short, m, "blocksize_short");
    f[F_BSL] = ref(K_I32, s->blocksize_long, m, "blocksize_long");
    f[F_MASTERATT] = ref(K_ATT3, s->psy_tone_masteratt, m + 1, "psy_tone_masteratt");
    f[F_0DB] = ref(K_I32, s->psy_tone_0dB, m + 1, "psy_tone_0dB");
    f[F_TSUPP] = ref(K_I32, s->psy_tone_dBsuppress, m + 1, "psy_tone_dBsuppress");
    f[F_ADJ_IMP] = ref(K_ADJ, s->psy_tone_adj_impulse, m + 1, "psy_tone_adj_impulse");
    f[F_ADJ_LONG] = ref(K_ADJ, s->psy_tone_adj_long, m + 1, "psy_tone_adj_long");
    f[F_ADJ_OTHER] = ref(K_ADJ, s->psy_tone_adj_other, m + 1, "psy_tone_adj_other");
    f[F_GUARDS] = ref(K_GUARD, s->psy_noiseguards, single ? 2 : 4, "psy_noiseguards");
    f[F_NB_IMP] = ref(K_NOISE3, s->psy_noise_bias_impulse, m + 1, "psy_noise_bias_impulse");
    f[F_NB_PAD] = ref(K_NOISE3, s->psy_noise_bias_padding, m + 1, "psy_noise_bias_padding");
    f[F_NB_TRANS] = ref(K_NOISE3, s->psy_noise_bias_trans, m + 1, "psy_noise_bias_trans");
    f[F_NB_LONG] = ref(K_NOISE3, s->psy_noise_bias_long, m + 1, "psy_noise_bias_long");
    f[F_NSUPP] = ref(K_I32, s->psy_noise_dBsuppress, m + 1, "psy_noise_dBsuppress");
    f[F_COMPAND] = ref(K_COMPAND, s->psy_noise_compand, remap_need(s->psy_noise_compand_short_mapping, m),
                       "psy_noise_compand");
    f[F_CMAP_S] = ref(K_F64, s->psy_noise_compand_short_mapping, m + 1, "compand_short_mapping");
    f[F_CMAP_L] = ref(K_F64, s->psy_noise_compand_long_mapping, m + 1, "compand_long_mapping");
    f[F_NS0] = ref(K_I32, s->psy_noise_normal_start[0], m, "psy_noise_normal_start[0]");
    f[F_NS1] = ref(K_I32, s->psy_noise_normal_start[1], m, "psy_noise_normal_start[1]");
    f[F_NP0] = ref(K_I32, s->psy_noise_normal_partition[0], m, "psy_noise_normal_partition[0]");
    f[F_NP1] = ref(K_I32, s->psy_noise_normal_partition[1], m, "psy_noise_normal_partition[1]");
    f[F_NTHRESH] = ref(K_F64, s->psy_noise_normal_thresh, m, "psy_noise_normal_thresh");
    f[F_ATHF] = ref(K_I32, s->psy_ath_float, m + 1, "psy_ath_float");
    f[F_ATHA] = ref(K_I32, s->psy_ath_abs, m + 1, "psy_ath_abs");
    f[F_LOWPASS] = ref(K_F64, s->psy_lowpass, m + 1, "psy_lowpass");
    f[F_GLOBAL] = ref(K_GLOBAL, s->global_params, remap_need(s->global_mapping, m), "global_params");
    if (s->psy_noise_compand_long_mapping &&
        remap_need(s->psy_noise_compand_long_mapping, m) > find(K_COMPAND, s->psy_noise_compand)->n) {
        fprintf(stderr, "dump_quality_tables: compand table too short for long mapping\n");
        exit(1);
    }
    f[F_GMAP] = ref(K_F64, s->global_mapping, m + 1, "global_mapping");
    f[F_STEREO] = ref(K_STEREO, s->stereo_modes, m + 1, "stereo_modes");

    /* Tables every template must have, and the ones only two-block
     * templates may omit (vorbis_encode_setup_init skips blocks 2/3 when
     * the short and long block sizes are equal). */
    for (i = 0; i < NF; i++) {
        int optional = i == F_STEREO || (single && (i == F_ADJ_LONG || i == F_NB_TRANS ||
                                                     i == F_NB_LONG || i == F_CMAP_L));
        if (!f[i] && !optional) {
            fprintf(stderr, "dump_quality_tables: template %s field %d is NULL\n",
                    template_names[t], i);
            exit(1);
        }
    }
    /* Mixed single/double block templates would need per-setting psys
     * handling of the optional tables above; libvorbis has none with NULLs. */
}

static void opt(const char *field, const char *v) {
    if (v)
        printf("    %s: Some(&%s),\n", field, v);
    else
        printf("    %s: None,\n", field);
}
static void req(const char *field, const char *v) { printf("    %s: &%s,\n", field, v); }

static void emit_template(int t) {
    const ve_setup_data_template *s = templates[t];
    const char **f = res[t].f;
    {
        /* C name: ve_setup_44_stereo, ve_setup_X_uncoupled, ... */
        const char *c;
        printf("/// `ve_setup_");
        for (c = template_names[t]; *c; c++)
            putchar(*c == 'X' ? 'X' : tolower((unsigned char)*c));
        printf("`.\n");
    }
    printf("pub(crate) static SETUP_%s: QualityTemplate = QualityTemplate {\n",
           template_names[t]);
    printf("    name: \"%s\",\n", template_names[t]);
    printf("    mappings: %d,\n", s->mappings);
    req("rate_mapping", f[F_RATE]);
    req("quality_mapping", f[F_QUAL]);
    printf("    coupling_restriction: %d,\n", s->coupling_restriction);
    printf("    samplerate_min_restriction: %ld,\n", s->samplerate_min_restriction);
    printf("    samplerate_max_restriction: %ld,\n", s->samplerate_max_restriction);
    req("blocksize_short", f[F_BSS]);
    req("blocksize_long", f[F_BSL]);
    req("psy_tone_masteratt", f[F_MASTERATT]);
    req("psy_tone_0db", f[F_0DB]);
    req("psy_tone_dbsuppress", f[F_TSUPP]);
    req("psy_tone_adj_impulse", f[F_ADJ_IMP]);
    opt("psy_tone_adj_long", f[F_ADJ_LONG]);
    req("psy_tone_adj_other", f[F_ADJ_OTHER]);
    req("psy_noiseguards", f[F_GUARDS]);
    req("psy_noise_bias_impulse", f[F_NB_IMP]);
    req("psy_noise_bias_padding", f[F_NB_PAD]);
    opt("psy_noise_bias_trans", f[F_NB_TRANS]);
    opt("psy_noise_bias_long", f[F_NB_LONG]);
    req("psy_noise_dbsuppress", f[F_NSUPP]);
    req("psy_noise_compand", f[F_COMPAND]);
    req("psy_noise_compand_short_mapping", f[F_CMAP_S]);
    opt("psy_noise_compand_long_mapping", f[F_CMAP_L]);
    printf("    psy_noise_normal_start: [&%s, &%s],\n", f[F_NS0], f[F_NS1]);
    printf("    psy_noise_normal_partition: [&%s, &%s],\n", f[F_NP0], f[F_NP1]);
    req("psy_noise_normal_thresh", f[F_NTHRESH]);
    req("psy_ath_float", f[F_ATHF]);
    req("psy_ath_abs", f[F_ATHA]);
    req("psy_lowpass", f[F_LOWPASS]);
    req("global_params", f[F_GLOBAL]);
    req("global_mapping", f[F_GMAP]);
    opt("stereo_modes", f[F_STEREO]);
    printf("};\n\n");
}

int main(void) {
    int t, k, i;
    const vorbis_info_psy *pt = &_psy_info_template;

    check_setup_list();
    register_tables();
    for (t = 0; t < NTEMPLATES; t++) resolve(t);

    printf("// AUTO-GENERATED by tools/oracle-encoder/dump_quality_tables.c from the\n");
    printf("// vendored libvorbis 1.3.7 modes/{setup_*,psych_*}.h.\n");
    printf("// Do not edit. Regenerate via `just regen-quality-tables`.\n");
    printf("//\n");
    printf("// The quality-dependent half of every 1-2 channel ve_setup_data_template\n");
    printf("// in vorbisenc.c's setup_list (all but ve_setup_44_51), in list order.\n");
    printf("// Each table appears once under its C name (leading `_` dropped,\n");
    printf("// upper-cased) with its full C length.\n");
    printf("#![allow(clippy::excessive_precision)]\n");
    printf("#![allow(clippy::unreadable_literal)]\n\n");

    printf("/// `vorbis_info_psy_global` template entry (`_psy_global_44[i]`).\n");
    printf("#[derive(Clone, Copy, Debug)]\n");
    printf("pub(crate) struct PsyGlobalTemplate {\n");
    printf("    pub eighth_octave_lines: i32,\n");
    printf("    pub preecho_thresh: [f32; %d],\n", VE_BANDS);
    printf("    pub postecho_thresh: [f32; %d],\n", VE_BANDS);
    printf("    pub stretch_penalty: f32,\n");
    printf("    pub preecho_minenergy: f32,\n");
    printf("    pub ampmax_att_per_sec: f32,\n");
    printf("    pub coupling_pkhz: [i32; %d],\n", PACKETBLOBS);
    printf("    pub coupling_pointlimit: [[i32; %d]; 2],\n", PACKETBLOBS);
    printf("    pub coupling_prepointamp: [i32; %d],\n", PACKETBLOBS);
    printf("    pub coupling_postpointamp: [i32; %d],\n", PACKETBLOBS);
    printf("    pub sliding_lowpass: [[i32; %d]; 2],\n", PACKETBLOBS);
    printf("}\n\n");

    printf("/// `adj_stereo`.\n");
    printf("#[derive(Clone, Copy, Debug)]\n");
    printf("pub(crate) struct AdjStereo {\n");
    printf("    pub pre: [i32; %d],\n", PACKETBLOBS);
    printf("    pub post: [i32; %d],\n", PACKETBLOBS);
    printf("    pub khz: [f32; %d],\n", PACKETBLOBS);
    printf("    pub lowpass_khz: [f32; %d],\n", PACKETBLOBS);
    printf("}\n\n");

    printf("/// `att3`.\n");
    printf("#[derive(Clone, Copy, Debug)]\n");
    printf("pub(crate) struct Att3 {\n");
    printf("    pub att: [i32; %d],\n", P_NOISECURVES);
    printf("    pub boost: f32,\n");
    printf("    pub decay: f32,\n");
    printf("}\n\n");

    printf("/// `vp_adjblock`.\n");
    printf("pub(crate) type AdjBlock = [i32; %d];\n", P_BANDS);
    printf("/// `noise3`.\n");
    printf("pub(crate) type Noise3 = [[i32; %d]; %d];\n", P_BANDS, P_NOISECURVES);
    printf("/// `compandblock`.\n");
    printf("pub(crate) type CompandBlock = [i32; %d];\n", NOISE_COMPAND_LEVELS);
    printf("/// `noiseguard` as `[lo, hi, fixed]`.\n");
    printf("pub(crate) type NoiseGuard = [i32; 3];\n\n");

    printf("/// The quality-dependent fields of `ve_setup_data_template` (the setup\n");
    printf("/// header half lives in `setup_templates.rs`). `None` is a NULL pointer.\n");
    printf("pub(crate) struct QualityTemplate {\n");
    printf("    pub name: &'static str,\n");
    printf("    pub mappings: usize,\n");
    printf("    pub rate_mapping: &'static [f64],\n");
    printf("    pub quality_mapping: &'static [f64],\n");
    printf("    pub coupling_restriction: i32,\n");
    printf("    pub samplerate_min_restriction: i64,\n");
    printf("    pub samplerate_max_restriction: i64,\n");
    printf("    pub blocksize_short: &'static [i32],\n");
    printf("    pub blocksize_long: &'static [i32],\n");
    printf("    pub psy_tone_masteratt: &'static [Att3],\n");
    printf("    pub psy_tone_0db: &'static [i32],\n");
    printf("    pub psy_tone_dbsuppress: &'static [i32],\n");
    printf("    pub psy_tone_adj_impulse: &'static [AdjBlock],\n");
    printf("    pub psy_tone_adj_long: Option<&'static [AdjBlock]>,\n");
    printf("    pub psy_tone_adj_other: &'static [AdjBlock],\n");
    printf("    pub psy_noiseguards: &'static [NoiseGuard],\n");
    printf("    pub psy_noise_bias_impulse: &'static [Noise3],\n");
    printf("    pub psy_noise_bias_padding: &'static [Noise3],\n");
    printf("    pub psy_noise_bias_trans: Option<&'static [Noise3]>,\n");
    printf("    pub psy_noise_bias_long: Option<&'static [Noise3]>,\n");
    printf("    pub psy_noise_dbsuppress: &'static [i32],\n");
    printf("    pub psy_noise_compand: &'static [CompandBlock],\n");
    printf("    pub psy_noise_compand_short_mapping: &'static [f64],\n");
    printf("    pub psy_noise_compand_long_mapping: Option<&'static [f64]>,\n");
    printf("    pub psy_noise_normal_start: [&'static [i32]; 2],\n");
    printf("    pub psy_noise_normal_partition: [&'static [i32]; 2],\n");
    printf("    pub psy_noise_normal_thresh: &'static [f64],\n");
    printf("    pub psy_ath_float: &'static [i32],\n");
    printf("    pub psy_ath_abs: &'static [i32],\n");
    printf("    pub psy_lowpass: &'static [f64],\n");
    printf("    pub global_params: &'static [PsyGlobalTemplate],\n");
    printf("    pub global_mapping: &'static [f64],\n");
    printf("    pub stereo_modes: Option<&'static [AdjStereo]>,\n");
    printf("}\n\n");

    printf("/// Scalar fields of `_psy_info_template` that survive setup.\n");
    printf("pub(crate) const PSY_INFO_TEMPLATE_NOISEMASKP: i32 = %d;\n", pt->noisemaskp);
    printf("pub(crate) const PSY_INFO_TEMPLATE_NOISEWINDOWLO: f32 = ");
    pf32(pt->noisewindowlo);
    printf(";\npub(crate) const PSY_INFO_TEMPLATE_NOISEWINDOWHI: f32 = ");
    pf32(pt->noisewindowhi);
    printf(";\n\n");

    for (k = 0; k < NK; k++)
        for (i = 0; i < nregs[k]; i++)
            if (regs[k][i].used) emit_table(k, &regs[k][i]);

    for (t = 0; t < NTEMPLATES; t++) emit_template(t);

    printf("/// vorbisenc.c `setup_list` without `ve_setup_44_51` (6 channels), in\n");
    printf("/// list order: `get_setup_template` returns the first match.\n");
    printf("pub(crate) static SETUP_LIST: [&QualityTemplate; %d] = [\n", NTEMPLATES);
    for (t = 0; t < NTEMPLATES; t++) printf("    &SETUP_%s,\n", template_names[t]);
    printf("];\n");
    return 0;
}
