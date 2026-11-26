//! Quality-dependent encoder parameters, derived bit-exactly the way
//! libvorbis 1.3.7 `vorbis_encode_init_vbr()` derives them for the 44.1/48 kHz
//! mono/stereo templates (`ve_setup_44_stereo` / `ve_setup_44_uncoupled`).
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
//! | `psy`             | `ci->psy_param[0..4]`: 0 impulse, 1 padding (short),     |
//! |                   | 2 transition, 3 long                                     |
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
use crate::tables::quality as t;

/// Lowest `base_quality` libvorbis accepts for these templates (after its
/// `+1e-7` nudge, the request must be `>= quality_mapping_44[0] = -0.1`).
pub(crate) const MIN_BASE_QUALITY: f32 = -0.1;

/// Everything quality-dependent that the encoder needs outside the setup
/// header itself. See the module docs for field provenance.
#[derive(Clone, Debug)]
pub(crate) struct QualityConfig {
    /// `hi->base_setting`: template position in `0.0..11.0`.
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
    pub bitrate_nominal: i32,
    /// `hi->lowpass_kHz` (may be 999 for "no lowpass").
    pub lowpass_khz: f64,
    pub psy_global: VorbisInfoPsyGlobal,
    /// Indexed by libvorbis block type: 0 impulse, 1 padding, 2 transition, 3 long.
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

/// `vorbis_encode_setup_vbr` quality nudge + `get_setup_template` lookup.
/// Returns `base_setting`, or `None` if libvorbis would return `OV_EIMPL`.
fn base_setting_for(base_quality: f32) -> Option<f64> {
    let mut quality = (base_quality as f64 + 0.0000001) as f32;
    if quality as f64 >= 1. {
        quality = 0.9999_f64 as f32;
    }
    let req = quality as f64;
    let map = &t::QUALITY_MAPPING_44;
    // C would let NaN fall through to base_setting = 10.999; reject it instead.
    if req.is_nan() || req < map[0] || req > map[t::MAPPINGS] {
        return None;
    }
    let mut j = 0;
    while j < t::MAPPINGS {
        if req >= map[j] && req < map[j + 1] {
            break;
        }
        j += 1;
    }
    if j == t::MAPPINGS {
        return Some(j as f64 - 0.001);
    }
    let low = map[j] as f32;
    let high = map[j + 1] as f32;
    let del = ((req - low as f64) / (high - low) as f64) as f32;
    Some((j as f32 + del) as f64)
}

/// Derive all quality-dependent parameters for `base_quality` (libvorbis
/// scale: -0.1 = q-1 .. 1.0 = q10; fractional allowed), `rate` in
/// 40000..=50000 Hz and 1 or 2 channels.
///
/// Panics outside that domain (where libvorbis' `vorbis_encode_init_vbr`
/// would fail or select a different template).
pub(crate) fn quality_config(base_quality: f32, rate: u32, channels: usize) -> QualityConfig {
    assert!(
        (t::SAMPLERATE_MIN..=t::SAMPLERATE_MAX).contains(&rate),
        "quality_config: rate {rate} outside the 44.1/48 kHz template range"
    );
    assert!(
        channels == 1 || channels == 2,
        "quality_config: {channels} channels unsupported"
    );
    let base_setting = base_setting_for(base_quality).unwrap_or_else(|| {
        panic!("quality_config: base_quality {base_quality} outside -0.1..=1.0")
    });
    let s = base_setting;
    let is = s as usize;
    let ds = s - is as f64;

    // vorbis_encode_blocksize_setup
    let blocksizes = [
        t::BLOCKSIZE_SHORT_44[is] as usize,
        t::BLOCKSIZE_LONG_44[is] as usize,
    ];

    // vorbis_encode_setup_setting (hi->*)
    let lowpass_khz = lerp(t::PSY_LOWPASS_44[is], t::PSY_LOWPASS_44[is + 1], ds);
    let mut ath_floating_db = lerp(
        t::PSY_ATH_FLOATER[is] as f64,
        t::PSY_ATH_FLOATER[is + 1] as f64,
        ds,
    );
    let ath_absolute_db = lerp(t::PSY_ATH_ABS[is] as f64, t::PSY_ATH_ABS[is + 1] as f64, ds);
    let amplitude_track_db_per_sec = -6.0_f64;
    // vorbis_encode_setup_init clamps
    ath_floating_db = ath_floating_db.clamp(-200., -80.);

    let psy_global = global_psych(s, blocksizes, rate, amplitude_track_db_per_sec);

    let psy = std::array::from_fn(|block| {
        block_psy(s, block, ath_floating_db as f32, ath_absolute_db as f32)
    });

    // setting_to_approx_bitrate
    let r: &[f64; 12] = if channels == 2 {
        &t::RATE_MAPPING_44_STEREO
    } else {
        &t::RATE_MAPPING_44_UN
    };
    let bitrate_nominal = (lerp(r[is], r[is + 1], ds) * channels as f64) as i64 as i32;

    QualityConfig {
        base_setting,
        is,
        ds,
        blocksizes,
        bitrate_nominal,
        lowpass_khz,
        psy_global,
        psy,
    }
}

/// `vorbis_encode_global_psych_setup` + `vorbis_encode_global_stereo`
/// (unmanaged / VBR branch).
fn global_psych(
    s: f64,
    blocksizes: [usize; 2],
    rate: u32,
    amplitude_track: f64,
) -> VorbisInfoPsyGlobal {
    let x = &t::GLOBAL_MAPPING_44;
    let tmpl = &t::PSY_GLOBAL_44;
    let base = &tmpl[x[s as usize] as usize];

    let mut g = VorbisInfoPsyGlobal {
        eighth_octave_lines: base.eighth_octave_lines,
        preecho_thresh: base.preecho_thresh,
        postecho_thresh: base.postecho_thresh,
        stretch_penalty: base.stretch_penalty,
        preecho_minenergy: base.preecho_minenergy,
        ..Default::default()
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

    // global_stereo: `float s = hi->stereo_point_setting` (== base_setting).
    let s = s as f32;
    let is = s as usize;
    let ds = s as f64 - is as f64;
    let p = &t::PSY_STEREO_MODES_44;
    g.coupling_prepointamp = p[is].pre;
    g.coupling_postpointamp = p[is].post;

    let mid = PACKETBLOBS / 2;
    let point = |khz: f32, bs: usize| (khz as f64 * 1000. / rate as f64 * bs as f64) as i32;

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

/// psyset + tonemask + compand + peak + noisebias + ath setup for one block
/// type. All four per-block settings equal `base_setting` in VBR mode.
fn block_psy(s: f64, block: usize, ath_adjatt: f32, ath_maxatt: f32) -> VorbisInfoPsy {
    let is = s as usize;
    let ds = s - is as f64;
    let long = block >= 2;

    // vorbis_encode_psyset_setup (memcpy _psy_info_template; noise_normalize_p=1)
    let mut p = VorbisInfoPsy {
        blockflag: (block >> 1) as i32,
        noisemaskp: t::PSY_INFO_TEMPLATE_NOISEMASKP,
        noisewindowlo: t::PSY_INFO_TEMPLATE_NOISEWINDOWLO,
        noisewindowhi: t::PSY_INFO_TEMPLATE_NOISEWINDOWHI,
        normal_p: 1,
        normal_start: if long {
            t::NOISE_START_LONG_44[is]
        } else {
            t::NOISE_START_SHORT_44[is]
        },
        normal_partition: if long {
            t::NOISE_PART_LONG_44[is]
        } else {
            t::NOISE_PART_SHORT_44[is]
        },
        normal_thresh: t::NOISE_THRESH_44[is],
        ..Default::default()
    };

    // vorbis_encode_tonemask_setup
    let att = &t::PSY_TONE_MASTERATT_44;
    for k in 0..P_NOISECURVES {
        p.tone_masteratt[k] = lerp(att[is].att[k] as f64, att[is + 1].att[k] as f64, ds) as f32;
    }
    p.tone_centerboost = lerp(att[is].boost as f64, att[is + 1].boost as f64, ds) as f32;
    p.tone_decay = lerp(att[is].decay as f64, att[is + 1].decay as f64, ds) as f32;
    p.max_curve_dB = lerp(
        t::PSY_TONE_0DB[is] as f64,
        t::PSY_TONE_0DB[is + 1] as f64,
        ds,
    ) as f32;
    let adj = if block == 3 {
        &t::VP_TONEMASK_ADJ_LONGBLOCK
    } else {
        &t::VP_TONEMASK_ADJ_OTHERBLOCK
    };
    for i in 0..P_BANDS {
        p.toneatt[i] = lerp(adj[is][i] as f64, adj[is + 1][i] as f64, ds) as f32;
    }

    // vorbis_encode_compand_setup
    let (cis, cds) = remap(
        s,
        if long {
            &t::PSY_COMPAND_LONG_MAPPING
        } else {
            &t::PSY_COMPAND_SHORT_MAPPING
        },
    );
    let c = &t::PSY_COMPAND_44;
    for i in 0..NOISE_COMPAND_LEVELS {
        p.noisecompand[i] = lerp(c[cis][i] as f64, c[cis + 1][i] as f64, cds) as f32;
    }

    // vorbis_encode_peak_setup
    p.tone_abs_limit = lerp(
        t::PSY_TONE_SUPPRESS[is] as f64,
        t::PSY_TONE_SUPPRESS[is + 1] as f64,
        ds,
    ) as f32;

    // vorbis_encode_noisebias_setup (userbias = impulse_noisetune = 0 for
    // block 0, literal 0 otherwise)
    p.noisemaxsupp = lerp(
        t::PSY_NOISE_SUPPRESS[is] as f64,
        t::PSY_NOISE_SUPPRESS[is + 1] as f64,
        ds,
    ) as f32;
    let guard = t::PSY_NOISEGUARDS_44[block];
    p.noisewindowlomin = guard[0];
    p.noisewindowhimin = guard[1];
    p.noisewindowfixed = guard[2];
    let bias = match block {
        0 => &t::PSY_NOISEBIAS_IMPULSE,
        1 => &t::PSY_NOISEBIAS_PADDING,
        2 => &t::PSY_NOISEBIAS_TRANS,
        _ => &t::PSY_NOISEBIAS_LONG,
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
