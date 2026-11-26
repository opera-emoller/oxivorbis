//! LPC (Linear Predictive Coding) for pre-stream extrapolation.
//!
//! Port of libvorbis lib/lpc.c: `vorbis_lpc_from_data` and `vorbis_lpc_predict`.
//! Used by `_preextrapolate_helper` in block.c to fill in pre-stream samples
//! using backward prediction from actual audio data.

/// Compute LPC coefficients from data using Levinson-Durbin algorithm.
/// Port of libvorbis `vorbis_lpc_from_data`.
///
/// `data`: n samples
/// `lpci`: output LPC coefficients (length m)
/// `n`: number of input samples
/// `m`: LPC order
/// Returns the prediction error energy.
pub fn lpc_from_data(data: &[f32], lpci: &mut [f32], n: usize, m: usize) -> f32 {
    let mut aut = vec![0.0f64; m + 1];
    let mut lpc = vec![0.0f64; m];

    // Autocorrelation, m+1 lag coefficients.
    // libvorbis lib/lpc.c::vorbis_lpc_from_data writes:
    //   double d = 0; for(i=j;i<n;i++) d += (double)data[i]*data[i-j];
    // The cast to (double) on the LEFT operand promotes; the right operand
    // (also float) gets promoted by the multiplication; product is in
    // double precision. We mirror that exactly — separate mul + add, no FMA.
    for j in 0..=m {
        let mut d = 0.0f64;
        for i in j..n {
            d += (data[i] as f64) * (data[i - j] as f64);
        }
        aut[j] = d;
    }
    if crate::debug_flag!("LW_DEBUG_LPC") {
        eprint!("LW_AUT[0..5]:");
        for val in aut.iter().take(5) {
            eprint!(" {:.15e}", val);
        }
        eprintln!();
    }
    // One-shot dump of the full f64 aut[] for layer-diff against libvorbis.
    if crate::debug_dump::dump_enabled() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static FIRED: AtomicBool = AtomicBool::new(false);
        if !FIRED.swap(true, Ordering::Relaxed) {
            let mut bytes = Vec::with_capacity((m + 1) * 8);
            for v in aut.iter() {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            let _ = std::fs::write("/tmp/lewtoff-debug/r_lpc_aut.bin", &bytes);
        }
    }

    // Generate LPC coefficients from autocorr values (Levinson-Durbin)
    let mut error = aut[0] * (1.0 + 1e-10);
    let epsilon = 1e-9 * aut[0] + 1e-10;

    for i in 0..m {
        if error < epsilon {
            for val in lpc[i..m].iter_mut() {
                *val = 0.0;
            }
            break;
        }

        let mut r = -aut[i + 1];
        for j in 0..i {
            r -= lpc[j] * aut[i - j];
        }
        r /= error;

        lpc[i] = r;
        for j in 0..i / 2 {
            let tmp = lpc[j];
            lpc[j] += r * lpc[i - 1 - j];
            lpc[i - 1 - j] += r * tmp;
        }
        // libvorbis: after `for(j=0;j<i/2;j++)` exits with j=i/2,
        //   if(i&1) lpc[j] += lpc[j]*r;  (i.e. lpc[i/2])
        if i & 1 != 0 {
            lpc[i / 2] += lpc[i / 2] * r;
        }

        error *= 1.0 - r * r;
    }

    // Slightly damp the filter
    {
        let g = 0.99f64;
        let mut damp = g;
        for val in lpc[..m].iter_mut() {
            *val *= damp;
            damp *= g;
        }
    }

    for (out, &val) in lpci[..m].iter_mut().zip(lpc[..m].iter()) {
        *out = val as f32;
    }

    if crate::debug_dump::dump_enabled() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static FIRED: AtomicBool = AtomicBool::new(false);
        if !FIRED.swap(true, Ordering::Relaxed) {
            let mut bytes = Vec::with_capacity(m * 8);
            for v in lpc.iter() {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            let _ = std::fs::write("/tmp/lewtoff-debug/r_lpc_coeffs.bin", &bytes);
            // Also dump the f32 lpci (after cast)
            let mut bytes32 = Vec::with_capacity(m * 4);
            for v in lpci.iter() {
                bytes32.extend_from_slice(&v.to_le_bytes());
            }
            let _ = std::fs::write("/tmp/lewtoff-debug/r_lpc_coeffs_f32.bin", &bytes32);
        }
    }

    error as f32
}

/// Predict n samples from LPC coefficients and prime values.
/// Port of libvorbis `vorbis_lpc_predict`.
///
/// `coeff`: LPC coefficients (length m)
/// `prime`: initial values (length m)
/// `m`: LPC order
/// `data`: output predicted samples (length n)
/// `n`: number of samples to predict
pub fn lpc_predict(coeff: &[f32], prime: &[f32], m: usize, data: &mut [f32], n: usize) {
    let mut work = vec![0.0f32; m + n];

    if !prime.is_empty() {
        work[..m].copy_from_slice(&prime[..m]);
    }

    if crate::debug_dump::dump_enabled() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static FIRED: AtomicBool = AtomicBool::new(false);
        if !FIRED.swap(true, Ordering::Relaxed) {
            // dump prime input + coeff input
            let mut p = Vec::new();
            for v in prime.iter() {
                p.extend_from_slice(&v.to_le_bytes());
            }
            let _ = std::fs::write("/tmp/lewtoff-debug/r_lpc_predict_prime.bin", &p);
            let mut c = Vec::new();
            for v in coeff.iter() {
                c.extend_from_slice(&v.to_le_bytes());
            }
            let _ = std::fs::write("/tmp/lewtoff-debug/r_lpc_predict_coeff.bin", &c);
        }
    }

    // Port of libvorbis lib/lpc.c::vorbis_lpc_predict.
    //   o=i; p=m;
    //   for(j=0;j<m;j++) y-=work[o++]*coeff[--p];
    // No FMA — matches the C source semantics exactly (separate mul + sub).
    // Tests against the no-FMA C debug harness pass byte-identically.
    let mut i = 0;
    while i < n {
        let mut y = 0.0f32;
        for (o, p) in (i..i + m).zip((0..m).rev()) {
            y -= work[o] * coeff[p];
        }
        data[i] = y;
        work[m + i] = y;
        i += 1;
        // A decaying prediction eventually runs on subnormals, which cost
        // a microcode assist per operation on x86. Once the whole window
        // is tiny, continue in the exact f64 emulation instead. (Other
        // targets, e.g. Apple Silicon, run subnormals at full speed.)
        if SLOW_SUBNORMALS && is_tiny(y) && work[i..i + m].iter().all(|&v| is_tiny(v)) {
            i = predict_tiny(coeff, m, &mut work, data, i, n);
        }
    }

    if crate::debug_dump::dump_enabled() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static FIRED: AtomicBool = AtomicBool::new(false);
        if !FIRED.swap(true, Ordering::Relaxed) {
            let mut bytes = Vec::with_capacity(n * 4);
            for v in data.iter() {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            let _ = std::fs::write("/tmp/lewtoff-debug/r_lpc_predict_out.bin", &bytes);
        }
    }
}

/// x86 takes a microcode assist on subnormal operands and results.
const SLOW_SUBNORMALS: bool = cfg!(any(target_arch = "x86", target_arch = "x86_64"));

/// `|v| < 2^-96`, tested on the bits (no FP op on a subnormal).
#[inline]
fn is_tiny(v: f32) -> bool {
    v.to_bits() & 0x7fff_ffff < (127 - 96) << 23
}

/// 2^e as f64, for e in the f64 normal range.
const fn pow2(e: i32) -> f64 {
    f64::from_bits(((1023 + e) as u64) << 52)
}

/// Widen an f32 to f64 exactly, without a subnormal f32 FP operand.
#[inline]
fn widen(v: f32) -> f64 {
    let b = v.to_bits();
    if b & 0x7f80_0000 != 0 {
        v as f64
    } else {
        let mag = (b & 0x7f_ffff) as f64 * pow2(-149);
        if b >> 31 != 0 { -mag } else { mag }
    }
}

/// Round an f64 to the nearest f32 (ties to even, gradual underflow),
/// returned as f64, without producing a subnormal f32 in hardware.
#[inline]
fn round_f32(x: f64) -> f64 {
    let a = x.abs();
    if a < pow2(-126) {
        // a + 1.5*2^-97 lies in [2^-97, 2^-96), whose ulp is 2^-149: the
        // f32 subnormal spacing. The subtraction is exact (Sterbenz).
        const MAGIC: f64 = f64::from_bits(((1023 - 97) << 52) | (1 << 51));
        ((a + MAGIC) - MAGIC).copysign(x)
    } else {
        x as f32 as f64
    }
}

/// Narrow an f64 that holds an f32 value, without a subnormal f32 FP result.
#[inline]
fn narrow(x: f64) -> f32 {
    let a = x.abs();
    if a < pow2(-126) {
        let mant = (a * pow2(149)) as u32;
        f32::from_bits(((x.is_sign_negative() as u32) << 31) | mant)
    } else {
        x as f32
    }
}

/// The `lpc_predict` recursion from output `i` on, while all values stay
/// below 2^-96, computed in f64 with explicit f32 rounding: bit-identical
/// to the f32 loop. Each product of two f32s is exact in f64 and then
/// rounded once. A difference of two f32s is exact in f64 whenever its
/// f32 result is subnormal; otherwise rounding to f64 and then to f32
/// equals direct f32 rounding (53 >= 2*24 + 2). No intermediate can
/// overflow or underflow in f64. Returns where the f32 loop resumes.
fn predict_tiny(
    coeff: &[f32],
    m: usize,
    work: &mut [f32],
    data: &mut [f32],
    mut i: usize,
    n: usize,
) -> usize {
    let c: Vec<f64> = coeff[..m].iter().map(|&v| widen(v)).collect();
    // w[k] mirrors work[i0 + k].
    let i0 = i;
    let mut w: Vec<f64> = work[i..i + m].iter().map(|&v| widen(v)).collect();
    w.reserve(n - i);
    // Trailing run of exact zeros in the window. Once all m values are
    // ±0 and the coefficients are finite, every product is ±0 and
    // y = +0 - (±0) = +0, so the rest of the output is +0.0.
    let finite = c.iter().all(|v| v.is_finite());
    let mut zeros = w.iter().rev().take_while(|&&v| v == 0.0).count();
    while i < n {
        if finite && zeros >= m {
            data[i..n].fill(0.0);
            return n;
        }
        let mut y = 0.0f64;
        for (o, p) in (i - i0..i - i0 + m).zip((0..m).rev()) {
            y = round_f32(y - round_f32(w[o] * c[p]));
        }
        let y32 = narrow(y);
        data[i] = y32;
        work[m + i] = y32;
        w.push(y);
        i += 1;
        zeros = if y == 0.0 { zeros + 1 } else { 0 };
        if !is_tiny(y32) {
            break;
        }
    }
    i
}

/// Compute pre-stream extrapolated samples using the same algorithm as
/// libvorbis `_preextrapolate_helper`.
///
/// `pcm`: the actual PCM samples (at least `order*2` samples needed)
/// `order`: LPC order (libvorbis uses 16)
/// `n_predict`: number of pre-stream samples to generate (typically `centerW = 1024`)
///
/// Returns a Vec of `n_predict` pre-stream samples, where index 0 corresponds
/// to the sample just before the stream start (index -1), and index n_predict-1
/// corresponds to sample at -(n_predict).
///
/// In libvorbis, the pre-stream buffer is the REVERSED time domain:
/// `work[j] = pcm[pcm_current - j - 1]` (reversed), then LPC, then predict,
/// then reverse back. This predicts BACKWARD from the stream start.
#[allow(dead_code)]
pub fn preextrapolate(pcm: &[f32], order: usize, n_predict: usize) -> Vec<f32> {
    let n_data = pcm.len();

    if n_data <= order * 2 {
        // Not enough data for LPC
        return vec![0.0; n_predict];
    }

    // Reverse the PCM data (libvorbis reverses the entire pcm_current worth)
    let mut work: Vec<f32> = pcm.iter().rev().cloned().collect();

    // Compute LPC from the reversed data
    let mut lpc_coeffs = vec![0.0f32; order];
    lpc_from_data(&work, &mut lpc_coeffs, n_data, order);

    // Predict n_predict samples forward in the reversed domain
    // The prime (initial values) are work[n_data-order..n_data]
    let prime: Vec<f32> = work[n_data - order..n_data].to_vec();
    let mut predicted = vec![0.0f32; n_predict];
    lpc_predict(&lpc_coeffs, &prime, order, &mut predicted, n_predict);

    // Append predicted to work
    work.extend_from_slice(&predicted);

    // Now extract the pre-stream samples by reversing back.
    // In libvorbis: `for j in 0..pcm_current: v->pcm[i][pcm_current-j-1] = work[j]`
    // This reversal writes work[0..pcm_current] back into pcm[pcm_current-1..0].
    // The predicted samples (work[n_data..n_data+n_predict]) get placed at
    // indices n_data+n_predict-1..n_data (= n_predict-1..0 in the output pcm).
    //
    // After the reversal, v->pcm[i][0..centerW] is filled with predicted values
    // in the right order. The sample at pcm[0] = work[pcm_current-1+n_predict] reversed...
    //
    // Actually let me trace through more carefully:
    // work[j] = pcm_reversed[j] for j in 0..n_data (= pcm_current in libvorbis)
    // work[n_data..n_data+n_predict] = predicted (= predicted samples in reverse time)
    //
    // The reversal back: v->pcm[i][pcm_current-j-1] = work[j]
    // For j in 0..n_data: this writes work[0..n_data] back into pcm[n_data-1..0]
    //   = restores original pcm
    // For j in n_data..n_data+n_predict: this writes predicted[0..n_predict] into
    //   pcm[n_data+n_predict-1-n_data..n_data+n_predict-1-n_data+n_predict]
    //   Wait, that's out of range...
    //
    // Actually libvorbis only does the reversal for j in 0..pcm_current = 0..n_data.
    // The predicted samples extend work beyond work[n_data-1], so they go into
    // v->pcm[i][n_data-1-n_data .. -1] which is negative indices!
    //
    // Hmm. Let me re-read _preextrapolate_helper:
    //
    //   for(j=0;j<v->pcm_current;j++)
    //     work[j]=v->pcm[i][v->pcm_current-j-1];   // work = reversed pcm[0..pcm_current]
    //   vorbis_lpc_from_data(work, lpc, pcm_current-centerW, order);
    //   vorbis_lpc_predict(lpc, work+pcm_current-centerW-order, order,
    //                      work+pcm_current-centerW, centerW);  // predict centerW samples
    //   for(j=0;j<v->pcm_current;j++)
    //     v->pcm[i][v->pcm_current-j-1]=work[j];   // write back
    //
    // KEY: the predict call fills work[pcm_current-centerW .. pcm_current-centerW+centerW]
    //      = work[pcm_current-centerW .. pcm_current]
    // Then the write-back writes work[j] -> pcm[pcm_current-j-1]:
    //   work[pcm_current-centerW] -> pcm[centerW-1]
    //   work[pcm_current-1]       -> pcm[0]
    //
    // So the predicted samples (work[pcm_current-centerW..pcm_current])
    // are written into pcm[0..centerW]!
    //
    // pcm[0..centerW] is the pre-stream region (the padded zeros).
    // pcm[centerW..pcm_current] is the actual audio data (unchanged).
    //
    // The predict fills work[pcm_current-centerW..pcm_current]:
    // Prime = work[pcm_current-centerW-order..pcm_current-centerW]
    //       = reversed audio data near the stream start
    // Predicted = work[pcm_current-centerW..pcm_current]
    //           = continuation of the reversed audio = backward extrapolation
    //
    // Write-back: work[pcm_current-centerW+k] -> pcm[centerW-1-k]
    // So predicted[0] -> pcm[centerW-1] (= virtual sample at -1)
    //    predicted[1] -> pcm[centerW-2] (= virtual sample at -2)
    //    ...
    //    predicted[centerW-1] -> pcm[0]  (= virtual sample at -centerW)
    //
    // So predicted[0] = virtual sample at -1 (closest to stream start)
    //    predicted[1] = virtual sample at -2
    //    etc.

    // Simpler implementation: predicted[i] corresponds to virtual sample at -(i+1)
    predicted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lpc_sine_prediction() {
        // Generate 256 samples of 440Hz sine at 44100Hz
        let n = 256usize;
        let rate = 44100.0f32;
        let freq = 440.0f32;
        let amp = 0.5f32;
        let pcm: Vec<f32> = (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / rate).sin())
            .collect();

        // Predict 128 pre-stream samples
        let predicted = preextrapolate(&pcm, 16, 128);

        // predicted[0] should be close to pcm[-1] = amp * sin(-2π*440/44100)
        let expected_neg1 = amp * (-2.0 * std::f32::consts::PI * freq / rate).sin();
        // LPC prediction may not be perfect with only 256 input samples,
        // but it should be in the right ballpark
        eprintln!(
            "predicted[0]={:.4} expected_neg1={:.4}",
            predicted[0], expected_neg1
        );
        // Just ensure it's not zero (prediction actually ran)
        assert!(
            predicted[0].abs() > 1e-6,
            "prediction should be non-zero for sine"
        );
    }

    /// The f64 emulation of the subnormal tail must match plain f32
    /// arithmetic bit for bit.
    #[test]
    fn predict_tiny_matches_f32() {
        let mut st = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = || {
            st ^= st << 13;
            st ^= st >> 7;
            st ^= st << 17;
            st
        };
        let mut unit = || (rnd() >> 11) as f64 / (1u64 << 53) as f64;
        for case in 0..1200 {
            let m = [1, 2, 16, 32][case % 4];
            // Mildly decaying random filters, sometimes unstable.
            let coeff: Vec<f32> = (0..m)
                .map(|_| ((unit() - 0.5) * 2.4 / m as f64) as f32)
                .collect();
            let e = -80.0 - 70.0 * unit();
            let sparse = case % 3 == 0;
            let prime: Vec<f32> = (0..m)
                .map(|_| {
                    let v = ((unit() - 0.5) * 2f64.powf(e)) as f32;
                    if sparse && unit() < 0.7 { 0.0 } else { v }
                })
                .collect();
            let n = 600;
            let mut got = vec![0.0f32; n];
            lpc_predict(&coeff, &prime, m, &mut got, n);
            let mut work = prime.clone();
            // predict_tiny directly (lpc_predict only uses it on x86).
            let mut tiny = vec![0.0f32; n];
            let mut tiny_work = prime.clone();
            tiny_work.resize(m + n, 0.0);
            let end = predict_tiny(&coeff, m, &mut tiny_work, &mut tiny, 0, n);
            for (i, g) in got.iter().enumerate() {
                let mut y = 0.0f32;
                for (o, p) in (i..i + m).zip((0..m).rev()) {
                    y -= work[o] * coeff[p];
                }
                work.push(y);
                assert_eq!(g.to_bits(), y.to_bits(), "case {case} i {i}");
                if i < end {
                    assert_eq!(tiny[i].to_bits(), y.to_bits(), "case {case} i {i}");
                }
            }
        }
        for _ in 0..200_000 {
            let x = (unit() - 0.5) * 2f64.powf(-100.0 - 60.0 * unit());
            assert_eq!(round_f32(x).to_bits(), (x as f32 as f64).to_bits(), "{x:e}");
            let v = x as f32;
            assert_eq!(widen(v).to_bits(), (v as f64).to_bits());
            assert_eq!(narrow(v as f64).to_bits(), v.to_bits());
        }
    }
}
