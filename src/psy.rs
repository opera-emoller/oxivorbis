//! Psychoacoustic model — literal port of libvorbis 1.3.7 lib/psy.c (encode side only).
//!
//! All runtime transcendentals (log, exp, sin, cos, atan, pow) have been
//! replaced by either precomputed constants or inline IEEE-754 bit tricks that
//! match the originals exactly.  The only remaining transcendental is `sqrt`,
//! which is IEEE-754 required and therefore platform-identical.

#![allow(clippy::needless_range_loop)]
#![allow(clippy::manual_clamp)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::assign_op_pattern)]
#![allow(clippy::approx_constant)]
#![allow(clippy::too_many_arguments)]
#![allow(non_snake_case)]
#![allow(unused_mut)]
#![allow(unused_variables)]
#![allow(unused_assignments)]

use crate::tables::lookup::FLOOR1_FROMDB_LOOKUP;
use crate::tables::masking::{ATH, EHMER_MAX, EHMER_OFFSET, MAX_ATH, TONEMASKS};

// ---------------------------------------------------------------------------
// Constants from psy.h / psy.c
// ---------------------------------------------------------------------------

const NEGINF: f32 = -9999.0_f32;

/// Upper bound on `VorbisLookPsy::total_octave_lines` — used to size the
/// stack-allocated seed scratch in `vp_tonemask`. Observed max for the Q5
/// supported input space is 777 (long block at 44.1kHz, n=1024); the q-1
/// long block (n=2048) reaches 841 at 44.1kHz, so 1024 still has headroom.
/// (n-sized scratch uses `window::MAX_LONG_HALF` instead.)
const MAX_OCTAVE_LINES: usize = 1024;
/// Upper bound on `eighth_octave_lines` (8 in every libvorbis setup);
/// padding for the phase-major seed scratch in `vp_tonemask`.
const MAX_LINESPER: usize = 64;

/// Q5 normal_partition is 32; older 16-partition layouts also fit. Used for
/// stack-allocated scratch in `vp_couple_quantize_normalize`.
const MAX_PARTITION: usize = 32;
/// Mono or stereo for the supported input space.
const MAX_CH: usize = 2;
/// Stereo Q5 has 1 coupling step; mono has 0. 2 is overkill safe.
const MAX_COUPLING_STEPS: usize = 2;

pub const P_BANDS: usize = 17;
pub const P_LEVELS: usize = 8;
pub const P_LEVEL_0: f32 = 30.0_f32;
pub const P_NOISECURVES: usize = 3;
pub const NOISE_COMPAND_LEVELS: usize = 40;

pub const PACKETBLOBS: usize = 15;
pub const VE_BANDS: usize = 7;

// stereo thresholds from psy.c
static STEREO_THRESHHOLDS: [f64; 9] = [0.0, 0.5, 1.0, 1.5, 2.5, 4.5, 8.5, 16.5, 9e10];
static STEREO_THRESHHOLDS_LIMITED: [f64; 9] = [0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 4.5, 8.5, 9e10];

// ---------------------------------------------------------------------------
// Scale helpers — literal ports of scales.h macros.
// No runtime transcendentals: toBARK / toOC / fromOC use inline formulas that
// *do* call atan/log/exp.  We keep them here as private helpers used only
// during psy_init (setup), not in the per-frame hot path.
// ---------------------------------------------------------------------------

/// toBARK (scales.h): `13.1f*atan(.00074f*(n))+2.24f*atan((n)*(n)*1.85e-8f)+1e-4f*(n)`
/// for the integer (`long`) argument `_vp_psy_init` passes (`rate/(2*n)*i`).
///
/// With an integer `n` the C types are: `.00074f*(n)` and `1e-4f*(n)` are
/// float multiplies; `(n)*(n)` is an exact integer product that is then
/// rounded to float and multiplied in float; only the `atan`s and the
/// `13.1f*`/`2.24f*` products (and hence the sums) are double. The result
/// is double — callers compare it unrounded and only `float bark = ...`
/// rounds to f32.
fn to_bark(n: i64) -> f64 {
    let nf = n as f32;
    let a = (0.00074_f32 * nf) as f64;
    let b = ((n * n) as f32 * 1.85e-8_f32) as f64;
    let c = (1e-4_f32 * nf) as f64;
    13.1_f32 as f64 * a.atan() + 2.24_f32 as f64 * b.atan() + c
}

/// toOC (scales.h): `log(n)*1.442695f-5.965784f`. `log` is double, so the
/// result is double whatever the argument type; the f32 constants are
/// promoted. These are textual macros in C: each call site must form its
/// argument in the C expression's own type (pass it here as f64 — exact
/// for float arguments) and round the double result only where C assigns
/// it to a float/int.
fn to_oc(n: f64) -> f64 {
    n.ln() * 1.442695_f32 as f64 - 5.965784_f32 as f64
}

/// fromOC (scales.h): `exp(((o)+5.965784f)*.693147f)`; double result, see
/// [`to_oc`].
fn from_oc(o: f64) -> f64 {
    ((o + 5.965784_f32 as f64) * 0.693147_f32 as f64).exp()
}

/// todB: bit-manipulation fast dB (scales.h, VORBIS_IEEE_FLOAT32 path).
/// Uses the bit trick: dB ≈ bits * 7.17711438e-7 - 764.6161886.
/// No transcendentals.
#[inline(always)]
pub fn to_db(x: f32) -> f32 {
    let bits = x.to_bits() & 0x7fff_ffff;
    bits as f32 * 7.17711438e-7_f32 - 764.6161886_f32
}

/// unitnorm: sign-preserving unit normalisation (scales.h, IEEE path).
#[inline(always)]
pub fn unitnorm(x: f32) -> f32 {
    let bits = (x.to_bits() & 0x8000_0000u32) | 0x3f80_0000u32;
    f32::from_bits(bits)
}

// ---------------------------------------------------------------------------
// Info structs
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct VorbisInfoPsy {
    pub blockflag: i32,

    pub ath_adjatt: f32,
    pub ath_maxatt: f32,

    pub tone_masteratt: [f32; P_NOISECURVES],
    pub tone_centerboost: f32,
    pub tone_decay: f32,
    pub tone_abs_limit: f32,
    pub toneatt: [f32; P_BANDS],

    pub noisemaskp: i32,
    pub noisemaxsupp: f32,
    pub noisewindowlo: f32,
    pub noisewindowhi: f32,
    pub noisewindowlomin: i32,
    pub noisewindowhimin: i32,
    pub noisewindowfixed: i32,
    pub noiseoff: [[f32; P_BANDS]; P_NOISECURVES],
    pub noisecompand: [f32; NOISE_COMPAND_LEVELS],

    pub max_curve_dB: f32,

    pub normal_p: i32,
    pub normal_start: i32,
    pub normal_partition: i32,
    pub normal_thresh: f64,
}

impl Default for VorbisInfoPsy {
    fn default() -> Self {
        Self {
            blockflag: 0,
            ath_adjatt: 0.0,
            ath_maxatt: 0.0,
            tone_masteratt: [0.0; P_NOISECURVES],
            tone_centerboost: 0.0,
            tone_decay: 0.0,
            tone_abs_limit: 0.0,
            toneatt: [0.0; P_BANDS],
            noisemaskp: 0,
            noisemaxsupp: 0.0,
            noisewindowlo: 0.0,
            noisewindowhi: 0.0,
            noisewindowlomin: 0,
            noisewindowhimin: 0,
            noisewindowfixed: 0,
            noiseoff: [[0.0; P_BANDS]; P_NOISECURVES],
            noisecompand: [0.0; NOISE_COMPAND_LEVELS],
            max_curve_dB: 0.0,
            normal_p: 0,
            normal_start: 0,
            normal_partition: 0,
            normal_thresh: 0.0,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct VorbisInfoPsyGlobal {
    pub eighth_octave_lines: i32,

    pub preecho_thresh: [f32; VE_BANDS],
    pub postecho_thresh: [f32; VE_BANDS],
    pub stretch_penalty: f32,
    pub preecho_minenergy: f32,

    pub ampmax_att_per_sec: f32,

    pub coupling_pkHz: [i32; PACKETBLOBS],
    pub coupling_pointlimit: [[i32; PACKETBLOBS]; 2],
    pub coupling_prepointamp: [i32; PACKETBLOBS],
    pub coupling_postpointamp: [i32; PACKETBLOBS],
    pub sliding_lowpass: [[i32; PACKETBLOBS]; 2],
}

#[derive(Clone, Debug, Default)]
pub struct VorbisLookPsyGlobal {
    pub ampmax: f32,
    pub channels: i32,
    pub gi: Option<VorbisInfoPsyGlobal>,
    pub coupling_pointlimit: [[i32; P_NOISECURVES]; 2],
}

// ---------------------------------------------------------------------------
// Tone curve buffer: mirrors the float*** returned by setup_tone_curves.
// We flatten to Box<[[[f32; EHMER_MAX+2]; P_LEVELS]; P_BANDS]>.
// ---------------------------------------------------------------------------

pub type ToneCurves = Box<[[[f32; EHMER_MAX + 2]; P_LEVELS]; P_BANDS]>;

// ---------------------------------------------------------------------------
// Look struct
// ---------------------------------------------------------------------------

pub struct VorbisLookPsy {
    pub n: usize,
    pub vi: VorbisInfoPsy,

    pub tonecurves: ToneCurves,
    pub noiseoffset: Vec<Vec<f32>>, // [P_NOISECURVES][n]

    pub ath: Vec<f32>,    // [n]
    pub octave: Vec<i32>, // [n]  in n.ocshift format (i32 in C is long)
    pub bark: Vec<i64>,   // [n]

    pub firstoc: i64,
    pub shiftoc: i32,
    pub eighth_octave_lines: i32,
    pub total_octave_lines: i32,
    pub rate: i64,

    pub m_val: f32,
}

// ---------------------------------------------------------------------------
// setup_tone_curves — port of the static function in psy.c
// ---------------------------------------------------------------------------

fn setup_tone_curves(
    curveatt_db: &[f32; P_BANDS],
    geom: &ToneGeom,
    n: usize,
    center_boost: f32,
    center_decay_rate: f32,
) -> ToneCurves {
    let mut workc = [[[0.0_f32; EHMER_MAX]; P_LEVELS]; P_BANDS];
    let athc0 = ath_curves();

    // build return buffer
    let mut ret = Box::new([[[0.0_f32; EHMER_MAX + 2]; P_LEVELS]; P_BANDS]);

    // The centered curve boost/decay depends only on k.
    let mut adj = [0.0_f32; EHMER_MAX];
    for (k, a) in adj.iter_mut().enumerate() {
        let offset_k = EHMER_OFFSET as i32 - k as i32;
        *a = center_boost + offset_k.unsigned_abs() as f32 * center_decay_rate;
        if *a < 0.0 && center_boost > 0.0 {
            *a = 0.0;
        }
        if *a > 0.0 && center_boost < 0.0 {
            *a = 0.0;
        }
    }

    for i in 0..P_BANDS {
        let mut athc = athc0[i];
        for j in 0..P_LEVELS {
            // copy curves (6 levels from tonemasks; replicate 0→0,1; 5→6,7)
            let mask = &TONEMASKS[i][j.max(2) - 2];
            // C: `curveatt_dB[i]+100.-(j<2?2:j)*10.-P_LEVEL_0` — the `100.`
            // literal (and P_LEVEL_0 = `30.`) make the whole sum double; it
            // is rounded to float once, when passed to attenuate_curve.
            let att = (curveatt_db[i] as f64 + 100.0
                - (if j < 2 { 2 } else { j }) as f64 * 10.0
                - P_LEVEL_0 as f64) as f32;
            let w = &mut workc[i][j];
            let a = &mut athc[j];
            for k in 0..EHMER_MAX {
                // apply centered curve boost/decay, then normalize
                w[k] = (mask[k] + adj[k]) + att;
                // overlay ATH: max_curve(athc[j], workc[i][j]), i.e.
                // `if (a < w) a = w`, as a select so it vectorizes.
                a[k] = if a[k] < w[k] { w[k] } else { a[k] };
            }
        }

        // limit louder curves
        for j in 1..P_LEVELS {
            let (prev, cur) = athc.split_at_mut(j);
            let (prev, a) = (&prev[j - 1], &mut cur[0]);
            let w = &mut workc[i][j];
            for k in 0..EHMER_MAX {
                // min_curve(athc[j], athc[j-1])
                a[k] = if a[k] > prev[k] { prev[k] } else { a[k] };
                // min_curve(workc[i][j], athc[j])
                w[k] = if w[k] > a[k] { a[k] } else { w[k] };
            }
        }
    }

    if crate::debug_dump::dump_enabled() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static FIRED: AtomicBool = AtomicBool::new(false);
        if !FIRED.swap(true, Ordering::Relaxed) {
            let mut bytes = Vec::new();
            for b in 0..P_BANDS {
                for lv in 0..P_LEVELS {
                    for v in workc[b][lv].iter() {
                        bytes.extend_from_slice(&v.to_le_bytes());
                    }
                }
            }
            let _ = std::fs::write("/tmp/lewtoff-debug/r_workc.bin", &bytes);
        }
    }

    // workc transposed to [band][j][level] for the all-levels spreading.
    let mut workt = [[[0.0_f32; P_LEVELS]; EHMER_MAX]; P_BANDS];
    for (wt, wc) in workt.iter_mut().zip(&workc) {
        for (j, col) in wt.iter_mut().enumerate() {
            for (m, v) in col.iter_mut().enumerate() {
                *v = wc[m][j];
            }
        }
    }

    for i in 0..P_BANDS {
        let slot = &geom.slot[i];

        // libvorbis spreads every curve into a per-bin `brute` buffer and
        // then reads it back only at the pulled bins. `geom.ops` lists, in
        // the walk's order, just the min updates that land on those bins,
        // so each `cell[s]` (= `brute[bin]`, all levels at once) sees
        // exactly libvorbis's sequence of `if (brute > w) brute = w`.
        let mut cell = [[999.0_f32; P_LEVELS]; EHMER_MAX];
        for &(src, j, s) in &geom.ops[geom.op_start[i]..geom.op_start[i + 1]] {
            min_levels(&mut cell[s as usize], &workt[src as usize][j as usize]);
        }

        for m in 0..P_LEVELS {
            // pull values back into curve
            for j in 0..EHMER_MAX {
                ret[i][m][j + 2] = match slot[j] {
                    NO_SLOT => -999.0,
                    s => cell[s as usize][m],
                };
            }
            // fenceposts
            let mut j = 0;
            while j < EHMER_OFFSET {
                if ret[i][m][j + 2] > -200.0 {
                    break;
                }
                j += 1;
            }
            ret[i][m][0] = j as f32;

            let mut j2 = EHMER_MAX as i32 - 1;
            while j2 > EHMER_OFFSET as i32 + 1 {
                if ret[i][m][j2 as usize + 2] > -200.0 {
                    break;
                }
                j2 -= 1;
            }
            ret[i][m][1] = j2 as f32;
        }
    }

    ret
}

/// setup_tone_curves' ATH overlay before the curves are folded in:
/// `athc[i][j][k] = min(ATH[i*4+k..][..4]) + (100.-j*10.f-P_LEVEL_0)`,
/// clamped to the last ATH entry. Plain f32 arithmetic on static tables,
/// so it is the same for every quality.
fn ath_curves() -> &'static [[[f32; EHMER_MAX]; P_LEVELS]; P_BANDS] {
    static ATHC: std::sync::OnceLock<Box<[[[f32; EHMER_MAX]; P_LEVELS]; P_BANDS]>> =
        std::sync::OnceLock::new();
    ATHC.get_or_init(|| {
        let mut athc = Box::new([[[0.0_f32; EHMER_MAX]; P_LEVELS]; P_BANDS]);
        for (i, band) in athc.iter_mut().enumerate() {
            // build ATH slice for this band
            let ath_offset = i * 4;
            let mut ath = [0.0_f32; EHMER_MAX];
            for j in 0..EHMER_MAX {
                let mut min = 999.0_f32;
                for k in 0..4 {
                    let idx = j + k + ath_offset;
                    if idx < MAX_ATH {
                        if min > ATH[idx] {
                            min = ATH[idx];
                        }
                    } else {
                        if min > ATH[MAX_ATH - 1] {
                            min = ATH[MAX_ATH - 1];
                        }
                    }
                }
                ath[j] = min;
            }
            for (j, level) in band.iter_mut().enumerate() {
                // C: `+100.-j*10.f-P_LEVEL_0`, double as well (integral, so exact).
                let att2 = (100.0 - (j as f32 * 10.0) as f64 - P_LEVEL_0 as f64) as f32;
                for k in 0..EHMER_MAX {
                    level[k] = ath[k] + att2;
                }
            }
        }
        athc
    })
}

/// `if (b > w) b = w` for every tone-curve level.
#[inline(always)]
fn min_levels(b: &mut [f32; P_LEVELS], w: &[f32; P_LEVELS]) {
    for (b, &w) in b.iter_mut().zip(w) {
        *b = if *b > w { w } else { *b };
    }
}

// ---------------------------------------------------------------------------
// Quality-independent init geometry, cached per (n, rate, eighth_octave_lines)
// ---------------------------------------------------------------------------

/// The bin geometry of `setup_tone_curves`' spreading walk. It depends only
/// on `binHz` (`rate*.5/n`) and `n`, not on the tone attenuations.
struct ToneGeom {
    /// Curve point j of band i is read back from bin
    /// `fromOC(j*.125+i*.5-2.)/binHz`, the same for every level. Each
    /// distinct such bin inside [0, n) has a slot; `slot[i][j]` is that
    /// bin's slot, or [`NO_SLOT`] for a bin outside [0, n) (read as -999).
    slot: [[u8; EHMER_MAX]; P_BANDS],
    /// The spreading walk's min updates that land on a pulled bin, in walk
    /// order: `(src, j, s)` is `brute[bin of slot s] min= workc[src][.][j]`.
    /// Band i's are `ops[op_start[i]..op_start[i + 1]]`.
    ops: Vec<(u8, u8, u8)>,
    op_start: [usize; P_BANDS + 1],
}

/// [`ToneGeom::slot`] of a curve point pulled from outside [0, n).
const NO_SLOT: u8 = u8::MAX;

/// `sorted.partition_point(|&b| b < x)`, searched linearly from `hint`.
fn lower_bound_from(sorted: &[usize], x: usize, hint: usize) -> usize {
    let mut i = hint.min(sorted.len());
    while i > 0 && sorted[i - 1] >= x {
        i -= 1;
    }
    while i < sorted.len() && sorted[i] < x {
        i += 1;
    }
    i
}

/// `Vec::dedup` for a sorted slice: moves the distinct values to the front
/// and returns their count.
fn dedup_sorted(v: &mut [usize]) -> usize {
    let mut len = 0;
    for i in 0..v.len() {
        if len == 0 || v[i] != v[len - 1] {
            v[len] = v[i];
            len += 1;
        }
    }
    len
}

impl ToneGeom {
    fn new(bin_hz: f32, n: usize) -> Self {
        let bin_hz_d = bin_hz as f64;
        // Every fromOC argument below is `m*.125 + c` for an integer m and
        // c = -2.0625 or -2.: `j*.125+k*.5` is (j+4k)*.125, `j*.125+k*.5
        // -1.9375` is (j+4k+1)*.125-2.0625 and `i*.5` is (4i+16)*.125-2.
        // These are small multiples of 1/16, so each side of every such
        // identity is computed exactly in f64 and the fromOC arguments are
        // bit-identical. That leaves 121 + 120 distinct exp() calls instead
        // of about 2900.
        const M: usize = EHMER_MAX + 4 * (P_BANDS - 1) + 1;
        let from_lo: [f64; M] = std::array::from_fn(|m| from_oc(m as f64 * 0.125 - 2.0625));
        let from_pull: [f64; M - 1] = std::array::from_fn(|m| from_oc(m as f64 * 0.125 - 2.0));
        let mut spread = [[(0usize, 0usize); EHMER_MAX]; P_BANDS];
        for (k, row) in spread.iter_mut().enumerate() {
            for (j, e) in row.iter_mut().enumerate() {
                // fromOC(j*.125+k*.5-2.0625) and fromOC(j*.125+k*.5-1.9375)
                let lo_bin = (from_lo[j + 4 * k] / bin_hz_d) as i64;
                let hi_bin = (from_lo[j + 4 * k + 1] / bin_hz_d) as i64 + 1;
                *e = (
                    lo_bin.max(0).min(n as i64) as usize,
                    hi_bin.max(0).min(n as i64) as usize,
                );
            }
        }
        let mut curves = [(0i64, 0i64); P_BANDS];
        let mut pull = [[0i64; EHMER_MAX]; P_BANDS];
        for i in 0..P_BANDS {
            // C: `bin=floor(fromOC(i*.5)/binHz);` — double throughout.
            // `toOC(bin*binHz+1)` / `toOC((bin+1)*binHz)`: int*float is
            // float, so those arguments are float; the toOC results stay
            // double.
            // fromOC(i*.5)
            let bin = (from_pull[4 * i + 16] / bin_hz_d).floor() as i64;
            let lo_curve_f = to_oc((bin as f32 * bin_hz + 1.0) as f64) * 2.0;
            let hi_curve_f = to_oc(((bin + 1) as f32 * bin_hz) as f64) * 2.0;
            let mut lo_curve = lo_curve_f.ceil() as i64;
            let mut hi_curve = hi_curve_f.floor() as i64;
            if lo_curve > i as i64 {
                lo_curve = i as i64;
            }
            if lo_curve < 0 {
                lo_curve = 0;
            }
            if hi_curve >= P_BANDS as i64 {
                hi_curve = P_BANDS as i64 - 1;
            }
            curves[i] = (lo_curve, hi_curve);
            for (j, b) in pull[i].iter_mut().enumerate() {
                // fromOC(j*.125+i*.5-2.)
                *b = (from_pull[j + 4 * i] / bin_hz_d) as i64;
            }
        }

        // Replay libvorbis's spreading walk on bin indices only. For band i
        // it spreads `workc[k]` along `spread[k]` for k in
        // lo_curve..=hi_curve, then `workc[i+1]` along `spread[i]` (the
        // next half-octave). Each walk keeps a cursor l: curve point j sets
        // `if (lo_bin < l) l = lo_bin`, then updates bins [l, hi_bin) (and
        // l < n), leaving l there; bins from the final l to n get the last
        // point. Only the updates of pulled bins are kept.
        let mut ops = Vec::new();
        let mut op_start = [0usize; P_BANDS + 1];
        let mut slot = [[NO_SLOT; EHMER_MAX]; P_BANDS];
        for i in 0..P_BANDS {
            op_start[i] = ops.len();
            // The distinct pulled bins inside [0, n), sorted. Points pulled
            // from the same bin read the same brute cell, so they share a
            // slot (low bands pull many points from the same few bins).
            let mut all = [0usize; EHMER_MAX];
            let mut len = 0;
            for &b in &pull[i] {
                if (0..n as i64).contains(&b) {
                    all[len] = b as usize;
                    len += 1;
                }
            }
            let bins = &mut all[..len];
            bins.sort_unstable();
            let len = dedup_sorted(bins);
            let bins = &all[..len];
            for (c, s) in slot[i].iter_mut().enumerate() {
                *s = match usize::try_from(pull[i][c]) {
                    Ok(b) => bins.binary_search(&b).map_or(NO_SLOT, |s| s as u8),
                    Err(_) => NO_SLOT,
                };
            }
            let mut walk = |row: &[(usize, usize); EHMER_MAX], src: usize| {
                // Slot indices bounding the bins in [first, l): the first
                // slot with bin >= first, and with bin >= l. Both move little
                // between curve points, so they are searched from where they
                // were.
                let (mut a, mut b) = (0usize, 0usize);
                let mut l: usize = 0;
                for (j, &(lo_bin, hi_bin)) in row.iter().enumerate() {
                    if lo_bin < l {
                        l = lo_bin;
                    }
                    let first = l;
                    if l < hi_bin.min(n) {
                        l = hi_bin.min(n);
                    }
                    a = lower_bound_from(bins, first, a);
                    b = lower_bound_from(bins, l, b);
                    for c in a..b {
                        ops.push((src as u8, j as u8, c as u8));
                    }
                }
                for c in lower_bound_from(bins, l.min(n), a)..bins.len() {
                    ops.push((src as u8, (EHMER_MAX - 1) as u8, c as u8));
                }
            };
            let (lo_curve, hi_curve) = curves[i];
            for k in lo_curve..=hi_curve {
                walk(&spread[k as usize], k as usize);
            }
            if i + 1 < P_BANDS {
                walk(&spread[i], i + 1);
            }
        }
        op_start[P_BANDS] = ops.len();
        ToneGeom {
            slot,
            ops,
            op_start,
        }
    }
}

/// The parts of `_vp_psy_init` that depend only on the block size, the rate
/// and `eighth_octave_lines`: the ATH curve, the octave and bark tables,
/// the noise-offset band positions and the tone-curve spreading geometry.
/// They are computed with exactly the expressions `vp_psy_init` used to
/// evaluate inline, once per process instead of once per quality.
struct PsyGeom {
    key: (usize, i64, i32),
    shiftoc: i32,
    firstoc: i64,
    total_octave_lines: i32,
    ath: Vec<f32>,
    octave: Vec<i32>,
    /// `toBARK(rate/(2*n)*k)` for k in `-99..=n` (at index `k + 99`): every
    /// argument the bark-window search can evaluate.
    bark: Vec<f64>,
    /// `toOC((i+.5)*rate/(2.*n))*2.` per bin, rounded to float.
    halfoc: Vec<f32>,
    tone: ToneGeom,
}

/// Lowest `lo` the bark-window search starts from.
const BARK_LO0: i64 = -99;

impl PsyGeom {
    fn new(n: usize, rate: i64, eighth_octave_lines: i32) -> Self {
        // libvorbis: `p->shiftoc = rint(log(gi->eighth_octave_lines*8.f)/log(2.f))-1;`
        // log() is the f64 overload: f32 arg promotes to f64. f64 division.
        // rint() rounds half-to-even.
        let arg = (eighth_octave_lines * 8) as f32 as f64;
        let two = 2.0_f32 as f64;
        let shiftoc = (arg.ln() / two.ln()).round_ties_even() as i32 - 1;

        // C: `toOC(.25f*rate*.5/n)` — `.25f*rate` is float, then double.
        let firstoc = (to_oc((0.25_f32 * rate as f32) as f64 * 0.5 / n as f64)
            * (1i64 << (shiftoc + 1)) as f64
            - eighth_octave_lines as f64) as i64;
        // C: `toOC((n+.25f)*rate*.5/n)*(1<<(shiftoc+1))+.5f` — `(n+.25f)*rate`
        // is float, the rest double.
        let maxoc = (to_oc(((n as f32 + 0.25) * rate as f32) as f64 * 0.5 / n as f64)
            * (1i64 << (shiftoc + 1)) as f64
            + 0.5) as i64;
        let total_octave_lines = (maxoc - firstoc + 1) as i32;

        // ATH setup
        let mut ath = vec![0.0_f32; n];
        {
            let mut j: usize = 0;
            for i in 0..(MAX_ATH - 1) {
                // C: `rint(fromOC((i+1)*.125-2.)*2*n/rate)` — all double.
                let endpos = (from_oc((i + 1) as f64 * 0.125 - 2.0) * 2.0 * n as f64
                    / rate as f64)
                    .round_ties_even() as usize;
                let base = ATH[i];
                if j < endpos {
                    let delta = if endpos > j {
                        (ATH[i + 1] - base) / (endpos - j) as f32
                    } else {
                        0.0
                    };
                    let mut b = base;
                    while j < endpos && j < n {
                        ath[j] = b + 100.0;
                        b += delta;
                        j += 1;
                    }
                }
            }
            while j < n {
                ath[j] = ath[j - 1];
                j += 1;
            }
        }

        // libvorbis psy.c uses INTEGER division `rate/(2*n)*i` for the bark
        // array, where rate is long and n/i are int (see vp_psy_init).
        let bin_hz = rate / (2 * n as i64);
        let bark = (BARK_LO0..=n as i64).map(|k| to_bark(bin_hz * k)).collect();

        let octave = (0..n)
            .map(|i| {
                // C: `toOC((i+.25f)*.5*rate/n)*(1<<(shiftoc+1))+.5f` —
                // `(i+.25f)` is float, everything after it double;
                // truncated into a long.
                (to_oc((i as f32 + 0.25) as f64 * 0.5 * rate as f64 / n as f64)
                    * (1i32 << (shiftoc + 1)) as f64
                    + 0.5) as i32
            })
            .collect();

        let halfoc = (0..n)
            .map(|i| {
                // C: `float halfoc=toOC((i+.5)*rate/(2.*n))*2.;` — double
                // argument and double result, rounded to float once on
                // assignment.
                (to_oc((i as f64 + 0.5) * rate as f64 / (2.0 * n as f64)) * 2.0) as f32
            })
            .collect();

        // C passes `rate*.5/n` (double) to setup_tone_curves' float binHz.
        let tone = ToneGeom::new((rate as f64 * 0.5 / n as f64) as f32, n);

        PsyGeom {
            key: (n, rate, eighth_octave_lines),
            shiftoc,
            firstoc,
            total_octave_lines,
            ath,
            octave,
            bark,
            halfoc,
            tone,
        }
    }

    /// `toBARK(rate/(2*n)*k)`.
    #[inline]
    fn bark_at(&self, k: i64) -> f64 {
        self.bark[(k - BARK_LO0) as usize]
    }
}

/// Distinct geometries kept. Real encodes use at most 8 (two block sizes
/// at each of two rates, for each of the two block-size pairs); the cap
/// only bounds callers of the internal `vp_psy_init` with arbitrary sizes.
const PSY_GEOM_CAP: usize = 16;

static PSY_GEOM: std::sync::Mutex<Vec<std::sync::Arc<PsyGeom>>> =
    std::sync::Mutex::new(Vec::new());

fn psy_geom(n: usize, rate: i64, eighth_octave_lines: i32) -> std::sync::Arc<PsyGeom> {
    use std::sync::Arc;
    let key = (n, rate, eighth_octave_lines);
    let find = |v: &[Arc<PsyGeom>]| v.iter().find(|g| g.key == key).cloned();
    if let Some(g) = find(&PSY_GEOM.lock().unwrap_or_else(|e| e.into_inner())) {
        return g;
    }
    // Built outside the lock; a racing builder computes identical data.
    let built = Arc::new(PsyGeom::new(n, rate, eighth_octave_lines));
    let mut cache = PSY_GEOM.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(g) = find(&cache) {
        return g;
    }
    if cache.len() < PSY_GEOM_CAP {
        cache.push(Arc::clone(&built));
    }
    built
}

// ---------------------------------------------------------------------------
// _vp_psy_init
// ---------------------------------------------------------------------------

pub fn vp_psy_init(
    vi: VorbisInfoPsy,
    gi: &VorbisInfoPsyGlobal,
    n: usize,
    rate: i64,
) -> VorbisLookPsy {
    vp_psy_init_reusing(vi, gi, n, rate, &[])
}

/// [`vp_psy_init`], taking the tone curves from an `earlier` look built from
/// the same inputs (n, rate, toneatt, tone_centerboost, tone_decay) instead
/// of recomputing them; setup_tone_curves is a pure function of those, and
/// the short (and long) block types usually share them.
pub(crate) fn vp_psy_init_reusing(
    vi: VorbisInfoPsy,
    gi: &VorbisInfoPsyGlobal,
    n: usize,
    rate: i64,
    earlier: &[VorbisLookPsy],
) -> VorbisLookPsy {
    let geom = psy_geom(n, rate, gi.eighth_octave_lines);
    // tone curves
    let bits = |v: &VorbisInfoPsy| {
        (
            v.toneatt.map(f32::to_bits),
            v.tone_centerboost.to_bits(),
            v.tone_decay.to_bits(),
        )
    };
    let same = earlier
        .iter()
        .find(|e| e.n == n && e.rate == rate && bits(&e.vi) == bits(&vi));
    let tonecurves = match same {
        Some(e) => e.tonecurves.clone(),
        None => setup_tone_curves(
            &vi.toneatt,
            &geom.tone,
            n,
            vi.tone_centerboost,
            vi.tone_decay,
        ),
    };
    let mut p = VorbisLookPsy {
        n,
        vi: vi.clone(),
        tonecurves,
        noiseoffset: vec![vec![0.0; n]; P_NOISECURVES],
        // ATH setup (quality-independent; see PsyGeom::new)
        ath: geom.ath.clone(),
        // octave array (likewise)
        octave: geom.octave.clone(),
        bark: vec![0; n],
        firstoc: geom.firstoc,
        shiftoc: geom.shiftoc,
        eighth_octave_lines: gi.eighth_octave_lines,
        total_octave_lines: geom.total_octave_lines,
        rate,
        m_val: 1.0,
    };
    debug_assert!(
        p.total_octave_lines <= MAX_OCTAVE_LINES as i32,
        "total_octave_lines={} exceeds MAX_OCTAVE_LINES; bump const",
        p.total_octave_lines
    );

    // AoTuV HF weighting
    p.m_val = 1.0;
    if rate < 26000 {
        p.m_val = 0.0;
    } else if rate < 38000 {
        p.m_val = 0.94;
    } else if rate > 46000 {
        p.m_val = 1.275;
    }

    // bark array
    //
    // libvorbis psy.c uses INTEGER division `rate/(2*n)*i` here, where rate
    // is long and n/i are int. The truncation matters: at rate=44100, n=1024
    // we get 44100/2048 = 21 (integer) instead of the exact 21.5332. The
    // window-edge calculation depends on this so the integer truncation must
    // be reproduced literally — using float division here flips a few bark
    // window `hi` values by 1 bin (visible in noise[] starting around bin
    // 118 for some inputs). `geom.bark_at(k)` is `toBARK(rate/(2*n)*k)`.
    {
        let mut lo: i64 = BARK_LO0;
        let mut hi: i64 = 1;
        for i in 0..n {
            let bark = geom.bark_at(i as i64) as f32;

            while lo + (vi.noisewindowlomin as i64) < i as i64
                && geom.bark_at(lo) < (bark - vi.noisewindowlo) as f64
            {
                lo += 1;
            }

            while hi <= n as i64
                && (hi < i as i64 + vi.noisewindowhimin as i64
                    || geom.bark_at(hi) < (bark + vi.noisewindowhi) as f64)
            {
                hi += 1;
            }

            p.bark[i] = ((lo - 1) << 16) + (hi - 1);

            if (10..=30).contains(&i) && crate::debug_flag!("LW_DEBUG_BARK") {
                eprintln!("LW_BARK_INIT n={} i={}: lo={} hi={}", n, i, lo, hi);
            }
        }
    }

    if crate::debug_dump::dump_enabled() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static FIRED: AtomicBool = AtomicBool::new(false);
        if !FIRED.swap(true, Ordering::Relaxed) {
            let mut bytes = Vec::new();
            for b in 0..P_BANDS {
                for lv in 0..P_LEVELS {
                    for v in p.tonecurves[b][lv].iter() {
                        bytes.extend_from_slice(&v.to_le_bytes());
                    }
                }
            }
            let _ = std::fs::write("/tmp/lewtoff-debug/r_tonecurves.bin", &bytes);
        }
    }

    // noise offsets
    let mut halfoc_dump = Vec::with_capacity(n);
    for i in 0..n {
        // C: `float halfoc=toOC((i+.5)*rate/(2.*n))*2.;` (see PsyGeom::new).
        let mut halfoc = geom.halfoc[i];
        halfoc_dump.push(halfoc);
        if halfoc < 0.0 {
            halfoc = 0.0;
        }
        if halfoc >= (P_BANDS - 1) as f32 {
            halfoc = (P_BANDS - 1) as f32;
        }
        let mut inthalfoc = halfoc as usize;
        if inthalfoc >= P_BANDS - 2 {
            inthalfoc = P_BANDS - 2;
        }
        let del = halfoc - inthalfoc as f32;

        // C: a*(1.-del) + b*del — first term in f64 (1. is double), second
        // term in f32 (b and del are float); they sum in f64 and cast to f32.
        for j in 0..P_NOISECURVES {
            let a = vi.noiseoff[j][inthalfoc];
            let b = vi.noiseoff[j][inthalfoc + 1];
            let term1 = (a as f64) * (1.0 - (del as f64)); // f64
            let term2 = (b * del) as f64; // f32 mul, promote
            p.noiseoffset[j][i] = (term1 + term2) as f32;
        }
    }

    if crate::debug_dump::dump_enabled() && n == 128 {
        use std::sync::atomic::{AtomicBool, Ordering};
        static FIRED: AtomicBool = AtomicBool::new(false);
        if !FIRED.swap(true, Ordering::Relaxed) {
            let mut bytes = Vec::with_capacity(halfoc_dump.len() * 4);
            for v in halfoc_dump.iter() {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            let _ = std::fs::write("/tmp/lewtoff-debug/r_halfoc.bin", &bytes);
            // dump noiseoffset[1] from short block specifically
            let mut nbytes = Vec::with_capacity(n * 4);
            for v in p.noiseoffset[1].iter() {
                nbytes.extend_from_slice(&v.to_le_bytes());
            }
            let _ = std::fs::write("/tmp/lewtoff-debug/r_noiseoffset_1.bin", &nbytes);
        }
    }

    p
}

// ---------------------------------------------------------------------------
// seed_curve
// ---------------------------------------------------------------------------

fn seed_curve(
    seed: &mut [f32],
    curves: &[[f32; EHMER_MAX + 2]; P_LEVELS],
    amp: f32,
    oc: i64,
    n: i64,
    linesper: i32,
    db_offset: f32,
    qlen: usize,
) {
    let choice = (((amp + db_offset - P_LEVEL_0) * 0.1) as i32)
        .max(0)
        .min(P_LEVELS as i32 - 1) as usize;
    let posts = &curves[choice];
    let curve = &posts[2..];
    let post0 = posts[0] as usize;
    let post1 = posts[1] as usize;
    let lp = linesper as i64;
    let seedptr0 = oc + (post0 as i64 - EHMER_OFFSET as i64) * lp - (linesper >> 1) as i64;

    // Pre-compute the iteration window where 0 < seedptr < n. The seedptr
    // recurrence is `seedptr0 + lp*(i-post0)`, so we can solve for i directly
    // and drop the per-iter `if seedptr > 0` and `if seedptr >= n` branches.
    // ceil_div for positive (a, b > 0): (a + b - 1) / b
    let ceil_div = |a: i64, b: i64| -> i64 { (a + b - 1) / b };
    let i_lo = if seedptr0 > 0 {
        post0
    } else {
        // smallest i such that seedptr0 + lp*(i-post0) > 0
        post0 + ceil_div(-seedptr0 + 1, lp) as usize
    };
    let i_hi = if seedptr0 >= n {
        post0
    } else {
        // smallest i such that seedptr0 + lp*(i-post0) >= n
        post0 + ceil_div(n - seedptr0, lp) as usize
    };
    let lo = i_lo.max(post0).min(post1);
    let hi = i_hi.max(lo).min(post1);

    if hi <= lo {
        return;
    }
    // `seed` is stored phase-major (see `vp_tonemask`): line `s` lives at
    // `(s % lp) * qlen + s / lp`. The strided cells this call touches,
    // `sp_lo + k*lp`, are therefore contiguous, so the max below is a plain
    // element-wise loop the compiler vectorizes (each lane does the scalar
    // add and compare-select).
    let lp_us = lp as usize;
    let sp_lo = (seedptr0 + lp * (lo as i64 - post0 as i64)) as usize;
    let start = (sp_lo % lp_us) * qlen + sp_lo / lp_us;
    let dst = &mut seed[start..start + (hi - lo)];
    for (d, &c) in dst.iter_mut().zip(&curve[lo..hi]) {
        let lin = amp + c;
        *d = if *d < lin { lin } else { *d };
    }
}

// ---------------------------------------------------------------------------
// seed_loop
// ---------------------------------------------------------------------------

fn seed_loop(
    p: &VorbisLookPsy,
    curves: &ToneCurves,
    f: &[f32],
    flr: &[f32],
    seed: &mut [f32],
    specmax: f32,
    qlen: usize,
) {
    let n = p.n;
    let db_offset = p.vi.max_curve_dB - specmax;
    let mut i = 0usize;
    while i < n {
        let mut max = f[i];
        let oc = p.octave[i] as i64;
        while i + 1 < n && p.octave[i + 1] as i64 == oc {
            i += 1;
            if f[i] > max {
                max = f[i];
            }
        }

        if max + 6.0 > flr[i] {
            let mut oc2 = oc >> p.shiftoc;
            if oc2 >= P_BANDS as i64 {
                oc2 = P_BANDS as i64 - 1;
            }
            if oc2 < 0 {
                oc2 = 0;
            }

            seed_curve(
                seed,
                &curves[oc2 as usize],
                max,
                p.octave[i] as i64 - p.firstoc,
                p.total_octave_lines as i64,
                p.eighth_octave_lines,
                db_offset,
                qlen,
            );
        }
        i += 1;
    }
}

// ---------------------------------------------------------------------------
// seed_chase
// ---------------------------------------------------------------------------

fn seed_chase(
    seeds: &mut [f32],
    linesper: i32,
    n: usize,
    posstack: &mut [usize],
    ampstack: &mut [f32],
) {
    // Stacks: n <= MAX_OCTAVE_LINES; at most n entries.
    let posstack = &mut posstack[..n];
    let ampstack = &mut ampstack[..n];
    let lp = linesper as usize;
    let seeds = &mut seeds[..n];
    let mut sp = 0usize;

    // libvorbis pushes every line; before pushing line i (once the stack
    // holds two entries) it pops the top while
    //   !(seeds[i] < amp[top]) && i < pos[top] + lp
    //     && amp[top] <= amp[top-1] && i < pos[top-1] + lp.
    // Positions on the stack strictly increase, so `i < pos[top-1] + lp`
    // implies `i < pos[top] + lp`; that test is dropped here. The `stack < 2`
    // case pushes unconditionally, as does the loop once a pop leaves one.
    // The top entry is also kept in registers (`ta`), so the common
    // no-pop test does not wait on the previous iteration's store.
    let mut ta = 0.0_f32;
    for i in 0..n {
        let v = seeds[i];
        while sp >= 2 {
            let (p2, a2) = (posstack[sp - 2], ampstack[sp - 2]);
            if v < ta || !(ta <= a2 && i < p2 + lp) {
                break;
            }
            sp -= 1;
            ta = a2;
        }
        posstack[sp] = i;
        ampstack[sp] = v;
        ta = v;
        sp += 1;
    }
    let stack = sp;
    let mut pos: usize = 0;
    for i in 0..stack {
        let endpos = if i < stack - 1 && ampstack[i + 1] > ampstack[i] {
            posstack[i + 1]
        } else {
            posstack[i] + lp + 1
        };
        let endpos = endpos.min(n);
        if pos < endpos {
            seeds[pos..endpos].fill(ampstack[i]);
            pos = endpos;
        }
    }
}

// ---------------------------------------------------------------------------
// max_seeds
// ---------------------------------------------------------------------------

fn max_seeds(
    p: &VorbisLookPsy,
    seed: &mut [f32],
    flr: &mut [f32],
    posstack: &mut [usize],
    ampstack: &mut [f32],
) {
    let n = p.total_octave_lines as usize;
    let linesper = p.eighth_octave_lines;

    seed_chase(seed, linesper, n, posstack, ampstack);

    let mut linpos: usize = 0;
    let mut pos = p.octave[0] as i64 - p.firstoc - (linesper >> 1) as i64;

    while linpos + 1 < p.n {
        let mut minV = if pos >= 0 && pos < n as i64 {
            seed[pos as usize]
        } else {
            NEGINF
        };
        let end = ((p.octave[linpos] as i64 + p.octave[linpos + 1] as i64) >> 1) - p.firstoc;
        if minV > p.vi.tone_abs_limit {
            minV = p.vi.tone_abs_limit;
        }
        if pos < end {
            // libvorbis steps pos to end, folding in each in-range line;
            // the fold is a select (no data-dependent branch).
            let a = (pos + 1).max(0);
            let b = end.min(n as i64 - 1);
            if a <= b {
                for &sv in &seed[a as usize..=b as usize] {
                    let take = ((sv > NEGINF) & (sv < minV)) | (minV == NEGINF);
                    minV = if take { sv } else { minV };
                }
            }
            pos = end;
        }

        let end2 = pos + p.firstoc;
        while linpos < p.n && p.octave[linpos] as i64 <= end2 {
            flr[linpos] = if flr[linpos] < minV { minV } else { flr[linpos] };
            linpos += 1;
        }
    }
    let minV = if n > 0 { seed[n - 1] } else { NEGINF };
    while linpos < p.n {
        if flr[linpos] < minV {
            flr[linpos] = minV;
        }
        linpos += 1;
    }
}

// ---------------------------------------------------------------------------
// bark_noise_hybridmp
// ---------------------------------------------------------------------------

fn bark_noise_hybridmp(
    n: usize,
    b: &[i64],
    f: &[f32],
    noise: &mut [f32],
    offset: f32,
    fixed: i32,
    pre: &mut [f32],
) {
    // Prefix-sum scratch (5 slots of MAX_LONG_HALF; n <= 2048, the q-1 long
    // block). Every [..n] entry is written before it is read.
    let mut slots = pre.chunks_exact_mut(crate::window::MAX_LONG_HALF);
    let mut slot = || &mut slots.next().unwrap()[..n];
    let big_n = slot();
    let big_x = slot();
    let big_xx = slot();
    let big_y = slot();
    let big_xy = slot();

    // Prefix-sum loops scoped so the running accumulators live in registers
    // (no per-iter spills to function-scope memory slots).
    {
        let mut tn = 0.0_f32;
        let mut tx = 0.0_f32;
        let mut txx = 0.0_f32;
        let mut ty = 0.0_f32;
        let mut txy = 0.0_f32;

        let mut y = f[0] + offset;
        if y < 1.0 {
            y = 1.0;
        }
        let w = y * y * 0.5;
        tn += w;
        tx += w;
        ty += w * y;

        big_n[0] = tn;
        big_x[0] = tx;
        big_xx[0] = txx;
        big_y[0] = ty;
        big_xy[0] = txy;

        let mut x = 1.0_f32;
        for i in 1..n {
            let mut y = f[i] + offset;
            if y < 1.0 {
                y = 1.0;
            }
            let w = y * y;
            tn += w;
            tx += w * x;
            txx += w * x * x;
            ty += w * y;
            txy += w * x * y;

            big_n[i] = tn;
            big_x[i] = tx;
            big_xx[i] = txx;
            big_y[i] = ty;
            big_xy[i] = txy;

            x += 1.0;
        }
    }

    let pre = Prefix {
        n: big_n,
        x: big_x,
        xx: big_xx,
        y: big_y,
        xy: big_xy,
    };

    // libvorbis runs three loops over i (x = i as float throughout; x += 1
    // from 0 is exact): while the bark window's lo is below 0 (mirrored),
    // while it lies inside [0, n), then the remainder extrapolates the last
    // A/B/D. The loop bounds depend only on `b`, so they are found first;
    // each body is element-wise in i and runs HYB_LANES at a time with
    // per-lane gathers (every lane does the scalar ops in scalar order).
    let lohi = |i: usize| (b[i] >> 16, (b[i] & 0xffff) as usize);
    let mut e2 = 0;
    while e2 < n {
        let (lo, hi) = lohi(e2);
        if lo >= 0 || lo.unsigned_abs() as usize >= n || hi >= n {
            break;
        }
        e2 += 1;
    }
    let mut e3 = e2;
    while e3 < n {
        let (lo, hi) = lohi(e3);
        if lo < 0 || lo as usize >= n || hi >= n {
            break;
        }
        e3 += 1;
    }
    let mirrored = |i: usize| {
        let (lo, hi) = lohi(i);
        (lo.unsigned_abs() as usize, hi)
    };
    let inside = |i: usize| {
        let (lo, hi) = lohi(i);
        (lo as usize, hi)
    };
    {
        let set = |_: f32, r: f32| {
            let r = if r < 0.0 { 0.0 } else { r };
            r - offset
        };
        hyb_run(&pre, 0, e2, true, mirrored, noise, set);
        hyb_run(&pre, e2, e3, false, inside, noise, set);
        let (big_a, big_b, big_d) = if e3 > e2 {
            let (lo, hi) = inside(e3 - 1);
            pre.abd(hi, lo, false)
        } else if e2 > 0 {
            let (lo, hi) = mirrored(e2 - 1);
            pre.abd(hi, lo, true)
        } else {
            (0.0, 0.0, 1.0)
        };
        hyb_tail(e3, n, big_a, big_b, big_d, noise, set);
    }

    if fixed <= 0 {
        return;
    }

    // Fixed-width window: hi = i + fixed/2, lo = hi - fixed. Same three
    // loops; the first runs while hi < n and lo < 0, the second while
    // hi < n (lo >= 0 from then on).
    let half = fixed as usize / 2;
    let fixed = fixed as usize;
    let e5 = n.saturating_sub(half).min(fixed - half);
    let e6 = e5.max(n.saturating_sub(half));
    let mirrored = |i: usize| (fixed - (i + half), i + half);
    let inside = |i: usize| (i + half - fixed, i + half);
    let lower = |old: f32, r: f32| if r - offset < old { r - offset } else { old };
    hyb_run(&pre, 0, e5, true, mirrored, noise, lower);
    {
        // Inside the spectrum the fixed window's rows are contiguous
        // (hi = i + half, lo = hi - fixed), so this pass is a plain
        // element-wise loop over shifted slices; same ops as hyb_run.
        let len = e6 - e5;
        let (h0, l0) = (e5 + half, e5 + half - fixed.min(e5 + half));
        fn win(v: &[f32], s: usize, len: usize) -> &[f32] {
            &v[s..s + len]
        }
        let hs = |v| win(v, h0, len);
        let ls = |v| win(v, l0, len);
        let (nh, xh, xxh, yh, xyh) = (hs(pre.n), hs(pre.x), hs(pre.xx), hs(pre.y), hs(pre.xy));
        let (nl, xl, xxl, yl, xyl) = (ls(pre.n), ls(pre.x), ls(pre.xx), ls(pre.y), ls(pre.xy));
        let out = &mut noise[e5..e6];
        for t in 0..len {
            let (a, b, d) = abd(
                nh[t] - nl[t],
                xh[t] - xl[t],
                xxh[t] - xxl[t],
                yh[t] - yl[t],
                xyh[t] - xyl[t],
            );
            let r = (a + (e5 + t) as f32 * b) / d;
            out[t] = lower(out[t], r);
        }
    }
    let (big_a, big_b, big_d) = if e6 > e5 {
        let (lo, hi) = inside(e6 - 1);
        pre.abd(hi, lo, false)
    } else if e5 > 0 {
        let (lo, hi) = mirrored(e5 - 1);
        pre.abd(hi, lo, true)
    } else {
        (0.0, 0.0, 1.0)
    };
    hyb_tail(e6, n, big_a, big_b, big_d, noise, lower);
}

/// Prefix sums of bark_noise_hybridmp's weighted least-squares terms.
struct Prefix<'a> {
    n: &'a [f32],
    x: &'a [f32],
    xx: &'a [f32],
    y: &'a [f32],
    xy: &'a [f32],
}

/// Lane count for bark_noise_hybridmp's element-wise loops (one 128-bit
/// vector on SSE2 and NEON).
const HYB_LANES: usize = 4;

impl Prefix<'_> {
    /// Window sums over (lo, hi]. `mirrored`: the window starts below 0 and
    /// `lo` is the mirrored index, so N/XX/Y add and X/XY subtract, exactly
    /// as libvorbis writes it.
    #[inline(always)]
    fn sums(&self, hi: usize, lo: usize, mirrored: bool) -> [f32; 5] {
        if mirrored {
            [
                self.n[hi] + self.n[lo],
                self.x[hi] - self.x[lo],
                self.xx[hi] + self.xx[lo],
                self.y[hi] + self.y[lo],
                self.xy[hi] - self.xy[lo],
            ]
        } else {
            [
                self.n[hi] - self.n[lo],
                self.x[hi] - self.x[lo],
                self.xx[hi] - self.xx[lo],
                self.y[hi] - self.y[lo],
                self.xy[hi] - self.xy[lo],
            ]
        }
    }

    #[inline(always)]
    fn abd(&self, hi: usize, lo: usize, mirrored: bool) -> (f32, f32, f32) {
        let [tn, tx, txx, ty, txy] = self.sums(hi, lo, mirrored);
        abd(tn, tx, txx, ty, txy)
    }
}

#[inline(always)]
fn abd(tn: f32, tx: f32, txx: f32, ty: f32, txy: f32) -> (f32, f32, f32) {
    let big_a = ty * txx - tx * txy;
    let big_b = tn * txy - tx * ty;
    let big_d = tn * txx - tx * tx;
    (big_a, big_b, big_d)
}

/// `noise[i] = op(noise[i], (A + x*B) / D)` for i in [start, end), x = i,
/// with A/B/D from the window `idx(i) = (lo, hi)`.
#[inline(always)]
fn hyb_run(
    pre: &Prefix,
    start: usize,
    end: usize,
    mirrored: bool,
    idx: impl Fn(usize) -> (usize, usize),
    noise: &mut [f32],
    op: impl Fn(f32, f32) -> f32,
) {
    let mut i = start;
    while i + HYB_LANES <= end {
        let mut t = [[0.0_f32; HYB_LANES]; 5];
        for l in 0..HYB_LANES {
            let (lo, hi) = idx(i + l);
            let s = pre.sums(hi, lo, mirrored);
            for k in 0..5 {
                t[k][l] = s[k];
            }
        }
        let mut r = [0.0_f32; HYB_LANES];
        for l in 0..HYB_LANES {
            let (a, b, d) = abd(t[0][l], t[1][l], t[2][l], t[3][l], t[4][l]);
            r[l] = (a + (i + l) as f32 * b) / d;
        }
        let dst: &mut [f32; HYB_LANES] = (&mut noise[i..i + HYB_LANES]).try_into().unwrap();
        for l in 0..HYB_LANES {
            dst[l] = op(dst[l], r[l]);
        }
        i += HYB_LANES;
    }
    while i < end {
        let (lo, hi) = idx(i);
        let (a, b, d) = pre.abd(hi, lo, mirrored);
        noise[i] = op(noise[i], (a + i as f32 * b) / d);
        i += 1;
    }
}

/// libvorbis's trailing loop: extrapolate the last fit for i in [start, end).
#[inline(always)]
fn hyb_tail(
    start: usize,
    end: usize,
    a: f32,
    b: f32,
    d: f32,
    noise: &mut [f32],
    op: impl Fn(f32, f32) -> f32,
) {
    let mut i = start;
    while i + HYB_LANES <= end {
        let dst: &mut [f32; HYB_LANES] = (&mut noise[i..i + HYB_LANES]).try_into().unwrap();
        for (l, v) in dst.iter_mut().enumerate() {
            *v = op(*v, (a + (i + l) as f32 * b) / d);
        }
        i += HYB_LANES;
    }
    while i < end {
        noise[i] = op(noise[i], (a + i as f32 * b) / d);
        i += 1;
    }
}

// ---------------------------------------------------------------------------
// _vp_noisemask
// ---------------------------------------------------------------------------

pub fn vp_noisemask(
    p: &VorbisLookPsy,
    logmdct: &[f32],
    logmask: &mut [f32],
    scratch: &mut PsyScratch,
) {
    let n = p.n;
    let pre = &mut scratch.prefix[..];
    let work = &mut scratch.work[..n];

    bark_noise_hybridmp(n, &p.bark, logmdct, logmask, 140.0, -1, pre);

    for i in 0..n {
        work[i] = logmdct[i] - logmask[i];
    }

    bark_noise_hybridmp(n, &p.bark, work, logmask, 0.0, p.vi.noisewindowfixed, pre);

    for i in 0..n {
        work[i] = logmdct[i] - work[i];
    }

    for i in 0..n {
        // C: `int dB = logmask[i] + .5;` — float + double literal, so the
        // add happens in double. An f32 add can round up across an integer
        // boundary (x.4999999 + 0.5 -> x+1.0) and pick the next compand step.
        let db = (logmask[i] as f64 + 0.5) as i32;
        let db = db.max(0).min(NOISE_COMPAND_LEVELS as i32 - 1) as usize;
        logmask[i] = work[i] + p.vi.noisecompand[db];
    }
}

// ---------------------------------------------------------------------------
// _vp_tonemask
// ---------------------------------------------------------------------------

pub fn vp_tonemask(
    p: &VorbisLookPsy,
    logfft: &[f32],
    logmask: &mut [f32],
    global_specmax: f32,
    local_specmax: f32,
    scratch: &mut PsyScratch,
) {
    let n = p.n;
    // Seed lines: Q5 max observed total_octave_lines is 777 (long block at
    // 44.1kHz); MAX_OCTAVE_LINES gives headroom for 48k. Fully written by
    // the transpose below before anything reads it.
    let seed = &mut scratch.seed[..p.total_octave_lines as usize];

    let mut att = local_specmax + p.vi.ath_adjatt;
    if att < p.vi.ath_maxatt {
        att = p.vi.ath_maxatt;
    }

    for i in 0..n {
        logmask[i] = p.ath[i] + att;
    }

    // seed_loop works on a phase-major copy of the seed lines (line `s` at
    // `(s % lp) * qlen + s / lp`), turning seed_curve's stride-lp scatter
    // into contiguous runs. Transposed back before max_seeds.
    let total = p.total_octave_lines as usize;
    let lp = p.eighth_octave_lines as usize;
    let qlen = total.div_ceil(lp);
    let phase = &mut scratch.phase[..qlen * lp];
    phase.fill(NEGINF);
    seed_loop(p, &p.tonecurves, logfft, logmask, phase, global_specmax, qlen);
    for (q, chunk) in seed.chunks_mut(lp).enumerate() {
        for (r, s) in chunk.iter_mut().enumerate() {
            *s = phase[r * qlen + q];
        }
    }
    if crate::debug_dump::dump_enabled() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static FIRED: AtomicBool = AtomicBool::new(false);
        if !FIRED.swap(true, Ordering::Relaxed) {
            let mut bytes = Vec::new();
            for v in seed.iter() {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            let _ = std::fs::write("/tmp/lewtoff-debug/r_tone_seed.bin", &bytes);
        }
    }
    max_seeds(p, seed, logmask, &mut scratch.posstack, &mut scratch.ampstack);
}

/// Reusable scratch for the per-block psy passes, so they don't zero tens
/// of KB of stack arrays on every call. Contents carry nothing between
/// calls: every entry is written before it is read.
pub struct PsyScratch {
    /// bark_noise_hybridmp's five prefix sums, MAX_LONG_HALF each.
    prefix: Vec<f32>,
    work: Vec<f32>,
    seed: Vec<f32>,
    phase: Vec<f32>,
    posstack: Vec<usize>,
    ampstack: Vec<f32>,
}

impl Default for PsyScratch {
    fn default() -> Self {
        Self::new()
    }
}

impl PsyScratch {
    pub fn new() -> Self {
        let half = crate::window::MAX_LONG_HALF;
        Self {
            prefix: vec![0.0; 5 * half],
            work: vec![0.0; half],
            seed: vec![0.0; MAX_OCTAVE_LINES],
            phase: vec![0.0; MAX_OCTAVE_LINES + MAX_LINESPER],
            posstack: vec![0; MAX_OCTAVE_LINES],
            ampstack: vec![0.0; MAX_OCTAVE_LINES],
        }
    }
}

// ---------------------------------------------------------------------------
// _vp_offset_and_mix
// ---------------------------------------------------------------------------

pub fn vp_offset_and_mix(
    p: &VorbisLookPsy,
    noise: &[f32],
    tone: &[f32],
    offset_select: usize,
    logmask: &mut [f32],
    mdct: &mut [f32],
    logmdct: &[f32],
) {
    let n = p.n;
    let cx = p.m_val;
    let toneatt = p.vi.tone_masteratt[offset_select];
    let noisemaxsupp = p.vi.noisemaxsupp;
    let noise = &noise[..n];
    let tone = &tone[..n];
    let offset = &p.noiseoffset[offset_select][..n];
    let logmask = &mut logmask[..n];

    let mix = |i: usize| {
        let mut val = noise[i] + offset[i];
        if val > noisemaxsupp {
            val = noisemaxsupp;
        }
        val
    };
    if offset_select != 1 {
        for i in 0..n {
            logmask[i] = mix(i).max(tone[i] + toneatt);
        }
        return;
    }
    let mdct = &mut mdct[..n];
    let logmdct = &logmdct[..n];
    for i in 0..n {
        let val = mix(i);
        logmask[i] = val.max(tone[i] + toneatt);

        // C: `float de, coeffi, cx; coeffi = -17.2;` — coeffi is float
        // (= -17.20000076f, NOT the f64 literal -17.2 = -17.19999999...).
        // Then `val = val - logmdct[i]` and `if(val > coeffi)` are both
        // f32 operations. The math `1.0 - (val-coeffi)*0.005*cx` promotes
        // to f64 only because 0.005/0.0003/1.0 are double literals; the
        // final assignment to `float de` truncates back to f32.
        //
        // Both arms are computed and selected (no data-dependent branch, so
        // the loop vectorizes); each is exactly libvorbis's expression.
        let coeffi: f32 = -17.2;
        let val_diff = val - logmdct[i];
        let d_hi = 1.0_f64 - (val_diff - coeffi) as f64 * 0.005_f64 * cx as f64;
        let de_hi = if d_hi < 0.0 { 0.0001_f32 } else { d_hi as f32 };
        let de_lo = (1.0_f64 - (val_diff - coeffi) as f64 * 0.0003_f64 * cx as f64) as f32;
        let de: f32 = if val_diff > coeffi { de_hi } else { de_lo };

        mdct[i] *= de;
    }
}

// ---------------------------------------------------------------------------
// _vp_ampmax_decay
// ---------------------------------------------------------------------------

pub fn vp_ampmax_decay(amp: f32, gi: &VorbisInfoPsyGlobal, n: usize, rate: i64) -> f32 {
    let secs = n as f32 / rate as f32;
    let result = amp + secs * gi.ampmax_att_per_sec;
    if result < -9999.0 { -9999.0 } else { result }
}

// ---------------------------------------------------------------------------
// flag_lossless (static helper for _vp_couple_quantize_normalize)
// ---------------------------------------------------------------------------

fn flag_lossless(
    limit: i32,
    prepoint: f32,
    postpoint: f32,
    mdct: &[f32],
    floor: &[f32],
    flag: &mut [i32],
    i: i32,
    jn: usize,
) {
    for j in 0..jn {
        let point = if j as i32 >= limit - i {
            postpoint
        } else {
            prepoint
        };
        let r = mdct[j].abs() / floor[j];
        flag[j] = if r < point { 0 } else { 1 };
    }
}

// ---------------------------------------------------------------------------
// noise_normalize
// ---------------------------------------------------------------------------

/// C `rint` (round half to even, default rounding mode), bit-identical to
/// `f64::round_ties_even`.
///
/// Baseline x86_64 (SSE2) has no round instruction, so `round_ties_even`
/// becomes a software routine call that also keeps loops scalar. For
/// |x| < 2^52, `(|x| + 2^52) - 2^52` is exactly |x| rounded half to even
/// (the sum lands in [2^52, 2^53), where the ulp is 1, and 2^52 is even);
/// copysign restores the sign (including -0). Larger |x|, infinities and
/// NaN are returned unchanged, as `round_ties_even` does.
#[inline(always)]
pub(crate) fn rint(x: f64) -> f64 {
    #[cfg(all(target_arch = "x86_64", not(target_feature = "sse4.1")))]
    {
        const TWO52: f64 = 4_503_599_627_370_496.0;
        let a = x.abs();
        if a < TWO52 { ((a + TWO52) - TWO52).copysign(x) } else { x }
    }
    #[cfg(not(all(target_arch = "x86_64", not(target_feature = "sse4.1"))))]
    {
        x.round_ties_even()
    }
}

fn noise_normalize(
    p: &VorbisLookPsy,
    limit: i32,
    r: &[f32],
    q: &mut [f32],
    f: &[f32],
    flags: Option<&[i32]>,
    mut acc: f32,
    i: i32,
    n: usize,
    out: &mut [i32],
) -> f32 {
    let vi = &p.vi;
    let start = if vi.normal_p != 0 {
        (vi.normal_start - i).max(0).min(n as i32) as usize
    } else {
        n
    };

    // force classic behavior
    acc = 0.0;

    // Element-wise (vectorizes): flagged entries keep their value, the
    // others get libvorbis's `out[j] = -rint(sqrt(ve))` (sqrt is the f64
    // overload, rint rounds half-to-even; Rust's f32::sqrt/round differ).
    let (r1, q1, f1, o1) = (&r[..start], &q[..start], &f[..start], &mut out[..start]);
    let quant = |rv: f32, qv: f32, fv: f32| {
        let s = rint(((qv / fv) as f64).sqrt());
        if rv < 0.0 { -(s as i32) } else { s as i32 }
    };
    match flags {
        None => {
            for (((o, &rv), &qv), &fv) in o1.iter_mut().zip(r1).zip(q1).zip(f1) {
                *o = quant(rv, qv, fv);
            }
        }
        Some(fl) => {
            for ((((o, &rv), &qv), &fv), &fl) in o1.iter_mut().zip(r1).zip(q1).zip(f1).zip(&fl[..start]) {
                let v = quant(rv, qv, fv);
                *o = if fl != 0 { *o } else { v };
            }
        }
    }

    // Stack-allocated sort scratch — partition <= MAX_PARTITION = 32.
    let mut sort_buf = [0usize; MAX_PARTITION];
    let mut sort_len = 0usize;

    for j in start..n {
        let skip = if let Some(fl) = flags {
            fl[j] != 0
        } else {
            false
        };
        if !skip {
            let ve = q[j] / f[j];
            if ve < 0.25 && (flags.is_none() || j as i32 >= limit - i) {
                acc += ve;
                sort_buf[sort_len] = j;
                sort_len += 1;
            } else {
                let s = rint((ve as f64).sqrt());
                out[j] = if r[j] < 0.0 { -(s as i32) } else { s as i32 };
                q[j] = out[j] as f32 * out[j] as f32 * f[j];
            }
        }
    }

    if sort_len > 0 {
        let sort_idx = &mut sort_buf[..sort_len];
        // Stable sort: matches glibc's merge-sort qsort (and the oracle's
        // lw_stable_qsort), so tied magnitudes keep ascending index order.
        sort_idx.sort_by(|&a, &b| {
            let fa = q[a];
            let fb = q[b];
            fb.partial_cmp(&fa).unwrap_or(std::cmp::Ordering::Equal)
        });
        for &k in sort_idx.iter() {
            // C compares float acc against double normal_thresh.
            if acc as f64 >= vi.normal_thresh {
                out[k] = unitnorm(r[k]) as i32;
                acc -= 1.0;
                q[k] = f[k];
            } else {
                out[k] = 0;
                q[k] = 0.0;
            }
        }
    }

    acc
}

// ---------------------------------------------------------------------------
// VorbisInfoMapping0 — minimal subset needed for _vp_couple_quantize_normalize
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct VorbisInfoMapping0 {
    pub coupling_steps: usize,
    pub coupling_mag: Vec<usize>,
    pub coupling_ang: Vec<usize>,
}

// ---------------------------------------------------------------------------
// _vp_couple_quantize_normalize
// ---------------------------------------------------------------------------

pub fn vp_couple_quantize_normalize(
    blobno: usize,
    g: &VorbisInfoPsyGlobal,
    p: &VorbisLookPsy,
    vi: &VorbisInfoMapping0,
    mdct: &mut [Vec<f32>],
    iwork: &mut [Vec<i32>],
    nonzero: &mut [i32],
    sliding_lowpass: i32,
    ch: usize,
) {
    let n = p.n;
    let partition = if p.vi.normal_p != 0 {
        p.vi.normal_partition as usize
    } else {
        16
    };
    let limit = g.coupling_pointlimit[p.vi.blockflag as usize][blobno];
    let prepoint = STEREO_THRESHHOLDS[g.coupling_prepointamp[blobno] as usize] as f32;
    let mut postpoint = STEREO_THRESHHOLDS[g.coupling_postpointamp[blobno] as usize] as f32;

    if n > 1000 {
        postpoint = STEREO_THRESHHOLDS_LIMITED[g.coupling_postpointamp[blobno] as usize] as f32;
    }

    // Stack-allocated per-block scratch. Q5 has partition <= 32, ch <= 2,
    // coupling_steps <= 1. Fixed bounds avoid the per-call heap-alloc churn
    // of vec![vec![0.0; partition]; ch].
    debug_assert!(
        partition <= MAX_PARTITION,
        "partition {} > MAX_PARTITION",
        partition
    );
    debug_assert!(ch <= MAX_CH, "ch {} > MAX_CH", ch);
    let mut raw: [[f32; MAX_PARTITION]; MAX_CH] = [[0.0; MAX_PARTITION]; MAX_CH];
    let mut quant: [[f32; MAX_PARTITION]; MAX_CH] = [[0.0; MAX_PARTITION]; MAX_CH];
    let mut floor_v: [[f32; MAX_PARTITION]; MAX_CH] = [[0.0; MAX_PARTITION]; MAX_CH];
    let mut flag: [[i32; MAX_PARTITION]; MAX_CH] = [[0; MAX_PARTITION]; MAX_CH];
    let mut nz: [i32; MAX_CH] = [0; MAX_CH];
    let mut acc: [f32; MAX_CH + MAX_COUPLING_STEPS] = [0.0; MAX_CH + MAX_COUPLING_STEPS];

    let mut i = 0;
    while i < n {
        let jn = partition.min(n - i);
        let mut track = 0usize;

        nz[..ch].copy_from_slice(nonzero);

        for fl in flag[..ch].iter_mut() {
            for v in fl[..jn].iter_mut() {
                *v = 0;
            }
        }

        for k in 0..ch {
            let iout = &mut iwork[k][i..i + jn];
            if nz[k] != 0 {
                for j in 0..jn {
                    floor_v[k][j] = FLOOR1_FROMDB_LOOKUP[iout[j] as usize];
                }
                flag_lossless(
                    limit,
                    prepoint,
                    postpoint,
                    &mdct[k][i..i + jn],
                    &floor_v[k][..jn],
                    &mut flag[k][..jn],
                    i as i32,
                    jn,
                );
                let lines = mdct[k][i..i + jn]
                    .iter()
                    .zip(&mut quant[k][..jn])
                    .zip(&mut raw[k][..jn])
                    .zip(&mut floor_v[k][..jn]);
                for (((&m, q), r), fl) in lines {
                    *q = m * m;
                    *r = if m < 0.0 { *q * -1.0 } else { *q };
                    *fl *= *fl;
                }
                acc[track] = noise_normalize(
                    p,
                    limit,
                    &raw[k][..jn],
                    &mut quant[k][..jn],
                    &floor_v[k][..jn],
                    None,
                    acc[track],
                    i as i32,
                    jn,
                    iout,
                );
            } else {
                for j in 0..jn {
                    floor_v[k][j] = 1e-10;
                    raw[k][j] = 0.0;
                    quant[k][j] = 0.0;
                    flag[k][j] = 0;
                    iout[j] = 0;
                }
                acc[track] = 0.0;
            }
            track += 1;
        }

        // coupling
        for step in 0..vi.coupling_steps {
            let mi = vi.coupling_mag[step];
            let ai = vi.coupling_ang[step];

            if nz[mi] != 0 || nz[ai] != 0 {
                nz[mi] = 1;
                nz[ai] = 1;

                // Branch-free form of libvorbis's per-line coupling: every
                // candidate is computed and the line's case selects one, so
                // the loop vectorizes. Each case does exactly libvorbis's ops.
                let (raw_m, raw_a) = pair_mut(&mut raw, mi, ai);
                let (quant_m, quant_a) = pair_mut(&mut quant, mi, ai);
                let (flag_m, flag_a) = pair_mut(&mut flag, mi, ai);
                let (floor_m, floor_a) = pair_mut(&mut floor_v, mi, ai);
                let (iw_m, iw_a) = pair_mut(iwork, mi, ai);
                let active = (sliding_lowpass - i as i32).clamp(0, jn as i32) as usize;
                let dipole = (limit - i as i32).clamp(0, jn as i32) as usize;
                let (raw_m, raw_a) = (&mut raw_m[..active], &mut raw_a[..active]);
                let (quant_m, quant_a) = (&mut quant_m[..active], &mut quant_a[..active]);
                let (flag_m, flag_a) = (&mut flag_m[..active], &mut flag_a[..active]);
                let (iw_m, iw_a) = (&mut iw_m[i..i + active], &mut iw_a[i..i + active]);
                for j in 0..active {
                    let (rm, ra) = (raw_m[j], raw_a[j]);
                    let (qm, qa) = (quant_m[j], quant_a[j]);
                    let lossless = flag_m[j] != 0 || flag_a[j] != 0;

                    // lossless coupling (wrapping ops: only pathological,
                    // huge input overflows, and then like release builds / C)
                    let (a, b) = (iw_m[j], iw_a[j]);
                    let m = if a.wrapping_abs() > b.wrapping_abs() { a } else { b };
                    let an = if m > 0 { a.wrapping_sub(b) } else { b.wrapping_sub(a) };
                    let flip = an >= m.wrapping_abs().wrapping_mul(2);
                    let (ll_m, ll_a) = if flip {
                        (m.wrapping_neg(), an.wrapping_neg())
                    } else {
                        (m, an)
                    };
                    let ll_raw = rm.abs() + ra.abs();
                    let ll_quant = qm + qa;

                    // lossy (point) coupling: dipole below `limit`, else
                    // elliptical
                    let dip_raw = rm + ra;
                    let dip_quant = dip_raw.abs();
                    let sum = rm.abs() + ra.abs();
                    let ell_raw = if rm + ra < 0.0 { -sum } else { sum };
                    let (ly_raw, ly_quant) = if j < dipole {
                        (dip_raw, dip_quant)
                    } else {
                        (ell_raw, sum)
                    };

                    raw_m[j] = if lossless { ll_raw } else { ly_raw };
                    quant_m[j] = if lossless { ll_quant } else { ly_quant };
                    raw_a[j] = if lossless { ra } else { 0.0 };
                    quant_a[j] = if lossless { qa } else { 0.0 };
                    flag_m[j] = if lossless { 1 } else { flag_m[j] };
                    flag_a[j] = 1;
                    iw_m[j] = if lossless { ll_m } else { a };
                    iw_a[j] = if lossless { ll_a } else { 0 };
                }
                for (fm, fa) in floor_m[..jn].iter_mut().zip(&mut floor_a[..jn]) {
                    *fm = *fa + *fm;
                    *fa = *fm; // both point to same sum now
                }

                acc[track] = noise_normalize(
                    p,
                    limit,
                    &raw[mi][..jn],
                    &mut quant[mi][..jn],
                    &floor_v[mi][..jn],
                    Some(&flag[mi][..jn]),
                    acc[track],
                    i as i32,
                    jn,
                    &mut iwork[mi][i..i + jn],
                );
                track += 1;
            }
        }

        i += partition;
    }

    for i in 0..vi.coupling_steps {
        if nonzero[vi.coupling_mag[i]] != 0 || nonzero[vi.coupling_ang[i]] != 0 {
            nonzero[vi.coupling_mag[i]] = 1;
            nonzero[vi.coupling_ang[i]] = 1;
        }
    }
}

/// Two distinct elements of `v`, both mutable (`a != b`).
fn pair_mut<T>(v: &mut [T], a: usize, b: usize) -> (&mut T, &mut T) {
    assert!(a != b);
    if a < b {
        let (lo, hi) = v.split_at_mut(b);
        (&mut lo[a], &mut hi[0])
    } else {
        let (lo, hi) = v.split_at_mut(a);
        (&mut hi[0], &mut lo[b])
    }
}

#[cfg(test)]
mod tests {
    use super::rint;

    fn check(x: f64) {
        let want = x.round_ties_even();
        let got = rint(x);
        assert!(
            got.to_bits() == want.to_bits() || (got.is_nan() && want.is_nan()),
            "rint({x:e}) = {got:e}, want {want:e}"
        );
    }

    #[test]
    fn rint_matches_round_ties_even() {
        let two52 = 4_503_599_627_370_496.0_f64;
        for x in [
            0.0,
            -0.0,
            0.5,
            1.5,
            2.5,
            -0.5,
            -2.5,
            0.49999999999999994,
            two52,
            two52 - 0.5,
            two52 - 1.0,
            two52 + 1.0,
            2.0 * two52,
            f64::MAX,
            f64::MIN_POSITIVE,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
        ] {
            check(x);
            check(-x);
            check(f64::from_bits(x.to_bits().wrapping_add(1)));
            check(f64::from_bits(x.to_bits().wrapping_sub(1)));
        }
        // Halves and their neighbours across the magnitude range.
        for k in 0..2000_u64 {
            let h = k as f64 + 0.5;
            for x in [h, h * 1024.0 + 0.5, h * 1e12 + 0.5] {
                check(x);
                check(-x);
                check(f64::from_bits(x.to_bits() + 1));
                check(f64::from_bits(x.to_bits() - 1));
            }
        }
        // Pseudo-random bit patterns (xorshift), all exponents.
        let mut s = 0x9e37_79b9_7f4a_7c15_u64;
        for _ in 0..1_000_000 {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            check(f64::from_bits(s));
            // and moderate magnitudes, where rounding actually happens
            check(f64::from_bits((s & 0x800f_ffff_ffff_ffff) | ((1000 + (s >> 52) % 80) << 52)));
        }
    }
}
