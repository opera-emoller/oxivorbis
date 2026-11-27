// Literal port of drftf1 + butterfly helpers from libvorbis/lib/smallft.c.
// Variable names, loop structure, and indexing match the C source exactly.
// Only the forward transform is needed; no backward path.
//
// Supported sizes are the Vorbis blocksizes 256/2048 and 512/4096 (libvorbis
// inits one drft per blocksize, block.c `drft_init(&b->fft_look[i],
// ci->blocksizes[i])`). All are powers of two whose factorizations only
// produce radix-2 and radix-4 factors, so dradfg (generic radix) is never
// reached.

#![allow(clippy::too_many_arguments)]
#![allow(clippy::needless_range_loop)]
#![allow(non_snake_case)]

use crate::tables::drft::{
    DRFT_SPLIT_256, DRFT_SPLIT_512, DRFT_SPLIT_2048, DRFT_SPLIT_4096, DRFT_WA_256, DRFT_WA_512,
    DRFT_WA_2048, DRFT_WA_4096,
};

// ---------------------------------------------------------------------------
// dradf2  (radix-2 forward butterfly)
// Port of smallft.c lines 113-166.
//
// Same slicing scheme as dradf4 below: the two input halves and each k's
// 2*ido output block are split into exactly-sized slices, so the loops are
// bounds-check free. Every element is the C expression, unchanged.
// ---------------------------------------------------------------------------
fn dradf2(ido: usize, l1: usize, cc: &[f32], ch: &mut [f32], wa1: &[f32]) {
    let t0 = l1 * ido;
    let (c0, c1) = cc[..2 * t0].split_at(t0);
    let ch = &mut ch[..2 * t0];

    // Twiddle pairs per k (C's `i` from 2 below ido in steps of 2).
    let m = if ido > 2 { (ido - 1) / 2 } else { 0 };
    let w1 = &wa1[..2 * m];
    // Opaque imaginary-half index; see dradf4_general.
    let im = std::hint::black_box(1usize);

    for ((o, a0), a1) in ch
        .chunks_exact_mut(2 * ido)
        .zip(c0.chunks_exact(ido))
        .zip(c1.chunks_exact(ido))
    {
        // DC butterfly
        o[0] = a0[0] + a1[0];
        o[2 * ido - 1] = a0[0] - a1[0];

        if ido < 2 {
            continue;
        }
        // Twiddle loop. C's ch[t6-1..] ascends from o[1], ch[t4-1..]
        // descends to o[2*ido-2].
        let (q0, q1) = o.split_at_mut(ido);
        let ra = &mut q0[1..1 + 2 * m];
        let rb = &mut q1[ido - 1 - 2 * m..ido - 1];
        let (x0, x1) = (&a0[1..1 + 2 * m], &a1[1..1 + 2 * m]);
        if ido >= 64 {
            dradf2_row(x0, x1, w1, ra, rb);
        } else {
            dradf2_row_body(x0, x1, w1, ra, rb, im);
        }
        if ido % 2 == 1 {
            continue;
        }

        // L105: Nyquist terms (ido even, or ido==2 falls through here)
        o[ido] = -a1[ido - 1];
        o[ido - 1] = a0[ido - 1];
    }
}

/// One k's dradf2 twiddle loop, out of line for `noalias` arguments; see
/// [`dradf4_row`].
#[inline(never)]
fn dradf2_row(x0: &[f32], x1: &[f32], w1: &[f32], ra: &mut [f32], rb: &mut [f32]) {
    dradf2_row_body(x0, x1, w1, ra, rb, 1);
}

/// The dradf2 twiddle loop: `x0`/`x1` are the two input halves' (re, im)
/// pairs from i = 2 on, `ra` the ascending and `rb` the descending output
/// pairs, `im` the imaginary-half index (always 1).
#[inline(always)]
fn dradf2_row_body(x0: &[f32], x1: &[f32], w1: &[f32], ra: &mut [f32], rb: &mut [f32], im: usize) {
    let it = x0
        .chunks_exact(2)
        .zip(x1.chunks_exact(2))
        .zip(w1.chunks_exact(2))
        .zip(ra.chunks_exact_mut(2))
        .zip(rb.rchunks_exact_mut(2));
    for ((((x0, x1), w), oa), ob) in it {
        let tr2 = w[0] * x1[0] + w[1] * x1[1];
        let ti2 = w[0] * x1[1] - w[1] * x1[0];
        oa[im] = x0[1] + ti2;
        ob[im] = ti2 - x0[1];
        oa[0] = x0[0] + tr2;
        ob[0] = x0[0] - tr2;
    }
}

// ---------------------------------------------------------------------------
// dradf4  (radix-4 forward butterfly)
// Port of smallft.c lines 168-268.
//
// The C walks running offsets (t1..t6) through `cc` and `ch`. Here the four
// input quarters `cc[q*t0..]` and the 4*ido output block of each k are split
// into disjoint, exactly-sized slices up front, so every access is
// bounds-check free and the twiddle loop is a plain zip over (re, im) pairs.
// Each output element is still produced by exactly the C expression (same
// operands, same operation order); only the address arithmetic changed.
// ---------------------------------------------------------------------------

// libvorbis: static const float hsqt2 = .70710678118654752f;
// 17 sig digits round-trip to f32 bits 0x3f3504f3 (Rust's `7.0710678e-1` does too).
// The shorter `7.071068e-1` rounds to 0x3f3504f4 (1 ULP off), which is wrong.
#[allow(clippy::approx_constant)]
const HSQT2: f32 = 7.0710678e-1_f32;

fn dradf4(
    ido: usize,
    l1: usize,
    cc: &[f32],
    ch: &mut [f32],
    wa1: &[f32],
    wa2: &[f32],
    wa3: &[f32],
) {
    let t0 = l1 * ido;

    // Input quarters: C's cc[t3] / cc[t1] / cc[t4] / cc[t2] streams
    // (offsets 0, t0, 2*t0, 3*t0).
    let (c0, rest) = cc[..4 * t0].split_at(t0);
    let (c1, rest) = rest.split_at(t0);
    let (c2, c3) = rest.split_at(t0);
    let ch = &mut ch[..4 * t0];

    if ido == 1 {
        // Only the DC butterfly runs; every stream is contiguous in k, so
        // this loop vectorizes (4-way interleaved store).
        for ((((o, &a0), &a1), &a2), &a3) in ch.chunks_exact_mut(4).zip(c0).zip(c1).zip(c2).zip(c3)
        {
            let tr1 = a1 + a3;
            let tr2 = a0 + a2;
            o[0] = tr1 + tr2;
            o[3] = tr2 - tr1;
            o[1] = a0 - a2;
            o[2] = a3 - a1;
        }
        return;
    }

    if ido == 4 {
        // One twiddle pair per k: the general loop below, fully unrolled on
        // fixed-size views (DC, the i==2 twiddle step, then Nyquist).
        let (wr1, wi1) = (wa1[0], wa1[1]);
        let (wr2, wi2) = (wa2[0], wa2[1]);
        let (wr3, wi3) = (wa3[0], wa3[1]);
        for ((((o, a0), a1), a2), a3) in ch
            .chunks_exact_mut(16)
            .zip(c0.chunks_exact(4))
            .zip(c1.chunks_exact(4))
            .zip(c2.chunks_exact(4))
            .zip(c3.chunks_exact(4))
        {
            let tr1 = a1[0] + a3[0];
            let tr2 = a0[0] + a2[0];
            o[0] = tr1 + tr2;
            o[15] = tr2 - tr1;
            o[7] = a0[0] - a2[0];
            o[8] = a3[0] - a1[0];

            let cr2 = wr1 * a1[1] + wi1 * a1[2];
            let ci2 = wr1 * a1[2] - wi1 * a1[1];
            let cr3 = wr2 * a2[1] + wi2 * a2[2];
            let ci3 = wr2 * a2[2] - wi2 * a2[1];
            let cr4 = wr3 * a3[1] + wi3 * a3[2];
            let ci4 = wr3 * a3[2] - wi3 * a3[1];
            let tr1 = cr2 + cr4;
            let tr4 = cr4 - cr2;
            let ti1 = ci2 + ci4;
            let ti4 = ci2 - ci4;
            let ti2 = a0[2] + ci3;
            let ti3 = a0[2] - ci3;
            let tr2 = a0[1] + cr3;
            let tr3 = a0[1] - cr3;
            o[1] = tr1 + tr2;
            o[2] = ti1 + ti2;
            o[5] = tr3 - ti4;
            o[6] = tr4 - ti3;
            o[9] = ti4 + tr3;
            o[10] = tr4 + ti3;
            o[13] = tr2 - tr1;
            o[14] = ti1 - ti2;

            let ti1 = -HSQT2 * (a1[3] + a3[3]);
            let tr1 = HSQT2 * (a1[3] - a3[3]);
            o[3] = tr1 + a0[3];
            o[11] = a0[3] - tr1;
            o[4] = ti1 - a2[3];
            o[12] = ti1 + a2[3];
        }
        return;
    }

    // Constant ido for the sizes that occur, so the per-k slicing folds.
    match ido {
        16 => dradf4_general(16, ch, c0, c1, c2, c3, wa1, wa2, wa3),
        64 => dradf4_general(64, ch, c0, c1, c2, c3, wa1, wa2, wa3),
        256 => dradf4_general(256, ch, c0, c1, c2, c3, wa1, wa2, wa3),
        _ => dradf4_general(ido, ch, c0, c1, c2, c3, wa1, wa2, wa3),
    }
}

/// dradf4 for ido >= 2: `c0..c3` are the input quarters, `ch` the 4*t0
/// output.
#[inline(always)]
fn dradf4_general(
    ido: usize,
    ch: &mut [f32],
    c0: &[f32],
    c1: &[f32],
    c2: &[f32],
    c3: &[f32],
    wa1: &[f32],
    wa2: &[f32],
    wa3: &[f32],
) {
    // Twiddle pairs per k (C's `i` from 2 below ido in steps of 2).
    let m = (ido - 1) / 2;
    let w1 = &wa1[..2 * m];
    let w2 = &wa2[..2 * m];
    let w3 = &wa3[..2 * m];
    // Index of the imaginary half of each output pair: always 1, but opaque
    // to the optimizer. With a literal 1 the SLP vectorizer packs each
    // (re, im) pair into 2-lane shuffled code that is ~2x slower on x86_64
    // (and no faster on aarch64) than this plain scalar loop.
    let im = std::hint::black_box(1usize);

    for ((((o, a0), a1), a2), a3) in ch
        .chunks_exact_mut(4 * ido)
        .zip(c0.chunks_exact(ido))
        .zip(c1.chunks_exact(ido))
        .zip(c2.chunks_exact(ido))
        .zip(c3.chunks_exact(ido))
    {
        // DC butterfly. C: tr1=cc[t1]+cc[t2]; tr2=cc[t3]+cc[t4];
        let tr1 = a1[0] + a3[0];
        let tr2 = a0[0] + a2[0];
        o[0] = tr1 + tr2;
        o[4 * ido - 1] = tr2 - tr1;
        o[2 * ido - 1] = a0[0] - a2[0];
        o[2 * ido] = a3[0] - a1[0];

        if ido != 2 {
            // Twiddle loop. Output pairs: C's ch[t4-1..] ascending from
            // o[1], ch[t5-1..] descending to o[2*ido-2], ch[t4+t6-1..]
            // ascending from o[2*ido+1], ch[t5+t6-1..] descending to
            // o[4*ido-2].
            let (q01, q23) = o.split_at_mut(2 * ido);
            let (q0, q1) = q01.split_at_mut(ido);
            let (q2, q3) = q23.split_at_mut(ido);
            let ra = &mut q0[1..1 + 2 * m];
            let rb = &mut q1[ido - 1 - 2 * m..ido - 1];
            let rc = &mut q2[1..1 + 2 * m];
            let rd = &mut q3[ido - 1 - 2 * m..ido - 1];
            let x = [
                &a0[1..1 + 2 * m],
                &a1[1..1 + 2 * m],
                &a2[1..1 + 2 * m],
                &a3[1..1 + 2 * m],
            ];
            if ido >= 64 {
                dradf4_row(x, [w1, w2, w3], ra, rb, rc, rd);
            } else {
                dradf4_row_body(x, [w1, w2, w3], ra, rb, rc, rd, im);
            }
            if ido & 1 != 0 {
                continue;
            }
        }

        // L105: Nyquist half-sample terms (ido even, including ido==2).
        // C: ti1=-hsqt2*(cc[t1]+cc[t2]); tr1=hsqt2*(cc[t1]-cc[t2]);
        let n1 = a1[ido - 1];
        let n3 = a3[ido - 1];
        let ti1 = -HSQT2 * (n1 + n3);
        let tr1 = HSQT2 * (n1 - n3);
        o[ido - 1] = tr1 + a0[ido - 1];
        o[3 * ido - 1] = a0[ido - 1] - tr1;
        o[ido] = ti1 - a2[ido - 1];
        o[3 * ido] = ti1 + a2[ido - 1];
    }
}

/// One k's dradf4 twiddle loop, vectorized across i. It is kept out of
/// line on purpose: as function arguments the eleven streams are distinct
/// `noalias` slices, so LLVM's loop vectorizer needs no runtime overlap
/// checks (inlined, those checks send it to the scalar fallback, which is
/// the slow 2-lane SLP code on x86_64). Worth it for long rows (ido >= 64);
/// shorter rows use [`dradf4_row_body`] inline.
#[inline(never)]
fn dradf4_row(
    x: [&[f32]; 4],
    w: [&[f32]; 3],
    ra: &mut [f32],
    rb: &mut [f32],
    rc: &mut [f32],
    rd: &mut [f32],
) {
    dradf4_row_body(x, w, ra, rb, rc, rd, 1);
}

/// The twiddle loop proper. `x` are the four input quarters' (re, im)
/// pairs from i = 2 on, `w` the three twiddle tables, `ra`/`rc` the
/// ascending and `rb`/`rd` the descending output pairs; `im` is the index
/// of the imaginary half of an output pair (always 1).
#[inline(always)]
fn dradf4_row_body(
    x: [&[f32]; 4],
    w: [&[f32]; 3],
    ra: &mut [f32],
    rb: &mut [f32],
    rc: &mut [f32],
    rd: &mut [f32],
    im: usize,
) {
    let [a0, a1, a2, a3] = x;
    let [w1, w2, w3] = w;
    let it = a0
        .chunks_exact(2)
        .zip(a1.chunks_exact(2))
        .zip(a2.chunks_exact(2))
        .zip(a3.chunks_exact(2))
        .zip(w1.chunks_exact(2))
        .zip(w2.chunks_exact(2))
        .zip(w3.chunks_exact(2))
        .zip(ra.chunks_exact_mut(2))
        .zip(rb.rchunks_exact_mut(2))
        .zip(rc.chunks_exact_mut(2))
        .zip(rd.rchunks_exact_mut(2));
    for ((((((((((x0, x1), x2), x3), w1), w2), w3), oa), ob), oc), od) in it {
        let cr2 = w1[0] * x1[0] + w1[1] * x1[1];
        let ci2 = w1[0] * x1[1] - w1[1] * x1[0];
        let cr3 = w2[0] * x2[0] + w2[1] * x2[1];
        let ci3 = w2[0] * x2[1] - w2[1] * x2[0];
        let cr4 = w3[0] * x3[0] + w3[1] * x3[1];
        let ci4 = w3[0] * x3[1] - w3[1] * x3[0];

        let tr1 = cr2 + cr4;
        let tr4 = cr4 - cr2;
        let ti1 = ci2 + ci4;
        let ti4 = ci2 - ci4;
        let ti2 = x0[1] + ci3;
        let ti3 = x0[1] - ci3;
        let tr2 = x0[0] + cr3;
        let tr3 = x0[0] - cr3;

        oa[0] = tr1 + tr2;
        oa[im] = ti1 + ti2;
        ob[0] = tr3 - ti4;
        ob[im] = tr4 - ti3;
        oc[0] = ti4 + tr3;
        oc[im] = tr4 + ti3;
        od[0] = tr2 - tr1;
        od[im] = ti1 - ti2;
    }
}

// ---------------------------------------------------------------------------
// drftf1  (forward real FFT driver)
// Port of smallft.c lines 572-631.
// ---------------------------------------------------------------------------
fn drftf1(n: usize, c: &mut [f32], ch: &mut [f32], wa: &[f32], ifac: &[i32]) {
    let nf = ifac[1] as usize;
    let mut na = 1usize;
    let mut l2 = n;
    let mut iw = n;

    // Debug dump: input to drftf1 (first call only).
    if crate::debug_dump::dump_enabled() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static FIRED: AtomicBool = AtomicBool::new(false);
        if !FIRED.swap(true, Ordering::Relaxed) {
            let mut bytes = Vec::with_capacity(n * 4);
            for v in c.iter() {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            let _ = std::fs::write("/tmp/lewtoff-debug/r_drftf1_in.bin", &bytes);

            // C dumps 2n floats of `trigcache + n`; only the first n are
            // populated (we embed just those), so zero-pad to the C layout.
            let mut wabytes = Vec::with_capacity(2 * n * 4);
            for v in wa.iter().chain(std::iter::repeat(&0.0f32)).take(2 * n) {
                wabytes.extend_from_slice(&v.to_le_bytes());
            }
            let _ = std::fs::write("/tmp/lewtoff-debug/r_drftf1_wa.bin", &wabytes);
        }
    }

    for k1 in 0..nf {
        let kh = nf - k1;
        let ip = ifac[kh + 1] as usize;
        let l1 = l2 / ip;
        let ido = n / l2;
        iw -= (ip - 1) * ido;
        na = 1 - na;

        if ip == 4 {
            let ix2 = iw + ido;
            let ix3 = ix2 + ido;
            if na != 0 {
                dradf4(
                    ido,
                    l1,
                    ch,
                    c,
                    &wa[iw - 1..],
                    &wa[ix2 - 1..],
                    &wa[ix3 - 1..],
                );
            } else {
                dradf4(
                    ido,
                    l1,
                    c,
                    ch,
                    &wa[iw - 1..],
                    &wa[ix2 - 1..],
                    &wa[ix3 - 1..],
                );
            }
            if crate::debug_dump::dump_enabled() {
                use std::sync::atomic::{AtomicUsize, Ordering};
                static ITER: AtomicUsize = AtomicUsize::new(0);
                let iter = ITER.fetch_add(1, Ordering::Relaxed);
                if iter < 4 {
                    let buf = if na != 0 { &c[..] } else { &ch[..] };
                    let mut bytes = Vec::with_capacity(buf.len() * 4);
                    for v in buf {
                        bytes.extend_from_slice(&v.to_le_bytes());
                    }
                    let _ = std::fs::write(
                        format!("/tmp/lewtoff-debug/r_drftf1_iter{iter}_out.bin"),
                        &bytes,
                    );
                }
            }
        } else if ip == 2 {
            if na != 0 {
                dradf2(ido, l1, ch, c, &wa[iw - 1..]);
            } else {
                dradf2(ido, l1, c, ch, &wa[iw - 1..]);
            }
        } else {
            // Generic radix -- unreachable for n=256 or n=2048 (only radix 2,4 factors)
            panic!("drft: generic radix ip={ip} not supported for this FFT size");
        }

        l2 = l1;
    }

    if na == 1 {
        return;
    }
    c.copy_from_slice(ch);
}

// ---------------------------------------------------------------------------
// Public API: in-place real forward FFT matching libvorbis drft_forward.
// ---------------------------------------------------------------------------

/// Twiddle table + factorization for one FFT size, as built by libvorbis
/// `drft_init(n)`: `wa` is the populated part of `trigcache + n` (n floats;
/// the rest of the C trigcache is zero or `ch` scratch), `split` is
/// `splitcache`.
pub(crate) struct DrftTables {
    pub n: usize,
    wa: &'static [f32],
    split: &'static [i32; 32],
}

pub(crate) static DRFT_256: DrftTables = DrftTables {
    n: 256,
    wa: &DRFT_WA_256,
    split: &DRFT_SPLIT_256,
};
pub(crate) static DRFT_512: DrftTables = DrftTables {
    n: 512,
    wa: &DRFT_WA_512,
    split: &DRFT_SPLIT_512,
};
pub(crate) static DRFT_2048: DrftTables = DrftTables {
    n: 2048,
    wa: &DRFT_WA_2048,
    split: &DRFT_SPLIT_2048,
};
pub(crate) static DRFT_4096: DrftTables = DrftTables {
    n: 4096,
    wa: &DRFT_WA_4096,
    split: &DRFT_SPLIT_4096,
};

/// Largest supported FFT size (the q-1 long block).
pub(crate) const DRFT_MAX_N: usize = 4096;

/// Tables for FFT size `n`. Panics unless `n` is 256, 512, 2048 or 4096.
pub(crate) fn drft_tables(n: usize) -> &'static DrftTables {
    match n {
        256 => &DRFT_256,
        512 => &DRFT_512,
        2048 => &DRFT_2048,
        4096 => &DRFT_4096,
        _ => panic!("drft: unsupported FFT size {n}"),
    }
}

/// In-place real forward FFT of `data` (length `t.n`), bit-identical to
/// libvorbis `drft_forward(&fft_look[W], data)`.
pub(crate) fn drft_forward_with(t: &DrftTables, data: &mut [f32]) {
    let n = t.n;
    assert_eq!(data.len(), n, "drft: data length must equal FFT size");
    // libvorbis uses trigcache[0..n] as the `ch` scratch; every element is
    // written before it is read, so its contents never matter. Size the
    // (zeroed) stack scratch to the call so short blocks stay cheap.
    if n <= 512 {
        let mut ch = [0.0f32; 512];
        drftf1(n, data, &mut ch[..n], t.wa, t.split);
    } else if n <= 2048 {
        let mut ch = [0.0f32; 2048];
        drftf1(n, data, &mut ch[..n], t.wa, t.split);
    } else {
        let mut ch = [0.0f32; DRFT_MAX_N];
        drftf1(n, data, &mut ch[..n], t.wa, t.split);
    }
}

/// [`drft_forward_with`] for `n = data.len()` (256, 512, 2048 or 4096).
pub(crate) fn drft_forward_n(data: &mut [f32]) {
    drft_forward_with(drft_tables(data.len()), data);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::dsp_oracle::{assert_manifest, hash_f32, hash_u32, lcg_input};

    #[test]
    fn dsp_manifest_drft_tables_match_libvorbis() {
        for n in [256, 512, 2048, 4096] {
            let t = drft_tables(n);
            assert_eq!(t.n, n);
            // The C manifest hashes all 2n floats of `trigcache + n`; we
            // embed the populated first n, the remainder is zero.
            let mut wa2n = t.wa.to_vec();
            wa2n.resize(2 * n, 0.0);
            assert_manifest(&format!("table.drft_wa.{n}"), 2 * n, hash_f32(&wa2n));
            let split: Vec<u32> = t.split.iter().map(|&v| v as u32).collect();
            assert_manifest(&format!("table.drft_split.{n}"), 32, hash_u32(&split));
        }
    }

    #[test]
    fn dsp_manifest_drft_forward_matches_libvorbis() {
        for n in [256usize, 512, 2048, 4096] {
            for seed in 1u32..=3 {
                let mut d = lcg_input(n, seed * 104729 + n as u32);
                drft_forward_n(&mut d);
                assert_manifest(&format!("drft.{n}.seed{seed}"), n, hash_f32(&d));
            }
        }
    }

    #[test]
    fn drft_short_dc_only() {
        // Input: constant 1.0 → DC bin = N, all other bins = 0
        let n = 256;
        let mut data = [1.0f32; 256];
        drft_forward_n(&mut data);
        // DC bin
        let dc = data[0];
        assert!(
            (dc - n as f32).abs() < 0.1,
            "DC bin expected {}, got {}",
            n,
            dc
        );
        // All other bins should be ~0
        for i in 1..n {
            assert!(
                data[i].abs() < 0.01,
                "bin {} expected ~0, got {}",
                i,
                data[i]
            );
        }
    }

    #[test]
    fn drft_long_dc_only() {
        let n = 2048;
        let mut data = [1.0f32; 2048];
        drft_forward_n(&mut data);
        let dc = data[0];
        assert!(
            (dc - n as f32).abs() < 0.1,
            "DC bin expected {}, got {}",
            n,
            dc
        );
        for i in 1..n {
            assert!(
                data[i].abs() < 0.01,
                "bin {} expected ~0, got {}",
                i,
                data[i]
            );
        }
    }
}

#[test]
fn drft_sine_peak() {
    use std::f32::consts::PI;
    let n = 256; // 256
    let mut data = [0.0f32; 256];
    // Sine at bin 10 (freq = 10/256 cycles/sample)
    for i in 0..n {
        data[i] = (2.0 * PI * 10.0 * i as f32 / n as f32).sin();
    }
    drft_forward_n(&mut data);

    // Find peak in Fortran-packed output
    let mut max_mag = 0.0f32;
    let mut max_bin = 0usize;
    for b in 0..n / 2 {
        let mag = if b == 0 {
            data[0].abs()
        } else {
            (data[2 * b - 1].powi(2) + data[2 * b].powi(2)).sqrt()
        };
        if mag > max_mag {
            max_mag = mag;
            max_bin = b;
        }
    }
    assert_eq!(max_bin, 10, "peak should be at bin 10, got {max_bin}");
    assert!(
        (max_mag - n as f32 / 2.0).abs() < 1.0,
        "peak magnitude expected ~{}, got {max_mag}",
        n / 2
    );
}

#[test]
fn drft_logfft_sine_440() {
    use crate::mapping0::add_345;
    use crate::psy::to_db;
    use std::f32::consts::PI;

    let rate = 44100u32;
    let freq = 440.0f32;
    let n = 256; // 256
    let _n2 = n / 2;

    // Build 256 samples of sine at 440Hz (matching lewtoff test amplitude)
    // In the test: (sin(2pi*440*t) * 16384.0) as i16, normalized to f32 by /32768.0
    // Actually lewtoff normalizes: pcm[i] = sample as f32 / 32768.0
    let mut pcm = [0.0f32; 256];
    for i in 0..n {
        let t = i as f32 / rate as f32;
        let sample = (2.0 * PI * freq * t).sin() * 16384.0;
        pcm[i] = sample / 32768.0;
    }

    // Apply a basic symmetric window (approximation)
    // Actually this is before windowing. Let's skip window for now.

    drft_forward_n(&mut pcm);

    let scale = 4.0f32 / n as f32;
    let scale_db = add_345(to_db(scale));

    // Compute logfft
    let mut logfft = [0.0f32; 128]; // n2
    let lam = add_345(scale_db + to_db(pcm[0]));
    logfft[0] = lam;
    let mut local_ampmax = lam;
    let mut j = 1usize;
    while j < n - 1 {
        let temp = pcm[j] * pcm[j] + pcm[j + 1] * pcm[j + 1];
        let t = add_345(scale_db + 0.5_f32 * to_db(temp));
        logfft[(j + 1) >> 1] = t;
        if t > local_ampmax {
            local_ampmax = t;
        }
        j += 2;
    }

    // Find peak bin
    let (peak_bin, peak_val) = logfft[1..]
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .map(|(i, v)| (i + 1, *v))
        .unwrap();

    // Expected bin for 440Hz at 44100Hz, n=256
    // bin = round(440 * 256 / 44100) = round(2.55) ≈ 2 or 3
    eprintln!(
        "Peak at logfft[{}] = {:.2} dB (local_ampmax={:.2})",
        peak_bin, peak_val, local_ampmax
    );
    eprintln!("logfft[1..5] = {:?}", &logfft[1..5]);

    // Peak should be near 0 dB (we computed ~0.69 dB earlier)
    assert!(
        peak_val > -20.0,
        "peak logfft expected > -20dB, got {peak_val}"
    );
    assert!(peak_val < 5.0, "peak logfft expected < 5dB, got {peak_val}");
}
