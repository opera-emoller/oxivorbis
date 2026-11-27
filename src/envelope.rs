//! Port of libvorbis lib/envelope.{h,c} for transient detection.
//!
//! libvorbis decides per-block whether to use short or long block size based
//! on a 7-band envelope analysis run on a small (n=128) MDCT of overlapping
//! 64-sample windows. Without this, ramp/transient inputs encode as long
//! blocks where libvorbis would emit short impulse blocks.
//!
//! This module mirrors `_ve_envelope_init`, `_ve_amp`, `_ve_envelope_search`
//! and `_ve_envelope_mark` enough to produce the same W-flag sequence that
//! libvorbis would emit for a given input. Numeric details (constants,
//! orderings, decay value, +-99999 sentinels) are kept literal.
//!
//! `encode.rs` drives [`EnvelopeLookup::step`] incrementally from its port
//! of `_ve_envelope_search` / `_ve_envelope_mark`.
#![allow(non_snake_case)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::items_after_test_module)]

use crate::mdct::mdct_forward_envelope;
use crate::psy::VorbisInfoPsyGlobal;
#[cfg(test)]
use crate::window::BlockSizes;

pub const VE_PRE: usize = 16;
pub const VE_WIN: usize = 4;
pub const VE_POST: usize = 2;
pub const VE_AMP: usize = VE_PRE + VE_POST - 1;
pub const VE_BANDS: usize = 7;
pub const VE_NEARDC: usize = 15;
pub const VE_MINSTRETCH: i32 = 2;
pub const VE_MAXSTRETCH: i32 = 12;

const ENV_WINLENGTH: usize = 128;
#[cfg(test)]
const ENV_SEARCHSTEP: usize = 64;

#[derive(Clone)]
struct FilterState {
    ampbuf: [f32; VE_AMP],
    ampptr: usize,
    near_dc: [f32; VE_NEARDC],
    near_dc_acc: f32,
    near_dc_partialacc: f32,
    nearptr: usize,
}

impl FilterState {
    fn new() -> Self {
        Self {
            ampbuf: [0.0; VE_AMP],
            ampptr: 0,
            near_dc: [0.0; VE_NEARDC],
            near_dc_acc: 0.0,
            near_dc_partialacc: 0.0,
            nearptr: 0,
        }
    }
}

struct Band {
    begin: usize,
    end: usize,
    window: Vec<f32>,
    total: f32,
}

pub struct EnvelopeLookup {
    ch: usize,
    minenergy: f32,
    mdct_win: [f32; ENV_WINLENGTH],
    bands: [Band; VE_BANDS],
    /// `filters[ch * VE_BANDS + band]` = state for that (channel, band).
    filters: Vec<FilterState>,
    stretch: i32,
}

impl EnvelopeLookup {
    pub fn new(channels: usize, minenergy: f32) -> Self {
        let mut mdct_win = [0.0f32; ENV_WINLENGTH];
        for i in 0..ENV_WINLENGTH {
            // libvorbis:
            //   e->mdct_win[i] = sin(i/(n-1.)*M_PI);  // sin() returns f64,
            //                                         // assigned to f32 truncates
            //   e->mdct_win[i] *= e->mdct_win[i];     // f32 * f32 with already-
            //                                         // truncated value
            // Mirror that exactly: sin in f64, truncate to f32, then square in f32.
            let s = (i as f64 / (ENV_WINLENGTH - 1) as f64 * std::f64::consts::PI).sin() as f32;
            mdct_win[i] = s * s;
        }
        if crate::debug_flag!("LW_DEBUG_MDCT_WIN") {
            for i in 0..8 {
                eprintln!("R_MDCT_WIN[{}] = 0x{:08x}", i, mdct_win[i].to_bits());
            }
        }

        let band_specs: [(usize, usize); VE_BANDS] =
            [(2, 4), (4, 5), (6, 6), (9, 8), (13, 8), (17, 8), (22, 8)];
        let bands: Vec<Band> = band_specs
            .iter()
            .map(|&(b, e)| {
                let n = e;
                let mut win = vec![0.0f32; n];
                let mut total = 0.0f32;
                for i in 0..n {
                    let w = ((i as f64 + 0.5) / n as f64 * std::f64::consts::PI).sin() as f32;
                    win[i] = w;
                    total += w;
                }
                Band {
                    begin: b,
                    end: e,
                    window: win,
                    // libvorbis: `e->band[j].total = 1./e->band[j].total;` —
                    // `1.` is double, so the division promotes to f64.
                    total: (1.0_f64 / total as f64) as f32,
                }
            })
            .collect();
        let bands: [Band; VE_BANDS] = bands.try_into().map_err(|_| ()).expect("VE_BANDS-sized");

        let filters = vec![FilterState::new(); VE_BANDS * channels];

        Self {
            ch: channels,
            minenergy,
            mdct_win,
            bands,
            filters,
            stretch: 0,
        }
    }

    /// One iteration of the mark loop in libvorbis `_ve_envelope_search`:
    /// bump `stretch`, run `_ve_amp` over every channel's 128-sample window
    /// `pcm[c][off..off + 128]`, and apply the `ret & 4` stretch reset.
    /// Returns `ret` (bit 0 = preecho, bit 1 = postecho, bit 2 = strong);
    /// the caller applies it to its marks (`mark[j]`, `mark[j±1]`).
    pub(crate) fn step(&mut self, gi: &VorbisInfoPsyGlobal, pcm: &[Vec<f32>], off: usize) -> i32 {
        let mut ret = 0i32;
        self.stretch += 1;
        if self.stretch > VE_MAXSTRETCH * 2 {
            self.stretch = VE_MAXSTRETCH * 2;
        }
        for i in 0..self.ch {
            ret |= self.amp(gi, &pcm[i][off..off + ENV_WINLENGTH], i * VE_BANDS);
        }
        if (ret & 4) != 0 {
            self.stretch = -1;
        }
        ret
    }

    /// Run envelope amplitude analysis on a 128-sample window.
    /// Updates `self.filters[band_offset..]` in place; returns the trigger
    /// flags (bit 0 = preecho hi, bit 1 = postecho lo, bit 2 = strong).
    fn amp(&mut self, gi: &VorbisInfoPsyGlobal, data: &[f32], band_offset: usize) -> i32 {
        let mut ret = 0i32;
        let stretch = VE_MINSTRETCH.max(self.stretch / 2);
        let mut penalty = gi.stretch_penalty - (self.stretch / 2 - VE_MINSTRETCH) as f32;
        if penalty < 0.0 {
            penalty = 0.0;
        }
        if penalty > gi.stretch_penalty {
            penalty = gi.stretch_penalty;
        }

        // window + MDCT in place. If every sample is below 2^-100 (e.g. the
        // subnormal end of the EOS extrapolation), the MDCT outputs are
        // far below 2^-76, so every square below rounds to +0.0 exactly as
        // for zeros: keep vec_out zero and skip the (on x86, very slow)
        // subnormal arithmetic.
        let mut vec_out = [0.0f32; ENV_WINLENGTH / 2];
        if data[..ENV_WINLENGTH]
            .iter()
            .any(|v| v.to_bits() & 0x7fff_ffff >= (127 - 100) << 23)
        {
            let mut vec = [0.0f32; ENV_WINLENGTH];
            for i in 0..ENV_WINLENGTH {
                vec[i] = data[i] * self.mdct_win[i];
            }
            let mut vec_in: [f32; ENV_WINLENGTH] = vec;
            mdct_forward_envelope(&vec_in, &mut vec_out);
            // Reuse vec to hold output spread/limited values across band loop:
            // first half stores spreaded values, second half unused.
            let _ = &mut vec_in;
        }

        // Near-DC accumulator decay term.
        let decay_init = {
            let band0 = self.filters.get_mut(band_offset).unwrap();
            // libvorbis: `temp = vec[0]*vec[0] + .7*vec[1]*vec[1] + .2*vec[2]*vec[2]`
            // The first term `vec[0]*vec[0]` is float*float (f32) since both
            // operands are float — `.7` only promotes its own multiplication
            // chain. The middle terms `.7*vec[1]*vec[1]` and `.2*vec[2]*vec[2]`
            // are evaluated in f64 (because `.7`/`.2` are double). The final
            // `f32 + f64 + f64` happens in f64; assignment to `float temp`
            // truncates back to f32.
            let term0 = vec_out[0] * vec_out[0]; // f32 * f32 = f32
            let term1 = 0.7_f64 * vec_out[1] as f64 * vec_out[1] as f64;
            let term2 = 0.2_f64 * vec_out[2] as f64 * vec_out[2] as f64;
            let temp = (term0 as f64 + term1 + term2) as f32;
            if crate::debug_flag!("LW_DEBUG_NEARDC") {
                use std::sync::atomic::{AtomicUsize, Ordering};
                static N: AtomicUsize = AtomicUsize::new(0);
                let n = N.fetch_add(1, Ordering::Relaxed);
                if n < 50 {
                    eprintln!(
                        "R_NEARDC call={} v0=0x{:08x} v1=0x{:08x} v2=0x{:08x} temp=0x{:08x}",
                        n,
                        vec_out[0].to_bits(),
                        vec_out[1].to_bits(),
                        vec_out[2].to_bits(),
                        temp.to_bits()
                    );
                }
            }
            let ptr = band0.nearptr;
            let decay;
            if ptr == 0 {
                band0.near_dc_acc = band0.near_dc_partialacc + temp;
                decay = band0.near_dc_acc;
                band0.near_dc_partialacc = temp;
            } else {
                band0.near_dc_acc += temp;
                decay = band0.near_dc_acc;
                band0.near_dc_partialacc += temp;
            }
            band0.near_dc_acc -= band0.near_dc[ptr];
            band0.near_dc[ptr] = temp;
            band0.nearptr += 1;
            if band0.nearptr >= VE_NEARDC {
                band0.nearptr = 0;
            }
            // libvorbis: `decay *= (1./(VE_NEARDC+1));` — `1./16` is double,
            // so decay promotes to f64, multiplies, truncates back to f32.
            let decay_scaled = ((decay as f64) * (1.0_f64 / (VE_NEARDC + 1) as f64)) as f32;
            // libvorbis: `decay = todB(&decay)*.5-15.f` where todB returns
            // FLOAT (the inline IEEE_FLOAT32 path). `.5` is double, so the
            // float result promotes to double for the multiply, then `15.f`
            // promotes to double for the subtract; assignment to float
            // truncates. Mirror that promotion chain explicitly.
            let decay_db = (todb(decay_scaled) as f64 * 0.5_f64 - 15.0_f32 as f64) as f32;
            if crate::debug_flag!("LW_DEBUG_DECAY") {
                use std::sync::atomic::{AtomicUsize, Ordering};
                static N: AtomicUsize = AtomicUsize::new(0);
                let n = N.fetch_add(1, Ordering::Relaxed);
                if n < 40 {
                    eprintln!(
                        "R_DECAY call={} decay_in=0x{:08x} decay_scaled=0x{:08x} decay_db=0x{:08x} acc=0x{:08x} pacc=0x{:08x}",
                        n,
                        decay.to_bits(),
                        decay_scaled.to_bits(),
                        decay_db.to_bits(),
                        band0.near_dc_acc.to_bits(),
                        band0.near_dc_partialacc.to_bits(),
                    );
                }
            }
            decay_db
        };

        // Spread + limit. Output stored in vec_out[i>>1] (half-size).
        let n2 = ENV_WINLENGTH / 2;
        let mut spread = [0.0f32; ENV_WINLENGTH / 4];
        let mut decay = decay_init;
        let minv = self.minenergy;
        let mut i = 0usize;
        while i < n2 {
            let val = vec_out[i] * vec_out[i] + vec_out[i + 1] * vec_out[i + 1];
            // libvorbis: `val = todB(&val)*.5f` — todB returns FLOAT, `.5f`
            // is FLOAT, so this is a pure f32 multiplication.
            let mut val_db = todb(val) * 0.5_f32;
            if val_db < decay {
                val_db = decay;
            }
            if val_db < minv {
                val_db = minv;
            }
            spread[i >> 1] = val_db;
            // libvorbis: `decay -= 8.;` — `8.` is double, so decay promotes
            // to f64, subtracts, truncates back. Mirror to keep precision
            // aligned. (Each j's decay starts as `decay_init` and decrements
            // by 8 per pair, so any drift here accumulates.)
            decay = ((decay as f64) - 8.0_f64) as f32;
            i += 2;
        }

        if crate::debug_flag!("LW_DEBUG_SPREAD") {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static N: AtomicUsize = AtomicUsize::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            if (32..=36).contains(&n) {
                let mut s = format!(
                    "R_SPREAD call={} decay_init=0x{:08x} spread:",
                    n,
                    decay_init.to_bits()
                );
                for k in 0..16 {
                    s.push_str(&format!(" 0x{:08x}", spread[k].to_bits()));
                }
                eprintln!("{}", s);
            }
        }
        // Per-band trigger detection.
        for j in 0..VE_BANDS {
            let band = &self.bands[j];
            let filter = &mut self.filters[band_offset + j];

            let mut acc = 0.0f32;
            for k in 0..band.end {
                acc += spread[k + band.begin] * band.window[k];
            }
            acc *= band.total;

            let this = filter.ampptr;
            let mut p = if this == 0 { VE_AMP - 1 } else { this - 1 };
            let postmax = crate::psy::c_max(acc, filter.ampbuf[p]);
            let postmin = crate::psy::c_min(acc, filter.ampbuf[p]);

            let mut premax = -99999.0f32;
            let mut premin = 99999.0f32;
            for _ in 0..stretch {
                if p == 0 {
                    p = VE_AMP - 1;
                } else {
                    p -= 1;
                }
                if filter.ampbuf[p] > premax {
                    premax = filter.ampbuf[p];
                }
                if filter.ampbuf[p] < premin {
                    premin = filter.ampbuf[p];
                }
            }

            let valmin = postmin - premin;
            let valmax = postmax - premax;

            filter.ampbuf[this] = acc;
            filter.ampptr += 1;
            if filter.ampptr >= VE_AMP {
                filter.ampptr = 0;
            }

            if valmax > gi.preecho_thresh[j] + penalty {
                ret |= 1;
                ret |= 4;
            }
            if valmin < gi.postecho_thresh[j] - penalty {
                ret |= 2;
            }
            if crate::debug_flag!("LW_DEBUG_AMP") {
                use std::sync::atomic::{AtomicUsize, Ordering};
                static N: AtomicUsize = AtomicUsize::new(0);
                let n = N.fetch_add(1, Ordering::Relaxed);
                if n < 2000 {
                    eprintln!(
                        "R_AMP band={} acc=0x{:08x} postmax=0x{:08x} premax=0x{:08x} valmax={:.6} thresh={:.6} stretch={}",
                        j,
                        acc.to_bits(),
                        postmax.to_bits(),
                        premax.to_bits(),
                        valmax,
                        gi.preecho_thresh[j],
                        stretch,
                    );
                }
            }
        }

        ret
    }
}

/// libvorbis `todB` (with VORBIS_IEEE_FLOAT32 defined, the actual path used):
///
/// ```c
/// static inline float todB(const float *x){
///   union { ogg_uint32_t i; float f; } ix;
///   ix.f = *x;
///   ix.i = ix.i & 0x7fffffff;          // absolute value
///   return (float)(ix.i * 7.17711438e-7f - 764.6161886f);
/// }
/// ```
///
/// Takes the absolute value, then uses **f32** constants (`7.17711438e-7f`,
/// `764.6161886f`) so the entire expression evaluates in f32. The cast to
/// float at the end is a no-op since everything is already f32. Earlier I
/// ported the macro form (returns f64), which is dead code in this build —
/// the inline function above is what's actually compiled.
#[inline]
fn todb(x: f32) -> f32 {
    let bits = x.to_bits() & 0x7fff_ffff;
    (bits as f32) * 7.17711438e-7_f32 - 764.6161886_f32
}

/// Compute the envelope-mark vector for an entire stream (test helper; the
/// encoder computes marks incrementally with [`EnvelopeLookup::step`]).
///
/// This runs `_ve_amp` over each 64-sample step of the input and returns a
/// boolean per step: true if libvorbis would have set `mark[j]` at that step.
/// The encoder uses these marks to decide block sizes per the libvorbis
/// `_ve_envelope_search` rule.
#[cfg(test)]
pub(crate) fn compute_marks(pcm_channels: &[Vec<f32>], gi: &VorbisInfoPsyGlobal) -> Vec<bool> {
    let ch = pcm_channels.len();
    if ch == 0 {
        return Vec::new();
    }
    let n_samples = pcm_channels[0].len();
    if n_samples < ENV_WINLENGTH {
        return Vec::new();
    }

    let mut e = EnvelopeLookup::new(ch, gi.preecho_minenergy);

    // libvorbis: `int last = v->pcm_current/searchstep - VE_WIN;`
    // The for loop processes j=0..last-1. Mirror that exactly so we don't
    // emit spurious marks for steps libvorbis skips at the buffer edge.
    let n_steps = (n_samples / ENV_SEARCHSTEP).saturating_sub(VE_WIN);
    let mut marks = vec![false; n_steps + VE_POST + 1];

    let dbg = crate::debug_flag!("LW_DEBUG_ENV");
    for j in 0..n_steps {
        let ret = e.step(gi, pcm_channels, j * ENV_SEARCHSTEP);
        marks[j + VE_POST] = false;
        if (ret & 1) != 0 {
            marks[j] = true;
            if j + 1 < marks.len() {
                marks[j + 1] = true;
            }
        }
        if (ret & 2) != 0 {
            marks[j] = true;
            if j > 0 {
                marks[j - 1] = true;
            }
        }
        if dbg && ret != 0 {
            eprintln!("R_ENV_MARK j={} ret={} stretch={}", j, ret, e.stretch);
        }
    }

    marks
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::psy::VorbisInfoPsyGlobal;

    fn make_q5_global() -> VorbisInfoPsyGlobal {
        let mut gi = VorbisInfoPsyGlobal::default();
        gi.eighth_octave_lines = 8;
        gi.preecho_thresh = [12.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0];
        gi.postecho_thresh = [-20.0, -20.0, -15.0, -15.0, -15.0, -15.0, -15.0];
        gi.stretch_penalty = 0.0;
        gi.preecho_minenergy = -80.0;
        gi.ampmax_att_per_sec = -6.0;
        gi
    }

    #[test]
    fn silence_no_marks() {
        let pcm = vec![vec![0.0f32; 44100]];
        let gi = make_q5_global();
        let marks = compute_marks(&pcm, &gi);
        let first_marks: Vec<usize> = marks
            .iter()
            .enumerate()
            .filter(|(_, b)| **b)
            .map(|(i, _)| i)
            .take(10)
            .collect();
        eprintln!("silence first marks at indices: {:?}", first_marks);
        assert_eq!(
            n_short_blocks_at_start_bs(BlockSizes::BS_256_2048, &marks),
            1,
            "silence: 1 short"
        );
    }

    #[test]
    fn ramp_two_short() {
        // libvorbis envelope detection runs on the pre-extrapolated PCM
        // buffer: 1024 LPC-predicted virtual samples (centerW) + audio. Mirror
        // that here so envelope state evolves the same way.
        use crate::encode::pre_extrap_for_test;

        let n = 44100usize;
        let raw_l: Vec<f32> = (0..n)
            .map(|i| (((i * 2) % 65536) as i32 - 32768) as f32 / 32768.0)
            .collect();
        let raw_r: Vec<f32> = (0..n)
            .map(|i| (((i * 2 + 1) % 65536) as i32 - 32768) as f32 / 32768.0)
            .collect();
        let pcm: Vec<Vec<f32>> = [&raw_l, &raw_r]
            .iter()
            .map(|raw| {
                let mut buf = Vec::with_capacity(1024 + raw.len());
                let pre = pre_extrap_for_test(&raw[..2112.min(raw.len())]);
                // pre[k] corresponds to virtual sample at -(k+1); insert
                // reversed into [0..1024] so that pcm[1024] = audio[0].
                let mut pre_rev = vec![0.0f32; 1024];
                for k in 0..1024 {
                    pre_rev[1024 - 1 - k] = pre[k];
                }
                buf.extend_from_slice(&pre_rev);
                buf.extend_from_slice(raw);
                buf
            })
            .collect();
        let gi = make_q5_global();
        let marks = compute_marks(&pcm, &gi);
        let first_marks: Vec<usize> = marks
            .iter()
            .enumerate()
            .filter(|(_, b)| **b)
            .map(|(i, _)| i)
            .take(20)
            .collect();
        eprintln!("ramp first marks at indices: {:?}", first_marks);
        let n_short = n_short_blocks_at_start_bs(BlockSizes::BS_256_2048, &marks);
        assert_eq!(n_short, 2, "ramp: 2 short (got {n_short})");
    }

    /// `amp` skips the MDCT for windows below 2^-100; check that the
    /// MDCT outputs there stay below 2^-76, so their squares are +0.0.
    #[test]
    fn tiny_window_mdct_squares_to_zero() {
        let max = f32::from_bits(((127 - 100) << 23) - 1);
        let mut st = 1u32;
        for case in 0..2000 {
            let mut x = [0.0f32; ENV_WINLENGTH];
            for (i, v) in x.iter_mut().enumerate() {
                st = st.wrapping_mul(1664525).wrapping_add(1013904223);
                let mag = if case % 2 == 0 {
                    max
                } else {
                    f32::from_bits(st >> 5 & 0x0d7f_ffff)
                };
                // Sign patterns: random, or aligned with an MDCT basis vector.
                let k = (case / 2 % 64) as f64;
                let c = ((std::f64::consts::PI / 64.0) * (i as f64 + 0.5 + 32.0) * (k + 0.5)).cos();
                let neg = if case % 4 < 2 { c < 0.0 } else { st & 1 != 0 };
                *v = if neg { -mag } else { mag };
            }
            let lookup = EnvelopeLookup::new(1, 0.0);
            for (v, w) in x.iter_mut().zip(lookup.mdct_win.iter()) {
                *v *= *w;
            }
            let mut out = [0.0f32; ENV_WINLENGTH / 2];
            mdct_forward_envelope(&x, &mut out);
            for v in out {
                assert!(
                    v.abs() < f32::from_bits((127 - 76) << 23),
                    "case {case}: {v:e}"
                );
                assert_eq!((v * v).to_bits(), 0);
                assert_eq!((0.7 * v as f64 * v as f64) as f32, 0.0);
            }
        }
    }
}

/// Count how many short blocks libvorbis would emit at the start of the
/// stream. For our constrained input space (silence / sine / ramp), this is
/// either 1 (no transient detected past the leading-edge ampbuf-init noise)
/// or 2 (transient detected before testW=2176 for 256/2048).
#[cfg(test)]
pub(crate) fn n_short_blocks_at_start_bs(bs: BlockSizes, marks: &[bool]) -> usize {
    // libvorbis _ve_envelope_search: cursor starts at bs1/2 (1024 → mark
    // index 16 for 256/2048). testW for the first decision = centerW +
    // bs[0]/4 + bs[1]/2 + bs[0]/4 (= 1024 + 64 + 1024 + 64 = 2176 → mark
    // index 34 for 256/2048; 2048 + 128 + 2048 + 128 = 4352 → 68 for
    // 512/4096). So only marks at cursor_idx ≤ i < test_idx inform the
    // first-block decision.
    //
    // The ampbuf-init noise only marks indices 0 and 1, which sit below the
    // cursor floor and thus never contribute to a decision.
    let center_w = bs.long / 2;
    let cursor_idx = center_w / ENV_SEARCHSTEP;
    let test_idx = (center_w + bs.short / 4 + bs.long / 2 + bs.short / 4) / ENV_SEARCHSTEP;
    let scan_end = test_idx.min(marks.len());
    for i in cursor_idx..scan_end {
        if marks[i] {
            return 2;
        }
    }
    1
}
