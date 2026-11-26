//! Frozen verbatim copy of the hand-written Q5 builders from `src/encode.rs`
//! (`make_q5_psy_global`, `make_q5_psy*`, lines 31-580 as of commit 6935b20),
//! so `quality::tests` can check `quality_config(0.5, ..)` against them
//! without touching encode.rs. Delete once encode.rs consumes `QualityConfig`.
//!
//! Only the imports above and the `pub(super)` wrappers at the bottom were added.

#![allow(clippy::field_reassign_with_default)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::manual_memcpy)]
#![allow(dead_code)]

use crate::psy::{NOISE_COMPAND_LEVELS, P_BANDS, PACKETBLOBS, VorbisInfoPsy, VorbisInfoPsyGlobal};
use crate::window::{BLOCK_SIZE, HALF_BLOCK};

// Shared values reused across the four make_q5_psy_* builders.
// `0xc1f00005` ≈ -30.00000381 from libvorbis's q=0.5+ε interpolation.
const Q5_NOISEMAXSUPP: f32 = f32::from_bits(0xc1f00005);
const Q5_MAX_CURVE_DB: f32 = 105.0;

// tone_masteratt post-q-interpolation. _psy_tone_masteratt_44[6]={20,6,-6},
// [7]={20,3,-10}; q=0.5+ε interpolation between rows 6 and 7 lands a sub-ULP
// fraction off the row-6 values. All four blocktypes (long, impulse, short
// padding, transition) read the SAME tone_masteratt template at their `is`,
// so the f32-correct values must be used in all four make_q5_psy_* builders.
const Q5_TONE_MASTERATT: [f32; 3] = [
    20.0,
    f32::from_bits(0x40bffffa), // ~5.999996
    f32::from_bits(0xc0c00008), // ~-6.000004
];

// 17-entry tone-attenuation table used by all four make_q5_psy_* builders.
// Post-interpolation values libvorbis produces for q=0.5 after its
// `quality += 0.0000001` adjustment. Bits dumped from the oracle-encoder.
const Q5_TONEATT: [f32; 17] = [
    -16.0,
    -16.0,
    -16.0,
    -16.0,
    -16.0,
    -16.0,
    -16.0,
    -15.0,
    -14.0,
    -14.0,
    f32::from_bits(0xc1500001),
    f32::from_bits(0xc1300001),
    f32::from_bits(0xc11ffffe),
    f32::from_bits(0xbf800018),
    f32::from_bits(0xbf800008),
    f32::from_bits(0xb6000000),
    0.0,
];

// ---------------------------------------------------------------------------
// make_q5_psy_global: build VorbisInfoPsyGlobal for Q5
//
// Values from libvorbis lib/modes/psych_44.h _psy_global_44[2] (global_mapping=2.0 @ Q5)
// and lib/modes/psych_44.h _psy_stereo_modes_44[5] (stereo_point_setting=5 @ Q5).
// Coupling pointlimits computed as kHz*1000/rate*blocksizes[1] for unmanaged mode.
// ---------------------------------------------------------------------------

fn make_q5_psy_global(rate: i64, channels: usize) -> VorbisInfoPsyGlobal {
    let mut gi = VorbisInfoPsyGlobal::default();
    gi.eighth_octave_lines = 8;

    // vorbis_encode_global_psych_setup: memcpy(g, in[is]), then interpolate
    // ONLY i=0..3. Bands 4-6 keep template[is] values verbatim.
    //   For Q5: is=2, ds=0.5
    //   template[2] preecho  = {12, 10, 10, 10, 10, 10, 10}
    //   template[3] preecho  = {10, 8, 8, 8, 8, 8, 8}
    //   blend bands 0..3      = {11, 9, 9, 9}
    //   bands 4..6 from [2]   = {10, 10, 10}
    //   template[2] postecho = {-20, -20, -15, -15, -15, -15, -15}
    //   template[3] postecho = {-20, -15, -12, -12, -12, -12, -12}
    //   blend bands 0..3      = {-20, -17.5, -13.5, -13.5}
    //   bands 4..6 from [2]   = {-15, -15, -15}
    gi.preecho_thresh[0] = 11.0;
    gi.preecho_thresh[1] = 9.0;
    gi.preecho_thresh[2] = 9.0;
    gi.preecho_thresh[3] = 9.0;
    gi.preecho_thresh[4] = 10.0;
    gi.preecho_thresh[5] = 10.0;
    gi.preecho_thresh[6] = 10.0;

    gi.postecho_thresh[0] = -20.0;
    gi.postecho_thresh[1] = -17.5;
    gi.postecho_thresh[2] = -13.5;
    gi.postecho_thresh[3] = -13.5;
    gi.postecho_thresh[4] = -15.0;
    gi.postecho_thresh[5] = -15.0;
    gi.postecho_thresh[6] = -15.0;

    gi.stretch_penalty = 0.0;
    gi.preecho_minenergy = -80.0;
    gi.ampmax_att_per_sec = -6.0;

    // For stereo: Q5 stereo coupling from _psy_stereo_modes_44[5]
    // pre = {2,2,2,1,1,0,0,0,0,0,0,0,0,0,0}
    // post = {3,3,3,3,3,2,2,2,2,2,2,0,0,0,0}
    // kHz[7] = 12 kHz (unmanaged mode uses PACKETBLOBS/2=7 index for all)
    // lowpasskHz[7] = 99 kHz → sliding_lowpass = 99000/rate * 2048

    let pre = [2i32, 2, 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let post = [3i32, 3, 3, 3, 3, 2, 2, 2, 2, 2, 2, 0, 0, 0, 0];

    for i in 0..PACKETBLOBS {
        gi.coupling_prepointamp[i] = pre[i];
        gi.coupling_postpointamp[i] = post[i];
    }

    if channels > 1 {
        // coupling_pointlimit: kHz * 1000 / rate * blocksizes[1]
        // for unmanaged mode, all i use the same kHz = kHz[PACKETBLOBS/2] = 12
        let coupling_khz = 12.0_f64;
        let coupling_limit = (coupling_khz * 1000.0 / rate as f64 * BLOCK_SIZE as f64) as i32;
        for i in 0..PACKETBLOBS {
            gi.coupling_pointlimit[0][i] = coupling_limit / 8; // short block (not used)
            gi.coupling_pointlimit[1][i] = coupling_limit.min(HALF_BLOCK as i32);
        }
        gi.coupling_pkHz = [12i32; PACKETBLOBS];

        // sliding_lowpass: 99 kHz = very large → clamp to n2
        let lp_long = (99000.0_f64 / rate as f64 * BLOCK_SIZE as f64) as i32;
        let lp_long = lp_long.min(HALF_BLOCK as i32);
        for i in 0..PACKETBLOBS {
            gi.sliding_lowpass[0][i] = lp_long / 8; // short
            gi.sliding_lowpass[1][i] = lp_long; // long
        }
    } else {
        // mono: no coupling, set sliding_lowpass to full n2
        for i in 0..PACKETBLOBS {
            gi.sliding_lowpass[0][i] = (BLOCK_SIZE / 8) as i32;
            gi.sliding_lowpass[1][i] = HALF_BLOCK as i32;
            gi.coupling_pointlimit[0][i] = (BLOCK_SIZE / 8) as i32;
            gi.coupling_pointlimit[1][i] = HALF_BLOCK as i32;
        }
    }

    gi
}

// ---------------------------------------------------------------------------
// make_q5_psy: build VorbisInfoPsy for Q5 long block
//
// Values from libvorbis psych_44.h at is=6 (base_setting=6.0, ds≈0).
// ffmpeg -q:a 5 → libvorbis quality=0.5+ε → quality_mapping_44 j=6 → base_setting=6.
// Array index 6 in each table is the authoritative source.
// ---------------------------------------------------------------------------

fn make_q5_psy() -> VorbisInfoPsy {
    let mut vi = VorbisInfoPsy::default();

    vi.blockflag = 1; // long block

    // ATH params: _psy_ath_floater[6]=-105, _psy_ath_abs[6]=-140
    vi.ath_adjatt = -105.0;
    vi.ath_maxatt = -140.0;

    // Tone mask params: post-interpolation values from oracle dump for q=0.5
    // (between row6 [20,6,-6] and row7 [20,3,-10] with ds≈1e-6).
    vi.tone_masteratt = Q5_TONE_MASTERATT;
    vi.tone_centerboost = 0.0;
    vi.tone_decay = 0.0;
    vi.tone_abs_limit = f32::from_bits(0xc1f00005); // ~-30.00000381

    // Tone per-band attenuation: post-interpolation values libvorbis produces for
    // q=0.5 after its `quality += 0.0000001` adjustment. Bits dumped from the
    // oracle-encoder; values match the literal table at indices 0..9 + 16, but
    // 10..15 carry a 1e-5 fraction toward the q=6 row from the interpolation.
    vi.toneatt = Q5_TONEATT;

    vi.max_curve_dB = Q5_MAX_CURVE_DB;

    // Noise mask params
    vi.noisemaskp = 1;
    vi.noisemaxsupp = Q5_NOISEMAXSUPP;
    vi.noisewindowlo = 0.5;
    vi.noisewindowhi = 0.5;
    vi.noisewindowlomin = 10; // _psy_noiseguards_44[2].lo (long block)
    vi.noisewindowhimin = 10; // _psy_noiseguards_44[2].hi
    vi.noisewindowfixed = 100; // _psy_noiseguards_44[2].fixed

    // Noise offsets dumped from libvorbis psy[3] (block=3, LONG MAINLINE) after
    // vorbis_encode_noisebias_setup interpolation+clamp at q=0.5+ε. Values
    // include sub-ULP perturbations from the q-interpolation; raw curves come
    // from _psy_noisebias_long[6]/[7].
    let noiseoff_0: [f32; P_BANDS] = [
        -9.0, -9.0, -9.0, -9.0, -9.0, -9.0, -9.0, -9.0, -4.0, 1.0, 1.0, 1.0, 2.0, 3.0, 3.0, 4.0,
        7.0,
    ];
    let noiseoff_1: [f32; P_BANDS] = [
        f32::from_bits(0xc1800001),
        f32::from_bits(0xc1800001),
        f32::from_bits(0xc1800001),
        f32::from_bits(0xc1800001),
        f32::from_bits(0xc1800001),
        f32::from_bits(0xc1800001),
        f32::from_bits(0xc1800001),
        f32::from_bits(0xc1800001),
        f32::from_bits(0xc1400002),
        f32::from_bits(0xc0c00004),
        f32::from_bits(0xc0800004),
        f32::from_bits(0xc0800004),
        f32::from_bits(0xc0800004),
        f32::from_bits(0xc0800004),
        f32::from_bits(0xc0400008),
        f32::from_bits(0xbf800008),
        0.0,
    ];
    let noiseoff_2: [f32; P_BANDS] = [
        f32::from_bits(0xc1900001),
        f32::from_bits(0xc1900001),
        f32::from_bits(0xc1900001),
        f32::from_bits(0xc1900001),
        f32::from_bits(0xc1900001),
        f32::from_bits(0xc1900001),
        f32::from_bits(0xc1900001),
        f32::from_bits(0xc1900000),
        f32::from_bits(0xc1600002),
        f32::from_bits(0xc1400003),
        f32::from_bits(0xc1400003),
        f32::from_bits(0xc1400003),
        f32::from_bits(0xc1400003),
        f32::from_bits(0xc1200003),
        f32::from_bits(0xc1200003),
        f32::from_bits(0xc1100003),
        f32::from_bits(0xc1000002),
    ];
    vi.noiseoff[0] = noiseoff_0;
    vi.noiseoff[1] = noiseoff_1;
    vi.noiseoff[2] = noiseoff_2;

    // Noise compander: Q5 → base_setting=5.0 → _psy_compand_long_mapping[5]=5.0 → is=5,ds=0 → is=4,ds=1.0
    // → interpolate(in[4], in[5]) with ds=1.0 → _psy_compand_44[5] "mode A long"
    let noisecompand: [f32; NOISE_COMPAND_LEVELS] = [
        0., 1., 2., 3., 4., 5., 6., 7., 8., 8., 7., 6., 5., 4., 4., 4., 4., 4., 5., 5., 5., 6., 6.,
        6., 7., 7., 7., 8., 8., 8., 9., 10., 11., 12., 13., 14., 15., 16., 17., 18.,
    ];
    vi.noisecompand = noisecompand;

    // Noise normalization: normal_start=9999 effectively disables it
    vi.normal_p = 1;
    vi.normal_start = 9999;
    vi.normal_partition = 32;
    vi.normal_thresh = 9999.0;

    vi
}

// ---------------------------------------------------------------------------
// make_q5_psy_impulse: build VorbisInfoPsy for Q5 short block, IMPULSE type
//
// Uses libvorbis block type 0 (impulse, W=0) at is=6.
// Noise bias: _psy_noisebias_impulse[6]
// Tone adj: _vp_tonemask_adj_otherblock[6]
// Guards: _psy_noiseguards_44[1] = {lo=3, hi=3, fixed=15}
// Compand: _psy_compand_44[2] (via _psy_compand_short_mapping[6]=2.0)
// ---------------------------------------------------------------------------

fn make_q5_psy_impulse() -> VorbisInfoPsy {
    let mut vi = VorbisInfoPsy::default();

    vi.blockflag = 0; // short block

    // ath: -105.0 / -140.0 are exact f32 integers
    vi.ath_adjatt = -105.0;
    vi.ath_maxatt = -140.0;

    // Interpolated at q=0.5 (ds≈1e-6) between row6 [20, 6, -6] and row7 [20, 3, -10]
    vi.tone_masteratt = Q5_TONE_MASTERATT;
    vi.tone_centerboost = 0.0;
    vi.tone_decay = 0.0;
    vi.tone_abs_limit = f32::from_bits(0xc1f00005); // ~-30.00000381 from interp

    vi.toneatt = Q5_TONEATT;

    vi.max_curve_dB = Q5_MAX_CURVE_DB;

    vi.noisemaskp = 1;
    vi.noisemaxsupp = Q5_NOISEMAXSUPP;
    vi.noisewindowlo = 0.5;
    vi.noisewindowhi = 0.5;
    vi.noisewindowlomin = 3;
    vi.noisewindowhimin = 3;
    vi.noisewindowfixed = 15;

    // Noise offsets from _psy_noisebias_impulse[6] (C-index 6, label "5") after
    // vorbis_encode_noisebias_setup clamping: min = noiseoff[j][0]+6, userbias=0.
    // raw curve 0: {-20,-20,-20,-20,-20,-18,-14,-10,-4,0,0,0,0,4,4,6,11}, min=-14
    // raw curve 1: {-32,-32,-32,-32,-28,-24,-22,-16,-10,-6,-8,-8,-6,-6,-6,-4,-2}, min=-26
    // raw curve 2: {-34,-34,-34,-34,-30,-26,-24,-18,-14,-12,-12,-12,-12,-12,-10,-9,-5}, min=-28
    // Post-interpolation values from C dump (q=0.5+ε, ds≈1e-6).
    // Integer values are exact f32; the fractional ones come from interp.
    let noiseoff_0: [f32; P_BANDS] = [
        -14.0, -14.0, -14.0, -14.0, -14.0, -14.0, -14.0, -10.0, -4.0, 0.0, 0.0, 0.0, 0.0, 4.0, 4.0,
        6.0, 11.0,
    ];
    let noiseoff_1: [f32; P_BANDS] = [
        -26.0,
        -26.0,
        -26.0,
        -26.0,
        -26.0,
        f32::from_bits(0xc1c00003),
        f32::from_bits(0xc1b00004),
        f32::from_bits(0xc1800004),
        f32::from_bits(0xc1200006),
        f32::from_bits(0xc0c00014),
        f32::from_bits(0xc1000008),
        f32::from_bits(0xc1000008),
        f32::from_bits(0xc0c00014),
        f32::from_bits(0xc0c00014),
        f32::from_bits(0xc0c00010),
        f32::from_bits(0xc0800014),
        f32::from_bits(0xc0000028),
    ];
    let noiseoff_2: [f32; P_BANDS] = [
        -28.0,
        -28.0,
        -28.0,
        -28.0,
        -28.0,
        f32::from_bits(0xc1d00004),
        f32::from_bits(0xc1c00002),
        f32::from_bits(0xc1900003),
        f32::from_bits(0xc1600006),
        f32::from_bits(0xc1400008),
        f32::from_bits(0xc1400008),
        f32::from_bits(0xc1400008),
        f32::from_bits(0xc1400008),
        f32::from_bits(0xc1400008),
        f32::from_bits(0xc120000a),
        f32::from_bits(0xc1100009),
        f32::from_bits(0xc0a00016),
    ];
    vi.noiseoff[0] = noiseoff_0;
    vi.noiseoff[1] = noiseoff_1;
    vi.noiseoff[2] = noiseoff_2;

    // Same compand as padding (both use _psy_compand_short_mapping[6]=2 → _psy_compand_44[2])
    let noisecompand: [f32; NOISE_COMPAND_LEVELS] = [
        0., 1., 2., 3., 4., 5., 5., 5., 6., 6., 6., 5., 4., 4., 4., 4., 4., 4., 5., 5., 5., 6., 6.,
        6., 7., 7., 7., 8., 8., 8., 9., 10., 11., 12., 13., 14., 15., 16., 17., 18.,
    ];
    vi.noisecompand = noisecompand;

    vi.normal_p = 1;
    vi.normal_start = 9999;
    vi.normal_partition = 32;
    vi.normal_thresh = 9999.0;

    vi
}

// ---------------------------------------------------------------------------
// make_q5_psy_short: build VorbisInfoPsy for Q5 short block (n=256, n2=128)
//
// Uses libvorbis block type 1 (padding) at is=6.
// Noise bias: _psy_noisebias_padding[6]
// Tone adj: _vp_tonemask_adj_otherblock[6]
// Guards: _psy_noiseguards_44[1] = {lo=3, hi=3, fixed=15}
// Compand: _psy_compand_44[2] "mode A short" (via _psy_compand_short_mapping[6]=2.0)
// ---------------------------------------------------------------------------

fn make_q5_psy_short() -> VorbisInfoPsy {
    let mut vi = VorbisInfoPsy::default();

    vi.blockflag = 0; // short block

    vi.ath_adjatt = -105.0;
    vi.ath_maxatt = -140.0;

    // Tone mask: _psy_tone_masteratt_44[6] = {{20,6,-6},0,0}
    vi.tone_masteratt = Q5_TONE_MASTERATT;
    vi.tone_centerboost = 0.0;
    vi.tone_decay = 0.0;
    vi.tone_abs_limit = -30.0; // _psy_tone_suppress[6] = -30

    // Short block tone attenuation from _vp_tonemask_adj_otherblock[6]
    // {-16,-16,-16,-16,-16,-16,-16,-15,-14,-14,-14,-12,-8,-4,-2,-2,0}
    vi.toneatt = Q5_TONEATT;

    vi.max_curve_dB = Q5_MAX_CURVE_DB;

    vi.noisemaskp = 1;
    vi.noisemaxsupp = Q5_NOISEMAXSUPP;
    vi.noisewindowlo = 0.5;
    vi.noisewindowhi = 0.5;
    vi.noisewindowlomin = 3; // _psy_noiseguards_44[1].lo = 3
    vi.noisewindowhimin = 3; // _psy_noiseguards_44[1].hi = 3
    vi.noisewindowfixed = 15; // _psy_noiseguards_44[1].fixed = 15

    // Noise offsets from _psy_noisebias_padding[6] (C-index 6, label "5") after
    // vorbis_encode_noisebias_setup clamping: min = noiseoff[j][0]+6, userbias=0.
    // raw curve 0: {-20,-20,-20,-20,-20,-18,-14,-10,-4,0,0,0,0,4,6,6,12}, min=-14
    // raw curve 1: {-32,-32,-32,-32,-28,-24,-22,-16,-12,-6,-3,-3,-3,-3,-2,0,4}, min=-26
    // raw curve 2: {-34,-34,-34,-34,-30,-26,-24,-18,-14,-10,-10,-10,-10,-10,-8,-5,-3}, min=-28
    let noiseoff_0: [f32; P_BANDS] = [
        -14.0, -14.0, -14.0, -14.0, -14.0, -14.0, -14.0, -10.0, -4.0, 0.0, 0.0, 0.0, 0.0, 4.0, 6.0,
        6.0, 12.0,
    ];
    // Post q-interpolation values from libvorbis psy[1] (block=1, SHORT PADDING)
    // for q=0.5+ε. Integer rows are exact f32; fractional ULP perturbations come
    // from the quality-interpolation between _psy_noisebias_padding[6] and [7].
    let noiseoff_1: [f32; P_BANDS] = [
        -26.0,
        -26.0,
        -26.0,
        -26.0,
        -26.0,
        f32::from_bits(0xc1c00003),
        f32::from_bits(0xc1b00001),
        f32::from_bits(0xc1800002),
        f32::from_bits(0xc1400002),
        f32::from_bits(0xc0c00004),
        f32::from_bits(0xc0400004),
        f32::from_bits(0xc0400004),
        f32::from_bits(0xc0400004),
        f32::from_bits(0xc0400004),
        f32::from_bits(0xc0000004),
        f32::from_bits(0xb5800000),
        4.0,
    ];
    let noiseoff_2: [f32; P_BANDS] = [
        -28.0,
        -28.0,
        -28.0,
        -28.0,
        -28.0,
        f32::from_bits(0xc1d00002),
        f32::from_bits(0xc1c00001),
        f32::from_bits(0xc1900001),
        f32::from_bits(0xc1600002),
        f32::from_bits(0xc1200003),
        f32::from_bits(0xc1200003),
        f32::from_bits(0xc1200003),
        f32::from_bits(0xc1200003),
        f32::from_bits(0xc1200003),
        f32::from_bits(0xc1000003),
        f32::from_bits(0xc0a00006),
        f32::from_bits(0xc040000c),
    ];
    vi.noiseoff[0] = noiseoff_0;
    vi.noiseoff[1] = noiseoff_1;
    vi.noiseoff[2] = noiseoff_2;

    // Noise compander: _psy_compand_44[2] "mode A short"
    // (via _psy_compand_short_mapping[6]=2.0 → is=1, ds=1 → _psy_compand_44[2])
    let noisecompand: [f32; NOISE_COMPAND_LEVELS] = [
        0., 1., 2., 3., 4., 5., 5., 5., 6., 6., 6., 5., 4., 4., 4., 4., 4., 4., 5., 5., 5., 6., 6.,
        6., 7., 7., 7., 8., 8., 8., 9., 10., 11., 12., 13., 14., 15., 16., 17., 18.,
    ];
    vi.noisecompand = noisecompand;

    vi.normal_p = 1;
    vi.normal_start = 9999;
    vi.normal_partition = 32;
    vi.normal_thresh = 9999.0;

    vi
}

// ---------------------------------------------------------------------------
// make_q5_psy_transition: build VorbisInfoPsy for Q5 short→long transition block
//
// Uses libvorbis block type 2 (TRANSITION) at is=6.
// Noise bias: _psy_noisebias_trans[6]
// Tone adj: _vp_tonemask_adj_otherblock[6] (same as long block)
// Guards: _psy_noiseguards_44[2] = {lo=10, hi=10, fixed=100} (same as long)
// Compand: same as long block (_psy_compand_long_mapping[6]=5 → _psy_compand_44[5])
// ---------------------------------------------------------------------------

fn make_q5_psy_transition() -> VorbisInfoPsy {
    let mut vi = VorbisInfoPsy::default();

    vi.blockflag = 1; // long block

    vi.ath_adjatt = -105.0;
    vi.ath_maxatt = -140.0;

    // Tone mask: _psy_tone_masteratt_44[6] = {{20,6,-6},0,0}
    vi.tone_masteratt = Q5_TONE_MASTERATT;
    vi.tone_centerboost = 0.0;
    vi.tone_decay = 0.0;
    vi.tone_abs_limit = -30.0;

    // Tone per-band attenuation from _vp_tonemask_adj_otherblock[6]
    // {-16,-16,-16,-16,-16,-16,-16,-15,-14,-14,-14,-12,-8,-4,-2,-2,0}
    vi.toneatt = Q5_TONEATT;

    vi.max_curve_dB = Q5_MAX_CURVE_DB;

    vi.noisemaskp = 1;
    vi.noisemaxsupp = Q5_NOISEMAXSUPP;
    vi.noisewindowlo = 0.5;
    vi.noisewindowhi = 0.5;
    vi.noisewindowlomin = 10;
    vi.noisewindowhimin = 10;
    vi.noisewindowfixed = 100;

    // Noise offsets dumped from libvorbis psy[2] (block=2, LONG TRANSITION)
    // after vorbis_encode_noisebias_setup interpolation+clamp at q=0.5+ε.
    // Raw curves come from _psy_noisebias_trans[6]/[7]; sub-ULP perturbations
    // arise from the q-interpolation.
    let noiseoff_0: [f32; P_BANDS] = [
        -18.0, -18.0, -18.0, -18.0, -18.0, -18.0, -14.0, -8.0, -1.0, 1.0, 1.0, 1.0, 2.0, 3.0, 3.0,
        4.0, 7.0,
    ];
    let noiseoff_1: [f32; P_BANDS] = [
        -26.0,
        -26.0,
        -26.0,
        -26.0,
        -26.0,
        -24.0,
        f32::from_bits(0xc1b00001),
        f32::from_bits(0xc1800001),
        f32::from_bits(0xc1400002),
        f32::from_bits(0xc0c00004),
        f32::from_bits(0xc0800004),
        f32::from_bits(0xc0800004),
        f32::from_bits(0xc0800004),
        f32::from_bits(0xc0800004),
        f32::from_bits(0xc0400008),
        f32::from_bits(0xbf800008),
        0.0,
    ];
    let noiseoff_2: [f32; P_BANDS] = [
        -28.0,
        -28.0,
        -28.0,
        -28.0,
        -28.0,
        f32::from_bits(0xc1c00001),
        f32::from_bits(0xc1c00001),
        f32::from_bits(0xc1900003),
        f32::from_bits(0xc1600008),
        f32::from_bits(0xc1400007),
        f32::from_bits(0xc1400007),
        f32::from_bits(0xc1400007),
        f32::from_bits(0xc1400007),
        f32::from_bits(0xc1200008),
        f32::from_bits(0xc1200007),
        f32::from_bits(0xc1100007),
        f32::from_bits(0xc0a0000e),
    ];
    vi.noiseoff[0] = noiseoff_0;
    vi.noiseoff[1] = noiseoff_1;
    vi.noiseoff[2] = noiseoff_2;

    // Same compand as long block: _psy_compand_44[5] "mode A long"
    let noisecompand: [f32; NOISE_COMPAND_LEVELS] = [
        0., 1., 2., 3., 4., 5., 6., 7., 8., 8., 7., 6., 5., 4., 4., 4., 4., 4., 5., 5., 5., 6., 6.,
        6., 7., 7., 7., 8., 8., 8., 9., 10., 11., 12., 13., 14., 15., 16., 17., 18.,
    ];
    vi.noisecompand = noisecompand;

    vi.normal_p = 1;
    vi.normal_start = 9999;
    vi.normal_partition = 32;
    vi.normal_thresh = 9999.0;

    vi
}

pub(super) fn q5_psy_global(rate: i64, channels: usize) -> VorbisInfoPsyGlobal {
    make_q5_psy_global(rate, channels)
}

/// Indexed like `QualityConfig::psy`: impulse, padding, transition, long.
pub(super) fn q5_psy() -> [VorbisInfoPsy; 4] {
    [
        make_q5_psy_impulse(),
        make_q5_psy_short(),
        make_q5_psy_transition(),
        make_q5_psy(),
    ]
}
