//! Quality-dependent encoder parameters, derived bit-exactly the way
//! libvorbis 1.3.7 `vorbis_encode_init_vbr()` derives them from the mono
//! and stereo mode templates (`ve_setup_{44,32,22,16,11,8,X,XX}_{stereo,
//! uncoupled}`; the tables are generated into `tables/quality.rs`).
//!
//! Entry point: [`quality_config`]`(base_quality, rate, channels)`.
//!
//! # `QualityConfig` fields
//!
//! | field             | libvorbis source                                         |
//! |-------------------|----------------------------------------------------------|
//! | `base_setting`    | `ci->hi.base_setting` (f32-valued, stored as f64)        |
//! | `is`, `ds`        | `(int)base_setting`, `base_setting - is`                 |
//! | `blocksizes`      | `ci->blocksizes[0..2]` (short, long)                     |
//! | `bitrate_nominal` | `vi->bitrate_nominal` (ID header); upper/lower are 0     |
//! | `lowpass_khz`     | `ci->hi.lowpass_kHz`                                     |
//! | `psy_global`      | `ci->psy_g_param`                                        |
//! | `psys`, `psy`     | `ci->psys`, `ci->psy_param[0..psys]`: 0 impulse,         |
//! |                   | 1 padding (short), 2 transition, 3 long                  |
//!
//! `base_setting` and `lowpass_khz` are what the setup-header generator
//! consumes (floor/residue/mapping template selection and residue `end`).
//!
//! # Arithmetic conventions (all mirror the C exactly; never use `mul_add`)
//!
//! * `quality += .0000001` is done in f64 and stored back to f32; quality
//!   `>= 1.0` clamps to `.9999f`.
//! * `get_setup_template` computes `del` with `float low, high`, and
//!   `base_setting = j + del` is an **f32** add, widened to f64.
//! * Linear interpolation (`lerp`): `a*(1.-ds) + b*ds` in f64, rounded once
//!   to the destination type.
//! * Remapped interpolation (`remap`, used by global psych and compand): the
//!   setting is first mapped through a `double` table, then re-split, with
//!   the `ds==0 && is>0 → is--, ds=1` quirk.
//! * `global_stereo` truncates the setting to f32 first; `kHz` is an f32.
//!
//! Every field is verified bit-for-bit against `tools/oracle-encoder/dump_params`
//! by the oracle-gated test `quality_oracle_grid`.

// Index loops deliberately mirror the C for side-by-side review.
#![allow(clippy::needless_range_loop)]

use crate::psy::{
    NOISE_COMPAND_LEVELS, P_BANDS, P_NOISECURVES, PACKETBLOBS, VE_BANDS, VorbisInfoPsy,
    VorbisInfoPsyGlobal,
};
use crate::tables::quality::{self as t, QualityTemplate};

/// Lowest `base_quality` libvorbis accepts (after its `+1e-7` nudge, the
/// request must be `>= quality_mapping[0] = -0.1`; every template starts
/// its quality mapping there and ends it at 1.0, see the tests).
pub(crate) const MIN_BASE_QUALITY: f32 = -0.1;

/// Lowest and highest sample rate any 1-2 channel template accepts
/// (`ve_setup_XX_*` starts at 0, but `vorbis_encode_setup_vbr` rejects
/// `rate <= 0`; `ve_setup_X_*` ends at 200 kHz).
pub(crate) const MIN_RATE: u32 = 1;
pub(crate) const MAX_RATE: u32 = 200_000;

/// Everything quality-dependent that the encoder needs outside the setup
/// header itself. See the module docs for field provenance.
#[derive(Clone, Debug)]
pub(crate) struct QualityConfig {
    /// Index of the selected template in [`t::SETUP_LIST`] (and
    /// `setup_templates::SETUP_LIST`).
    pub template: usize,
    /// `hi->base_setting`: template position in `0.0..mappings`.
    pub base_setting: f64,
    /// `(int)base_setting` (diagnostic; the encoder only needs base_setting).
    #[allow(dead_code)]
    pub is: usize,
    /// `base_setting - is`.
    #[allow(dead_code)]
    pub ds: f64,
    /// `[short, long]` block sizes.
    pub blocksizes: [usize; 2],
    /// ID-header nominal bitrate.
    /// (The encoder writes `ratecontrol::RatePlan::header_bitrates`.)
    #[allow(dead_code)]
    pub bitrate_nominal: i32,
    /// `hi->lowpass_kHz` (may be 999 for "no lowpass").
    pub lowpass_khz: f64,
    pub psy_global: VorbisInfoPsyGlobal,
    /// `ci->psys`: 4, or 2 when the short and long blocks are the same
    /// size (libvorbis then sets up only the impulse and padding psys).
    /// Only the oracle comparison reads it; the encoder never uses block
    /// types 2 and 3 then.
    #[allow(dead_code)]
    pub psys: usize,
    /// Indexed by libvorbis block type: 0 impulse, 1 padding, 2 transition,
    /// 3 long. With `psys == 2`, entries 2 and 3 are copies of entry 1
    /// (only block types 0 and 1 occur).
    pub psy: [VorbisInfoPsy; 4],
}

#[inline]
fn lerp(a: f64, b: f64, ds: f64) -> f64 {
    a * (1. - ds) + b * ds
}

/// libvorbis' two-level remap: `ds = x[is]*(1-ds)+x[is+1]*ds; is=(int)ds; ...`.
fn remap(s: f64, x: &[f64]) -> (usize, f64) {
    let is = s as usize;
    let ds = s - is as f64;
    let mut ds = x[is] * (1. - ds) + x[is + 1] * ds;
    let mut is = ds as usize;
    ds -= is as f64;
    if ds == 0.0 && is > 0 {
        is -= 1;
        ds = 1.;
    }
    (is, ds)
}

/// `vorbis_encode_setup_vbr` quality nudge + `get_setup_template` lookup:
/// the index of the selected template in [`t::SETUP_LIST`] and
/// `base_setting`, or `None` if libvorbis would return `OV_EIMPL` (no
/// template for this rate, channel count and quality).
///
/// One input is not libvorbis's: see [`is_libvorbis_oob`]. There the
/// request is treated as libvorbis's own top clamp (`.9999`, i.e. q10).
pub(crate) fn select_template(
    base_quality: f32,
    rate: u32,
    channels: usize,
) -> Option<(usize, f64)> {
    let (i, s) = get_setup_template(nudged_request(base_quality)?, rate, channels)?;
    if s as usize >= t::SETUP_LIST[i].mappings {
        return get_setup_template(0.9999_f64 as f32 as f64, rate, channels);
    }
    Some((i, s))
}

/// True when libvorbis 1.3.7 itself reads its mode tables out of bounds
/// for this request, so its output is undefined (it depends on whatever
/// the compiler placed after the tables).
///
/// `get_setup_template` computes `base_setting = j + del` in float. For
/// the templates whose top mapping interval is wider than ~0.5 (8, 11,
/// 16, 22 kHz and below 8 kHz) and the single request `0.99999994`
/// (`base_quality` `0x3f7ffffd`, ~0.99999982 before the `+1e-7` nudge;
/// oggenc `-q 9.999998`), that sum rounds up to `mappings`, one past the
/// last setting, and every `table[is]` / `table[is+1]` lookup that
/// follows overruns its array. AddressSanitizer confirms the read
/// (`vorbis_encode_setup_setting`, `_psy_lowpass_8[3]`). The 44.1 kHz-style
/// templates, with interval 0.1, are not affected.
#[cfg(test)]
pub(crate) fn is_libvorbis_oob(base_quality: f32, rate: u32, channels: usize) -> bool {
    nudged_request(base_quality)
        .and_then(|req| get_setup_template(req, rate, channels))
        .is_some_and(|(i, s)| s as usize >= t::SETUP_LIST[i].mappings)
}

/// `vorbis_encode_setup_vbr`: `quality += .0000001` (f32), clamped to
/// `.9999` from 1.0 up; `None` for NaN, which C would let fall through to
/// `base_setting = mappings - .001`.
fn nudged_request(base_quality: f32) -> Option<f64> {
    let mut quality = (base_quality as f64 + 0.0000001) as f32;
    if quality as f64 >= 1. {
        quality = 0.9999_f64 as f32;
    }
    let req = quality as f64;
    (!req.is_nan()).then_some(req)
}

/// `get_setup_template` (quality mode): the first template in
/// `setup_list` whose channel restriction, rate range and quality range
/// all match, and the `base_setting` within it.
fn get_setup_template(req: f64, rate: u32, channels: usize) -> Option<(usize, f64)> {
    get_setup_template_by(req, rate, channels as i64, false)
}

/// `get_setup_template` in bitrate mode (`q_or_bitrate = 1`), as
/// `vorbis_encode_setup_managed` calls it: `nominal` bits per second,
/// divided by `ch` and looked up in each template's `rate_mapping`. `ch`
/// is the channel count, or -1 for libvorbis's `OV_ECTL_COUPLING_SET`
/// re-fetch with coupling off (only unrestricted templates match then,
/// and the negative request matches none of them). `None` is libvorbis's
/// `OV_EIMPL`. The catch-all `X`/`XX` templates have an all -1
/// `rate_mapping`, so only 8-50 kHz can match.
pub(crate) fn select_template_bitrate(nominal: f64, rate: u32, ch: i64) -> Option<(usize, f64)> {
    let (i, s) = get_setup_template_by(nominal, rate, ch, true)?;
    // `j + del` in float can only round up to `mappings` for requests
    // within half a float ulp of a mapping's top; a bitrate request (a
    // whole number of bits per second, over 1 or 2 channels) is never
    // that close (see the tests).
    debug_assert!((s as usize) < t::SETUP_LIST[i].mappings);
    Some((i, s))
}

fn get_setup_template_by(req: f64, rate: u32, ch: i64, q_or_bitrate: bool) -> Option<(usize, f64)> {
    let srate = rate as i64;
    let req = if q_or_bitrate { req / ch as f64 } else { req };
    for (i, tpl) in t::SETUP_LIST.iter().enumerate() {
        if tpl.coupling_restriction != -1 && tpl.coupling_restriction as i64 != ch {
            continue;
        }
        if srate < tpl.samplerate_min_restriction || srate > tpl.samplerate_max_restriction {
            continue;
        }
        let mappings = tpl.mappings;
        let map = if q_or_bitrate {
            tpl.rate_mapping
        } else {
            tpl.quality_mapping
        };
        if req < map[0] || req > map[mappings] {
            continue;
        }
        let mut j = 0;
        while j < mappings {
            if req >= map[j] && req < map[j + 1] {
                break;
            }
            j += 1;
        }
        if j == mappings {
            return Some((i, j as f64 - 0.001));
        }
        let low = map[j] as f32;
        let high = map[j + 1] as f32;
        let del = ((req - low as f64) / (high - low) as f64) as f32;
        return Some((i, (j as f32 + del) as f64));
    }
    None
}

/// Derive all quality-dependent parameters for `base_quality` (libvorbis
/// scale: -0.1 = q-1 .. 1.0 = q10; fractional allowed), `rate` in
/// `MIN_RATE..=MAX_RATE` Hz and 1 or 2 channels.
///
/// Panics outside that domain (where libvorbis' `vorbis_encode_init_vbr`
/// would fail).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn quality_config(base_quality: f32, rate: u32, channels: usize) -> QualityConfig {
    let (template, base_setting) =
        select_template(base_quality, rate, channels).unwrap_or_else(|| {
            panic!(
                "quality_config: no libvorbis template for q={base_quality} {rate} Hz {channels} ch"
            )
        });
    setting_config(
        template,
        base_setting,
        rate,
        channels,
        &HighLevel::default(),
    )
}

/// The `highlevel_encode_setup` fields besides the template and
/// `base_setting` that `vorbis_encode_setup_init` reads. The default is
/// what `vorbis_encode_setup_vbr` leaves.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct HighLevel {
    /// `hi->managed`: per-packetblob stereo setup (and, in `setup_gen`, the
    /// managed residue books).
    pub managed: bool,
}

/// [`quality_config`] for an already selected template and
/// `hi->base_setting` (from the quality or, in bitrate mode, the
/// `rate_mapping`), with the given high-level flags.
pub(crate) fn setting_config(
    template: usize,
    base_setting: f64,
    rate: u32,
    channels: usize,
    hl: &HighLevel,
) -> QualityConfig {
    assert!(
        channels == 1 || channels == 2,
        "quality_config: {channels} channels unsupported"
    );
    let tpl = t::SETUP_LIST[template];
    let s = base_setting;
    let is = s as usize;
    let ds = s - is as f64;

    // vorbis_encode_blocksize_setup
    let blocksizes = [
        tpl.blocksize_short[is] as usize,
        tpl.blocksize_long[is] as usize,
    ];
    let singleblock = blocksizes[0] == blocksizes[1];

    // vorbis_encode_setup_setting (hi->*)
    let lowpass_khz = lerp(tpl.psy_lowpass[is], tpl.psy_lowpass[is + 1], ds);
    let mut ath_floating_db = lerp(
        tpl.psy_ath_float[is] as f64,
        tpl.psy_ath_float[is + 1] as f64,
        ds,
    );
    let ath_absolute_db = lerp(
        tpl.psy_ath_abs[is] as f64,
        tpl.psy_ath_abs[is + 1] as f64,
        ds,
    );
    let amplitude_track_db_per_sec = -6.0_f64;
    // vorbis_encode_setup_init clamps
    ath_floating_db = ath_floating_db.clamp(-200., -80.);

    let psy_global = global_psych(
        tpl,
        s,
        blocksizes,
        rate,
        amplitude_track_db_per_sec,
        hl.managed,
    );

    let block = |b: usize| block_psy(tpl, s, b, ath_floating_db as f32, ath_absolute_db as f32);
    let (psys, psy) = if singleblock {
        let p1 = block(1);
        (2, [block(0), p1.clone(), p1.clone(), p1])
    } else {
        (4, std::array::from_fn(block))
    };

    // setting_to_approx_bitrate
    let r = tpl.rate_mapping;
    let bitrate_nominal = (lerp(r[is], r[is + 1], ds) * channels as f64) as i64 as i32;

    QualityConfig {
        template,
        base_setting,
        is,
        ds,
        blocksizes,
        bitrate_nominal,
        lowpass_khz,
        psy_global,
        psys,
        psy,
    }
}

/// `vorbis_encode_global_psych_setup` + `vorbis_encode_global_stereo`
/// (VBR branch, or the bitrate-managed one with `managed`).
fn global_psych(
    tpl: &QualityTemplate,
    s: f64,
    blocksizes: [usize; 2],
    rate: u32,
    amplitude_track: f64,
    managed: bool,
) -> VorbisInfoPsyGlobal {
    let x = tpl.global_mapping;
    let tmpl = tpl.global_params;
    // memcpy(g, in + (int)x[is], sizeof(*g))
    let base = &tmpl[x[s as usize] as usize];

    let mut g = VorbisInfoPsyGlobal {
        eighth_octave_lines: base.eighth_octave_lines,
        preecho_thresh: base.preecho_thresh,
        postecho_thresh: base.postecho_thresh,
        stretch_penalty: base.stretch_penalty,
        preecho_minenergy: base.preecho_minenergy,
        ampmax_att_per_sec: base.ampmax_att_per_sec,
        coupling_pkHz: base.coupling_pkhz,
        coupling_pointlimit: base.coupling_pointlimit,
        coupling_prepointamp: base.coupling_prepointamp,
        coupling_postpointamp: base.coupling_postpointamp,
        sliding_lowpass: base.sliding_lowpass,
    };
    let (is, ds) = remap(s, x);
    for i in 0..4 {
        g.preecho_thresh[i] = lerp(
            tmpl[is].preecho_thresh[i] as f64,
            tmpl[is + 1].preecho_thresh[i] as f64,
            ds,
        ) as f32;
        g.postecho_thresh[i] = lerp(
            tmpl[is].postecho_thresh[i] as f64,
            tmpl[is + 1].postecho_thresh[i] as f64,
            ds,
        ) as f32;
    }
    g.ampmax_att_per_sec = amplitude_track as f32;
    debug_assert_eq!(g.preecho_thresh.len(), VE_BANDS);

    let Some(p) = tpl.stereo_modes else {
        // No stereo modes (32 kHz and catch-all uncoupled templates): the
        // coupling fields keep the global template's values.
        for i in 0..PACKETBLOBS {
            g.sliding_lowpass[0][i] = blocksizes[0] as i32;
            g.sliding_lowpass[1][i] = blocksizes[1] as i32;
        }
        return g;
    };

    // global_stereo: `float s = hi->stereo_point_setting` (== base_setting).
    let s = s as f32;
    let is = s as usize;
    let ds = s as f64 - is as f64;
    g.coupling_prepointamp = p[is].pre;
    g.coupling_postpointamp = p[is].post;

    let mid = PACKETBLOBS / 2;
    let point = |khz: f32, bs: usize| (khz as f64 * 1000. / rate as f64 * bs as f64) as i32;

    if managed {
        // hi->managed: every packetblob interpolates its own thresholds.
        for i in 0..PACKETBLOBS {
            let khz = lerp(p[is].khz[i] as f64, p[is + 1].khz[i] as f64, ds) as f32;
            g.coupling_pointlimit[0][i] = point(khz, blocksizes[0]);
            g.coupling_pointlimit[1][i] = point(khz, blocksizes[1]);
            g.coupling_pkHz[i] = khz as i32;
            let khz = lerp(
                p[is].lowpass_khz[i] as f64,
                p[is + 1].lowpass_khz[i] as f64,
                ds,
            ) as f32;
            g.sliding_lowpass[0][i] = point(khz, blocksizes[0]);
            g.sliding_lowpass[1][i] = point(khz, blocksizes[1]);
        }
        return g;
    }

    let khz = lerp(p[is].khz[mid] as f64, p[is + 1].khz[mid] as f64, ds) as f32;
    for i in 0..PACKETBLOBS {
        g.coupling_pointlimit[0][i] = point(khz, blocksizes[0]);
        g.coupling_pointlimit[1][i] = point(khz, blocksizes[1]);
        g.coupling_pkHz[i] = khz as i32;
    }
    let khz = lerp(
        p[is].lowpass_khz[mid] as f64,
        p[is + 1].lowpass_khz[mid] as f64,
        ds,
    ) as f32;
    for i in 0..PACKETBLOBS {
        g.sliding_lowpass[0][i] = point(khz, blocksizes[0]);
        g.sliding_lowpass[1][i] = point(khz, blocksizes[1]);
    }
    g
}

/// A table only blocks 2 and 3 use. Those are only set up for two-block
/// templates, which have every table (dump_quality_tables.c checks).
fn long_table<T>(tpl: &QualityTemplate, table: Option<T>) -> T {
    table.unwrap_or_else(|| panic!("template {} lacks a long-block table", tpl.name))
}

/// psyset + tonemask + compand + peak + noisebias + ath setup for one block
/// type. All four per-block settings equal `base_setting` in VBR mode.
fn block_psy(
    tpl: &QualityTemplate,
    s: f64,
    block: usize,
    ath_adjatt: f32,
    ath_maxatt: f32,
) -> VorbisInfoPsy {
    let is = s as usize;
    let ds = s - is as f64;
    let long = block >= 2;
    let nn = long as usize;

    // vorbis_encode_psyset_setup (memcpy _psy_info_template; noise_normalize_p=1)
    let mut p = VorbisInfoPsy {
        blockflag: (block >> 1) as i32,
        noisemaskp: t::PSY_INFO_TEMPLATE_NOISEMASKP,
        noisewindowlo: t::PSY_INFO_TEMPLATE_NOISEWINDOWLO,
        noisewindowhi: t::PSY_INFO_TEMPLATE_NOISEWINDOWHI,
        normal_p: 1,
        normal_start: tpl.psy_noise_normal_start[nn][is],
        normal_partition: tpl.psy_noise_normal_partition[nn][is],
        normal_thresh: tpl.psy_noise_normal_thresh[is],
        ..Default::default()
    };

    // vorbis_encode_tonemask_setup
    let att = tpl.psy_tone_masteratt;
    for k in 0..P_NOISECURVES {
        p.tone_masteratt[k] = lerp(att[is].att[k] as f64, att[is + 1].att[k] as f64, ds) as f32;
    }
    p.tone_centerboost = lerp(att[is].boost as f64, att[is + 1].boost as f64, ds) as f32;
    p.tone_decay = lerp(att[is].decay as f64, att[is + 1].decay as f64, ds) as f32;
    p.max_curve_dB = lerp(
        tpl.psy_tone_0db[is] as f64,
        tpl.psy_tone_0db[is + 1] as f64,
        ds,
    ) as f32;
    let adj = match block {
        0 => tpl.psy_tone_adj_impulse,
        3 => long_table(tpl, tpl.psy_tone_adj_long),
        _ => tpl.psy_tone_adj_other,
    };
    for i in 0..P_BANDS {
        p.toneatt[i] = lerp(adj[is][i] as f64, adj[is + 1][i] as f64, ds) as f32;
    }

    // vorbis_encode_compand_setup
    let (cis, cds) = remap(
        s,
        if long {
            long_table(tpl, tpl.psy_noise_compand_long_mapping)
        } else {
            tpl.psy_noise_compand_short_mapping
        },
    );
    let c = tpl.psy_noise_compand;
    for i in 0..NOISE_COMPAND_LEVELS {
        p.noisecompand[i] = lerp(c[cis][i] as f64, c[cis + 1][i] as f64, cds) as f32;
    }

    // vorbis_encode_peak_setup
    p.tone_abs_limit = lerp(
        tpl.psy_tone_dbsuppress[is] as f64,
        tpl.psy_tone_dbsuppress[is + 1] as f64,
        ds,
    ) as f32;

    // vorbis_encode_noisebias_setup (userbias = impulse_noisetune = 0 for
    // block 0, literal 0 otherwise)
    p.noisemaxsupp = lerp(
        tpl.psy_noise_dbsuppress[is] as f64,
        tpl.psy_noise_dbsuppress[is + 1] as f64,
        ds,
    ) as f32;
    let guard = tpl.psy_noiseguards[block];
    p.noisewindowlomin = guard[0];
    p.noisewindowhimin = guard[1];
    p.noisewindowfixed = guard[2];
    let bias = match block {
        0 => tpl.psy_noise_bias_impulse,
        1 => tpl.psy_noise_bias_padding,
        2 => long_table(tpl, tpl.psy_noise_bias_trans),
        _ => long_table(tpl, tpl.psy_noise_bias_long),
    };
    for j in 0..P_NOISECURVES {
        for i in 0..P_BANDS {
            p.noiseoff[j][i] = lerp(bias[is][j][i] as f64, bias[is + 1][j][i] as f64, ds) as f32;
        }
    }
    let userbias = 0.0_f64;
    for j in 0..P_NOISECURVES {
        let min = p.noiseoff[j][0] + 6.0;
        for i in 0..P_BANDS {
            p.noiseoff[j][i] = (p.noiseoff[j][i] as f64 + userbias) as f32;
            if p.noiseoff[j][i] < min {
                p.noiseoff[j][i] = min;
            }
        }
    }

    // vorbis_encode_ath_setup
    p.ath_adjatt = ath_adjatt;
    p.ath_maxatt = ath_maxatt;
    p
}

#[cfg(test)]
mod tests;
