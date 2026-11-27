//! Forward MDCT, port of libvorbis 1.3.7 lib/mdct.c.
//!
//! Sizes: 128 (envelope) and every block size: 256/2048 (q0..q10 at
//! 26-200 kHz), 512/4096 (q-1), 512/1024 and 1024 (15-26 kHz), 512
//! (below 15 kHz).
//! Tables come from src/tables/trig.rs (dumped from libvorbis `mdct_init`).
//! No runtime transcendentals.

#![allow(clippy::needless_range_loop)]
#![allow(clippy::identity_op)]
#![allow(clippy::eq_op)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::approx_constant)]

use crate::tables::trig::{
    BITREV_128, BITREV_256, BITREV_512, BITREV_1024, BITREV_2048, BITREV_4096, SCALE_128, SCALE_256,
    SCALE_512, SCALE_1024, SCALE_2048, SCALE_4096, TRIG_128, TRIG_256, TRIG_512, TRIG_1024, TRIG_2048,
    TRIG_4096,
};

// Constants from mdct.h (float mode)
const CPI3_8: f32 = 0.38268343236508977175_f32;
const CPI2_8: f32 = 0.70710678118654752441_f32;
const CPI1_8: f32 = 0.92387953251128675613_f32;

const N: usize = 2048;
const N2: usize = N / 2; // 1024

/// Largest supported MDCT size (the q-1 long block).
pub(crate) const MDCT_MAX_N: usize = 4096;

/// One `mdct_lookup` (libvorbis `mdct_init(n)`): trig (n + n/4), bitrev
/// (n/4), scale (4/n).
pub(crate) struct MdctTables {
    pub n: usize,
    log2n: usize,
    trig: &'static [f32],
    bitrev: &'static [u32],
    scale: f32,
}

pub(crate) static MDCT_128: MdctTables = MdctTables {
    n: 128,
    log2n: 7,
    trig: &TRIG_128,
    bitrev: &BITREV_128,
    scale: SCALE_128,
};
pub(crate) static MDCT_256: MdctTables = MdctTables {
    n: 256,
    log2n: 8,
    trig: &TRIG_256,
    bitrev: &BITREV_256,
    scale: SCALE_256,
};
pub(crate) static MDCT_512: MdctTables = MdctTables {
    n: 512,
    log2n: 9,
    trig: &TRIG_512,
    bitrev: &BITREV_512,
    scale: SCALE_512,
};
pub(crate) static MDCT_1024: MdctTables = MdctTables {
    n: 1024,
    log2n: 10,
    trig: &TRIG_1024,
    bitrev: &BITREV_1024,
    scale: SCALE_1024,
};
pub(crate) static MDCT_2048: MdctTables = MdctTables {
    n: 2048,
    log2n: 11,
    trig: &TRIG_2048,
    bitrev: &BITREV_2048,
    scale: SCALE_2048,
};
pub(crate) static MDCT_4096: MdctTables = MdctTables {
    n: 4096,
    log2n: 12,
    trig: &TRIG_4096,
    bitrev: &BITREV_4096,
    scale: SCALE_4096,
};

/// Tables for MDCT size `n`. Panics unless `n` is 128, 256, 512, 1024, 2048
/// or 4096.
pub(crate) fn mdct_tables(n: usize) -> &'static MdctTables {
    match n {
        128 => &MDCT_128,
        256 => &MDCT_256,
        512 => &MDCT_512,
        1024 => &MDCT_1024,
        2048 => &MDCT_2048,
        4096 => &MDCT_4096,
        _ => panic!("mdct: unsupported size {n}"),
    }
}

/* Generic versions that accept a trig table slice */

// The 8/16/32-point butterflies work on fixed-size views (no bounds checks)
// and are force-inlined into the per-size MDCT instances: out of line, the
// x86_64 SLP vectorizer turns them into shuffle-heavy code that is slower
// than the bounds-checked scalar original.

#[inline(always)]
fn mdct_butterfly_8_g(x: &mut [f32; 8]) {
    let r0 = x[6] + x[2];
    let r1 = x[6] - x[2];
    let r2 = x[4] + x[0];
    let r3 = x[4] - x[0];

    x[6] = r0 + r2;
    x[4] = r0 - r2;

    let r0 = x[5] - x[1];
    let r2 = x[7] - x[3];
    x[0] = r1 + r0;
    x[2] = r1 - r0;

    let r0 = x[5] + x[1];
    let r1 = x[7] + x[3];
    x[3] = r2 + r3;
    x[1] = r2 - r3;
    x[7] = r1 + r0;
    x[5] = r1 - r0;
}

#[inline(always)]
fn mdct_butterfly_16_g(x: &mut [f32; 16]) {
    let r0 = x[1] - x[9];
    let r1 = x[0] - x[8];

    x[8] += x[0];
    x[9] += x[1];
    x[0] = (r0 + r1) * CPI2_8;
    x[1] = (r0 - r1) * CPI2_8;

    let r0 = x[3] - x[11];
    let r1 = x[10] - x[2];
    x[10] += x[2];
    x[11] += x[3];
    x[2] = r0;
    x[3] = r1;

    let r0 = x[12] - x[4];
    let r1 = x[13] - x[5];
    x[12] += x[4];
    x[13] += x[5];
    x[4] = (r0 - r1) * CPI2_8;
    x[5] = (r0 + r1) * CPI2_8;

    let r0 = x[14] - x[6];
    let r1 = x[15] - x[7];
    x[14] += x[6];
    x[15] += x[7];
    x[6] = r0;
    x[7] = r1;

    let (a, b) = x.split_at_mut(8);
    mdct_butterfly_8_g(a.try_into().unwrap());
    mdct_butterfly_8_g(b.try_into().unwrap());
}

#[inline(always)]
fn mdct_butterfly_32_g(x: &mut [f32; 32]) {
    let r0 = x[30] - x[14];
    let r1 = x[31] - x[15];
    x[30] += x[14];
    x[31] += x[15];
    x[14] = r0;
    x[15] = r1;

    let r0 = x[28] - x[12];
    let r1 = x[29] - x[13];
    x[28] += x[12];
    x[29] += x[13];
    x[12] = r0 * CPI1_8 - r1 * CPI3_8;
    x[13] = r0 * CPI3_8 + r1 * CPI1_8;

    let r0 = x[26] - x[10];
    let r1 = x[27] - x[11];
    x[26] += x[10];
    x[27] += x[11];
    x[10] = (r0 - r1) * CPI2_8;
    x[11] = (r0 + r1) * CPI2_8;

    let r0 = x[24] - x[8];
    let r1 = x[25] - x[9];
    x[24] += x[8];
    x[25] += x[9];
    x[8] = r0 * CPI3_8 - r1 * CPI1_8;
    x[9] = r1 * CPI3_8 + r0 * CPI1_8;

    let r0 = x[22] - x[6];
    let r1 = x[7] - x[23];
    x[22] += x[6];
    x[23] += x[7];
    x[6] = r1;
    x[7] = r0;

    let r0 = x[4] - x[20];
    let r1 = x[5] - x[21];
    x[20] += x[4];
    x[21] += x[5];
    x[4] = r1 * CPI1_8 + r0 * CPI3_8;
    x[5] = r1 * CPI3_8 - r0 * CPI1_8;

    let r0 = x[2] - x[18];
    let r1 = x[3] - x[19];
    x[18] += x[2];
    x[19] += x[3];
    x[2] = (r1 + r0) * CPI2_8;
    x[3] = (r1 - r0) * CPI2_8;

    let r0 = x[0] - x[16];
    let r1 = x[1] - x[17];
    x[16] += x[0];
    x[17] += x[1];
    x[0] = r1 * CPI3_8 + r0 * CPI1_8;
    x[1] = r1 * CPI1_8 - r0 * CPI3_8;

    let (a, b) = x.split_at_mut(16);
    mdct_butterfly_16_g(a.try_into().unwrap());
    mdct_butterfly_16_g(b.try_into().unwrap());
}

/// One 8-float step of libvorbis's generic/first butterflies: `hi` is the
/// block at x1, `lo` the block at x2, and `tw[k]` the (T[0], T[1]) twiddle
/// pair applied to element pair `6 - 2k`. Exactly the C statement order and
/// expressions; fixed-size views remove the per-access bounds checks.
#[inline(always)]
fn mdct_bf8(hi: &mut [f32; 8], lo: &mut [f32; 8], tw: [(f32, f32); 4]) {
    for (k, &(t0, t1)) in tw.iter().enumerate() {
        let e = 6 - 2 * k;
        let r0 = hi[e] - lo[e];
        let r1 = hi[e + 1] - lo[e + 1];
        hi[e] += lo[e];
        hi[e + 1] += lo[e + 1];
        lo[e] = r1 * t1 + r0 * t0;
        lo[e + 1] = r1 * t0 - r0 * t1;
    }
}

/// `x[x1..x1+8]` and `x[x2..x2+8]` as disjoint fixed-size views (x2 + 8 <= x1).
#[inline(always)]
fn bf8_views(x: &mut [f32], x1: usize, x2: usize) -> (&mut [f32; 8], &mut [f32; 8]) {
    let (lo, hi) = x.split_at_mut(x1);
    (
        (&mut hi[..8]).try_into().unwrap(),
        (&mut lo[x2..x2 + 8]).try_into().unwrap(),
    )
}

fn mdct_butterfly_first_g(
    trig: &[f32],
    t_base: usize,
    x: &mut [f32],
    x_base: usize,
    points: usize,
) {
    let mut x1 = x_base + points - 8;
    let mut x2 = x_base + (points >> 1) - 8;
    let mut t = t_base;

    loop {
        let tr: &[f32; 16] = (&trig[t..t + 16]).try_into().unwrap();
        let (hi, lo) = bf8_views(x, x1, x2);
        mdct_bf8(
            hi,
            lo,
            [(tr[0], tr[1]), (tr[4], tr[5]), (tr[8], tr[9]), (tr[12], tr[13])],
        );

        if x2 == x_base {
            break;
        }
        x1 -= 8;
        x2 -= 8;
        t += 16;
    }
}

fn mdct_butterfly_generic_g(
    trig: &[f32],
    t_base: usize,
    x: &mut [f32],
    x_base: usize,
    points: usize,
    trigint: usize,
) {
    let mut x1 = x_base + points - 8;
    let mut x2 = x_base + (points >> 1) - 8;
    let mut t = t_base;

    loop {
        let tw = |t: usize| (trig[t], trig[t + 1]);
        let tws = [
            tw(t),
            tw(t + trigint),
            tw(t + 2 * trigint),
            tw(t + 3 * trigint),
        ];
        t += 4 * trigint;
        let (hi, lo) = bf8_views(x, x1, x2);
        mdct_bf8(hi, lo, tws);

        if x2 == x_base {
            break;
        }
        x1 -= 8;
        x2 -= 8;
    }
}

#[inline(always)]
fn mdct_butterflies_g(trig: &[f32], log2n: usize, x: &mut [f32], x_base: usize, points: usize) {
    let mut stages: i32 = (log2n - 5) as i32;

    if {
        stages -= 1;
        stages
    } > 0
    {
        mdct_butterfly_first_g(trig, 0, x, x_base, points);
    }

    let mut i: usize = 1;
    loop {
        stages -= 1;
        if stages <= 0 {
            break;
        }
        for j in 0..(1usize << i) {
            mdct_butterfly_generic_g(trig, 0, x, x_base + (points >> i) * j, points >> i, 4 << i);
        }
        i += 1;
    }

    for c in x[x_base..x_base + points].chunks_exact_mut(32) {
        mdct_butterfly_32_g(c.try_into().unwrap());
    }
}

#[inline(always)]
fn mdct_bitreverse_g(trig: &[f32], bitrev: &[u32], n: usize, w: &mut [f32]) {
    let n2 = n / 2;
    let n4 = n / 4;
    // Reads come from the upper half (x0/x1 = w + n2 + bitrev[..]); writes
    // go to the lower half, w0 ascending through w[0..n4] and w1 descending
    // through w[n4..n2], four floats per step.
    let (lo, hi) = w[..n].split_at_mut(n2);
    let (lo_a, lo_b) = lo.split_at_mut(n4);
    // bitrev entries are even and below n2 (pair offsets into `hi`); the
    // mask keeps that visible to the optimizer and never changes a value.
    let mask = n2 - 2;
    let pair = |b: u32| {
        let x = b as usize & mask;
        (hi[x], hi[x + 1])
    };
    for (((b, t), oa), ob) in bitrev[..n4]
        .chunks_exact(4)
        .zip(trig[n..n + n4].chunks_exact(4))
        .zip(lo_a.chunks_exact_mut(4))
        .zip(lo_b.rchunks_exact_mut(4))
    {
        let (x00, x01) = pair(b[0]);
        let (x10, x11) = pair(b[1]);

        let r0 = x01 - x11;
        let r1 = x00 + x10;
        let r2 = r1 * t[0] + r0 * t[1];
        let r3 = r1 * t[1] - r0 * t[0];

        let r0 = (x01 + x11) * 0.5_f32;
        let r1 = (x00 - x10) * 0.5_f32;

        oa[0] = r0 + r2;
        ob[2] = r0 - r2;
        oa[1] = r1 + r3;
        ob[3] = r3 - r1;

        let (x00, x01) = pair(b[2]);
        let (x10, x11) = pair(b[3]);

        let r0 = x01 - x11;
        let r1 = x00 + x10;
        let r2 = r1 * t[2] + r0 * t[3];
        let r3 = r1 * t[3] - r0 * t[2];

        let r0 = (x01 + x11) * 0.5_f32;
        let r1 = (x00 - x10) * 0.5_f32;

        oa[2] = r0 + r2;
        ob[0] = r0 - r2;
        oa[3] = r1 + r3;
        ob[1] = r3 - r1;
    }
}

/// Forward MDCT of `input` (length `t.n`) into `output` (length `t.n / 2`),
/// bit-identical to libvorbis `mdct_forward(&transform[W][0], in, out)`.
pub(crate) fn mdct_forward_with(t: &MdctTables, input: &[f32], output: &mut [f32]) {
    let n = t.n;
    assert_eq!(input.len(), n, "mdct: input length must equal MDCT size");
    assert_eq!(output.len(), n / 2, "mdct: output length must be n/2");
    // One instance per size, on that size's static tables, so every length
    // and stage count is a compile-time constant. Stack scratch of exactly
    // n floats (avoids per-call heap alloc and keeps the frequent 128-point
    // envelope MDCT cheap). Every element of `w` is written before it is
    // read, so its contents don't matter.
    match n {
        128 => mdct_forward_core(input, output, &MDCT_128, &mut [0f32; 128]),
        256 => mdct_forward_core(input, output, &MDCT_256, &mut [0f32; 256]),
        512 => mdct_forward_core(input, output, &MDCT_512, &mut [0f32; 512]),
        1024 => mdct_forward_core(input, output, &MDCT_1024, &mut [0f32; 1024]),
        2048 => mdct_forward_core(input, output, &MDCT_2048, &mut [0f32; 2048]),
        4096 => mdct_forward_core(input, output, &MDCT_4096, &mut [0f32; MDCT_MAX_N]),
        _ => panic!("mdct: unsupported size {n}"),
    }
}

/// [`mdct_forward_with`] for `n = input.len()` (128, 256, 512, 1024, 2048
/// or 4096).
pub(crate) fn mdct_forward_n(input: &[f32], output: &mut [f32]) {
    mdct_forward_with(mdct_tables(input.len()), input, output);
}

/// One pre-rotation segment of `mdct_forward`: for each step, `x0` walks
/// `x0s` downward and `x1` walks `x1s` upward four floats at a time, and the
/// twiddle pair walks `tp` downward. `f(x0, x1)` gives C's `(r0, r1)`.
#[inline(always)]
fn mdct_prerot(
    x0s: &[f32],
    x1s: &[f32],
    tp: &[f32],
    ws: &mut [f32],
    f: impl Fn(&[f32], &[f32]) -> (f32, f32),
) {
    for (((x0, x1), t), wo) in x0s
        .rchunks_exact(4)
        .zip(x1s.chunks_exact(4))
        .zip(tp.rchunks_exact(2))
        .zip(ws.chunks_exact_mut(2))
    {
        let (r0, r1) = f(x0, x1);
        wo[0] = r1 * t[1] + r0 * t[0];
        wo[1] = r1 * t[0] - r0 * t[1];
    }
}

#[inline(always)]
fn mdct_forward_core(input: &[f32], output: &mut [f32], tab: &MdctTables, w: &mut [f32]) {
    let n = tab.n;
    let log2n = tab.log2n;
    let trig = tab.trig;
    let bitrev = tab.bitrev;
    let scale = tab.scale;
    let n2 = n / 2;
    let n4 = n / 4;
    let n8 = n / 8;

    // Pre-rotation into w[n2..n]. C walks x0 down from in+n2+n4 (later
    // from in+n) and x1 up from in+n2+n4+1 (later from in+1), reading
    // x0[0], x0[2], x1[0], x1[2]; in quarters q0..q3 of `input` that is:
    //   steps [0, n/16):       x0 down q2,    x1 up q3
    //   steps [n/16, 3n/16):   x0 down q0q1,  x1 up q0q1
    //   steps [3n/16, n/4):    x0 down q3,    x1 up q2
    // x1 reads the odd elements of each aligned 4-float chunk.
    let input = &input[..n];
    let (q01, q23) = input.split_at(n2);
    let (q2, q3) = q23.split_at(n4);
    let (w_a, w_rest) = w[n2..n].split_at_mut(n8);
    let (w_b, w_c) = w_rest.split_at_mut(n2 - 2 * n8);
    mdct_prerot(q2, q3, &trig[n2 - n8..n2], w_a, |x0, x1| {
        (x0[2] + x1[1], x0[0] + x1[3])
    });
    mdct_prerot(q01, q01, &trig[n8..n2 - n8], w_b, |x0, x1| {
        (x0[2] - x1[1], x0[0] - x1[3])
    });
    mdct_prerot(q3, q2, &trig[..n8], w_c, |x0, x1| {
        (-x0[2] - x1[1], -x0[0] - x1[3])
    });

    mdct_butterflies_g(trig, log2n, w, n2, n2);
    mdct_bitreverse_g(trig, bitrev, n, w);

    // Post-rotation: output[k] ascending and output[n2-1-k] descending.
    let (out_lo, out_hi) = output[..n2].split_at_mut(n4);
    for (((wp, t), o0), o1) in w[..n2]
        .chunks_exact(2)
        .zip(trig[n2..n].chunks_exact(2))
        .zip(out_lo.iter_mut())
        .zip(out_hi.iter_mut().rev())
    {
        *o0 = (wp[0] * t[0] + wp[1] * t[1]) * scale;
        *o1 = (wp[0] * t[1] - wp[1] * t[0]) * scale;
    }
}

/// The 2048-point MDCT (public for the integration tests).
pub fn mdct_forward(input: &[f32; N], output: &mut [f32; N2]) {
    mdct_forward_with(&MDCT_2048, input, output);
}

/// MDCT for envelope detection (n=128, used by `_ve_amp`; fixed in libvorbis
/// regardless of blocksizes).
pub(crate) fn mdct_forward_envelope(input: &[f32; 128], output: &mut [f32; 64]) {
    mdct_forward_with(&MDCT_128, input, output);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::dsp_oracle::{assert_manifest, hash_f32, hash_u32, lcg_input};

    #[test]
    fn dsp_manifest_mdct_tables_match_libvorbis() {
        for n in [128, 256, 512, 1024, 2048, 4096] {
            let t = mdct_tables(n);
            assert_eq!(t.n, n);
            assert_eq!(1usize << t.log2n, n);
            assert_manifest(
                &format!("table.mdct_trig.{n}"),
                t.trig.len(),
                hash_f32(t.trig),
            );
            assert_manifest(
                &format!("table.mdct_bitrev.{n}"),
                t.bitrev.len(),
                hash_u32(t.bitrev),
            );
            assert_manifest(&format!("table.mdct_scale.{n}"), 1, hash_f32(&[t.scale]));
        }
    }

    #[test]
    fn dsp_manifest_mdct_forward_matches_libvorbis() {
        for n in [128usize, 256, 512, 1024, 2048, 4096] {
            for seed in 1u32..=3 {
                let input = lcg_input(n, seed * 7919 + n as u32);
                let mut out = vec![0.0f32; n / 2];
                mdct_forward_n(&input, &mut out);
                assert_manifest(&format!("mdct.{n}.seed{seed}"), n / 2, hash_f32(&out));
            }
        }
    }

    /// The fixed-size 128-point envelope MDCT must stay bit-identical to
    /// the generic entry point.
    #[test]
    fn envelope_wrapper_matches_generic() {
        let inp = lcg_input(128, 97);
        let mut a = [0.0f32; 64];
        mdct_forward_envelope(inp.as_slice().try_into().unwrap(), &mut a);
        let mut b = vec![0.0f32; 64];
        mdct_forward_n(&inp, &mut b);
        assert!(a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()));
    }
}
