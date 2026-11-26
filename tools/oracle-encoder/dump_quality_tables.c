/*
 * dump_quality_tables — prints the libvorbis 1.3.7 44.1/48 kHz mode-template
 * tables that drive quality-dependent psy / global / ID-header setup, as Rust
 * source on stdout. The output is src/tables/quality.rs.
 *
 * Why generate instead of transcribing: psych_44.h contains rows with
 * missing commas (e.g. `-7  -3` parses as the single value -10), and the
 * Q5 output depends on that. Compiling the real headers is the only way to
 * be sure the Rust tables hold the values libvorbis actually uses.
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
#include "vorbisenc.c"

static void pf32(float v) {
    /* %.9g always round-trips an IEEE single. Append the suffix so integral
     * values (e.g. "12") still parse as float literals. */
    printf("%.9g_f32", (double)v);
}
static void pf64(double v) { printf("%.17g_f64", v); }

static void arr_i32(const char *name, const int *a, int n) {
    int i;
    printf("pub(crate) const %s: [i32; %d] = [", name, n);
    for (i = 0; i < n; i++) printf("%s%d", i ? ", " : "", a[i]);
    printf("];\n\n");
}
static void arr_f64(const char *name, const double *a, int n) {
    int i;
    printf("pub(crate) const %s: [f64; %d] = [", name, n);
    for (i = 0; i < n; i++) {
        if (i) printf(", ");
        pf64(a[i]);
    }
    printf("];\n\n");
}

#define N(a) ((int)(sizeof(a) / sizeof((a)[0])))

static void adj(const char *name, const vp_adjblock *a, int n) {
    int i, k;
    printf("pub(crate) const %s: [[i32; %d]; %d] = [\n", name, P_BANDS, n);
    for (i = 0; i < n; i++) {
        printf("    [");
        for (k = 0; k < P_BANDS; k++) printf("%s%d", k ? ", " : "", a[i].block[k]);
        printf("],\n");
    }
    printf("];\n\n");
}

static void noise(const char *name, const noise3 *a, int n) {
    int i, j, k;
    printf("pub(crate) const %s: [[[i32; %d]; %d]; %d] = [\n", name, P_BANDS,
           P_NOISECURVES, n);
    for (i = 0; i < n; i++) {
        printf("    [\n");
        for (j = 0; j < P_NOISECURVES; j++) {
            printf("        [");
            for (k = 0; k < P_BANDS; k++)
                printf("%s%d", k ? ", " : "", a[i].data[j][k]);
            printf("],\n");
        }
        printf("    ],\n");
    }
    printf("];\n\n");
}

int main(void) {
    int i, k;
    const ve_setup_data_template *st = &ve_setup_44_stereo;
    const ve_setup_data_template *un = &ve_setup_44_uncoupled;

    /* Sanity: the stereo and uncoupled 44 kHz templates share every psy
     * table; only rate_mapping (and the residue/mapping templates) differ.
     * The Rust side relies on this, so fail loudly if it ever changes. */
    if (st->quality_mapping != un->quality_mapping ||
        st->blocksize_short != un->blocksize_short ||
        st->blocksize_long != un->blocksize_long ||
        st->psy_tone_masteratt != un->psy_tone_masteratt ||
        st->psy_tone_0dB != un->psy_tone_0dB ||
        st->psy_tone_dBsuppress != un->psy_tone_dBsuppress ||
        st->psy_tone_adj_impulse != un->psy_tone_adj_impulse ||
        st->psy_tone_adj_long != un->psy_tone_adj_long ||
        st->psy_tone_adj_other != un->psy_tone_adj_other ||
        st->psy_noiseguards != un->psy_noiseguards ||
        st->psy_noise_bias_impulse != un->psy_noise_bias_impulse ||
        st->psy_noise_bias_padding != un->psy_noise_bias_padding ||
        st->psy_noise_bias_trans != un->psy_noise_bias_trans ||
        st->psy_noise_bias_long != un->psy_noise_bias_long ||
        st->psy_noise_dBsuppress != un->psy_noise_dBsuppress ||
        st->psy_noise_compand != un->psy_noise_compand ||
        st->psy_noise_compand_short_mapping != un->psy_noise_compand_short_mapping ||
        st->psy_noise_compand_long_mapping != un->psy_noise_compand_long_mapping ||
        st->psy_noise_normal_start[0] != un->psy_noise_normal_start[0] ||
        st->psy_noise_normal_start[1] != un->psy_noise_normal_start[1] ||
        st->psy_noise_normal_partition[0] != un->psy_noise_normal_partition[0] ||
        st->psy_noise_normal_partition[1] != un->psy_noise_normal_partition[1] ||
        st->psy_noise_normal_thresh != un->psy_noise_normal_thresh ||
        st->psy_ath_float != un->psy_ath_float || st->psy_ath_abs != un->psy_ath_abs ||
        st->psy_lowpass != un->psy_lowpass || st->global_params != un->global_params ||
        st->global_mapping != un->global_mapping || st->stereo_modes != un->stereo_modes ||
        st->mappings != un->mappings) {
        fprintf(stderr, "dump_quality_tables: stereo/uncoupled templates diverge\n");
        return 1;
    }
    /* The Rust side assumes this wiring of the per-block tables. */
    if (st->psy_tone_adj_impulse != _vp_tonemask_adj_otherblock ||
        st->psy_tone_adj_other != _vp_tonemask_adj_otherblock ||
        st->psy_tone_adj_long != _vp_tonemask_adj_longblock) {
        fprintf(stderr, "dump_quality_tables: unexpected tonemask adj wiring\n");
        return 1;
    }

    printf("// AUTO-GENERATED by tools/oracle-encoder/dump_quality_tables.c from the\n");
    printf("// vendored libvorbis 1.3.7 modes/{setup_44,setup_44u,psych_44}.h.\n");
    printf("// Do not edit. Regenerate via `just regen-quality-tables`.\n");
    printf("//\n");
    printf("// Tables shared by ve_setup_44_stereo and ve_setup_44_uncoupled (the\n");
    printf("// generator asserts they share them). Names mirror the C identifiers.\n");
    printf("#![allow(clippy::excessive_precision)]\n");
    printf("#![allow(clippy::unreadable_literal)]\n\n");

    printf("/// `ve_setup_44_*.mappings`.\n");
    printf("pub(crate) const MAPPINGS: usize = %d;\n\n", st->mappings);
    printf("/// `ve_setup_44_*.samplerate_{min,max}_restriction`.\n");
    printf("pub(crate) const SAMPLERATE_MIN: u32 = %ld;\n", st->samplerate_min_restriction);
    printf("pub(crate) const SAMPLERATE_MAX: u32 = %ld;\n\n", st->samplerate_max_restriction);

    arr_f64("QUALITY_MAPPING_44", quality_mapping_44, N(quality_mapping_44));
    arr_f64("RATE_MAPPING_44_STEREO", rate_mapping_44_stereo, N(rate_mapping_44_stereo));
    arr_f64("RATE_MAPPING_44_UN", rate_mapping_44_un, N(rate_mapping_44_un));
    arr_i32("BLOCKSIZE_SHORT_44", blocksize_short_44, N(blocksize_short_44));
    arr_i32("BLOCKSIZE_LONG_44", blocksize_long_44, N(blocksize_long_44));
    arr_f64("PSY_COMPAND_SHORT_MAPPING", _psy_compand_short_mapping,
            N(_psy_compand_short_mapping));
    arr_f64("PSY_COMPAND_LONG_MAPPING", _psy_compand_long_mapping,
            N(_psy_compand_long_mapping));
    arr_f64("GLOBAL_MAPPING_44", _global_mapping_44, N(_global_mapping_44));

    /* _psy_global_44: only the fields global_psych_setup leaves alive (the
     * coupling/lowpass arrays are always overwritten by global_stereo). */
    printf("/// `_psy_global_44` (fields not overwritten by global_stereo).\n");
    printf("#[derive(Clone, Copy, Debug)]\n");
    printf("pub(crate) struct PsyGlobalTemplate {\n");
    printf("    pub eighth_octave_lines: i32,\n");
    printf("    pub preecho_thresh: [f32; %d],\n", VE_BANDS);
    printf("    pub postecho_thresh: [f32; %d],\n", VE_BANDS);
    printf("    pub stretch_penalty: f32,\n");
    printf("    pub preecho_minenergy: f32,\n");
    printf("    pub ampmax_att_per_sec: f32,\n");
    printf("}\n\n");
    printf("pub(crate) const PSY_GLOBAL_44: [PsyGlobalTemplate; %d] = [\n", N(_psy_global_44));
    for (i = 0; i < N(_psy_global_44); i++) {
        const vorbis_info_psy_global *g = &_psy_global_44[i];
        printf("    PsyGlobalTemplate {\n");
        printf("        eighth_octave_lines: %d,\n", g->eighth_octave_lines);
        printf("        preecho_thresh: [");
        for (k = 0; k < VE_BANDS; k++) {
            if (k) printf(", ");
            pf32(g->preecho_thresh[k]);
        }
        printf("],\n        postecho_thresh: [");
        for (k = 0; k < VE_BANDS; k++) {
            if (k) printf(", ");
            pf32(g->postecho_thresh[k]);
        }
        printf("],\n        stretch_penalty: ");
        pf32(g->stretch_penalty);
        printf(",\n        preecho_minenergy: ");
        pf32(g->preecho_minenergy);
        printf(",\n        ampmax_att_per_sec: ");
        pf32(g->ampmax_att_per_sec);
        printf(",\n    },\n");
    }
    printf("];\n\n");

    printf("/// `_psy_compand_44[i].data`.\n");
    printf("pub(crate) const PSY_COMPAND_44: [[i32; %d]; %d] = [\n", NOISE_COMPAND_LEVELS,
           N(_psy_compand_44));
    for (i = 0; i < N(_psy_compand_44); i++) {
        printf("    [");
        for (k = 0; k < NOISE_COMPAND_LEVELS; k++)
            printf("%s%d", k ? ", " : "", _psy_compand_44[i].data[k]);
        printf("],\n");
    }
    printf("];\n\n");

    adj("VP_TONEMASK_ADJ_LONGBLOCK", _vp_tonemask_adj_longblock,
        N(_vp_tonemask_adj_longblock));
    adj("VP_TONEMASK_ADJ_OTHERBLOCK", _vp_tonemask_adj_otherblock,
        N(_vp_tonemask_adj_otherblock));

    noise("PSY_NOISEBIAS_TRANS", _psy_noisebias_trans, N(_psy_noisebias_trans));
    noise("PSY_NOISEBIAS_LONG", _psy_noisebias_long, N(_psy_noisebias_long));
    noise("PSY_NOISEBIAS_IMPULSE", _psy_noisebias_impulse, N(_psy_noisebias_impulse));
    noise("PSY_NOISEBIAS_PADDING", _psy_noisebias_padding, N(_psy_noisebias_padding));

    printf("/// `_psy_noiseguards_44[block]` as `[lo, hi, fixed]`.\n");
    printf("pub(crate) const PSY_NOISEGUARDS_44: [[i32; 3]; %d] = [\n", N(_psy_noiseguards_44));
    for (i = 0; i < N(_psy_noiseguards_44); i++)
        printf("    [%d, %d, %d],\n", _psy_noiseguards_44[i].lo, _psy_noiseguards_44[i].hi,
               _psy_noiseguards_44[i].fixed);
    printf("];\n\n");

    arr_i32("PSY_TONE_SUPPRESS", _psy_tone_suppress, N(_psy_tone_suppress));
    arr_i32("PSY_TONE_0DB", _psy_tone_0dB, N(_psy_tone_0dB));
    arr_i32("PSY_NOISE_SUPPRESS", _psy_noise_suppress, N(_psy_noise_suppress));

    {
        const vorbis_info_psy *t = &_psy_info_template;
        printf("/// Scalar fields of `_psy_info_template` that survive setup.\n");
        printf("pub(crate) const PSY_INFO_TEMPLATE_NOISEMASKP: i32 = %d;\n", t->noisemaskp);
        printf("pub(crate) const PSY_INFO_TEMPLATE_NOISEWINDOWLO: f32 = ");
        pf32(t->noisewindowlo);
        printf(";\npub(crate) const PSY_INFO_TEMPLATE_NOISEWINDOWHI: f32 = ");
        pf32(t->noisewindowhi);
        printf(";\n\n");
    }

    arr_i32("PSY_ATH_FLOATER", _psy_ath_floater, N(_psy_ath_floater));
    arr_i32("PSY_ATH_ABS", _psy_ath_abs, N(_psy_ath_abs));

    printf("/// `_psy_stereo_modes_44[i]`.\n");
    printf("#[derive(Clone, Copy, Debug)]\n");
    printf("pub(crate) struct AdjStereo {\n");
    printf("    pub pre: [i32; %d],\n", PACKETBLOBS);
    printf("    pub post: [i32; %d],\n", PACKETBLOBS);
    printf("    pub khz: [f32; %d],\n", PACKETBLOBS);
    printf("    pub lowpass_khz: [f32; %d],\n", PACKETBLOBS);
    printf("}\n\n");
    printf("pub(crate) const PSY_STEREO_MODES_44: [AdjStereo; %d] = [\n",
           N(_psy_stereo_modes_44));
    for (i = 0; i < N(_psy_stereo_modes_44); i++) {
        const adj_stereo *s = &_psy_stereo_modes_44[i];
        printf("    AdjStereo {\n        pre: [");
        for (k = 0; k < PACKETBLOBS; k++) printf("%s%d", k ? ", " : "", s->pre[k]);
        printf("],\n        post: [");
        for (k = 0; k < PACKETBLOBS; k++) printf("%s%d", k ? ", " : "", s->post[k]);
        printf("],\n        khz: [");
        for (k = 0; k < PACKETBLOBS; k++) {
            if (k) printf(", ");
            pf32(s->kHz[k]);
        }
        printf("],\n        lowpass_khz: [");
        for (k = 0; k < PACKETBLOBS; k++) {
            if (k) printf(", ");
            pf32(s->lowpasskHz[k]);
        }
        printf("],\n    },\n");
    }
    printf("];\n\n");

    printf("/// `_psy_tone_masteratt_44[i]`.\n");
    printf("#[derive(Clone, Copy, Debug)]\n");
    printf("pub(crate) struct Att3 {\n");
    printf("    pub att: [i32; %d],\n", P_NOISECURVES);
    printf("    pub boost: f32,\n");
    printf("    pub decay: f32,\n");
    printf("}\n\n");
    printf("pub(crate) const PSY_TONE_MASTERATT_44: [Att3; %d] = [\n",
           N(_psy_tone_masteratt_44));
    for (i = 0; i < N(_psy_tone_masteratt_44); i++) {
        const att3 *a = &_psy_tone_masteratt_44[i];
        printf("    Att3 {\n        att: [%d, %d, %d],\n        boost: ", a->att[0], a->att[1],
               a->att[2]);
        pf32(a->boost);
        printf(",\n        decay: ");
        pf32(a->decay);
        printf(",\n    },\n");
    }
    printf("];\n\n");

    arr_f64("PSY_LOWPASS_44", _psy_lowpass_44, N(_psy_lowpass_44));
    arr_i32("NOISE_START_SHORT_44", _noise_start_short_44, N(_noise_start_short_44));
    arr_i32("NOISE_START_LONG_44", _noise_start_long_44, N(_noise_start_long_44));
    arr_i32("NOISE_PART_SHORT_44", _noise_part_short_44, N(_noise_part_short_44));
    arr_i32("NOISE_PART_LONG_44", _noise_part_long_44, N(_noise_part_long_44));
    arr_f64("NOISE_THRESH_44", _noise_thresh_44, N(_noise_thresh_44));

    return 0;
}
