//! Vorbis window application and overlap-add buffer.

#![allow(clippy::needless_range_loop)]
#![allow(clippy::explicit_counter_loop)]
//!
//! Port of libvorbis `lib/window.c` `_vorbis_apply_window` supporting both
//! blocksize pairs the 44.1/48 kHz templates use — 256/2048 (q0..q10) and
//! 512/4096 (q-1, `blocksize_short_44[0]`/`blocksize_long_44[0]`) — and all
//! long/short transitions.
//!
//! Window transition logic:
//!   _vorbis_apply_window(d, winno, blocksizes, lW, W, nW):
//!     - lW=(W?lW:0), nW=(W?nW:0)
//!     - windowLW = vwin[winno[lW]], windowNW = vwin[winno[nW]]
//!     - n = blocksizes[W]; ln = blocksizes[lW]; rn = blocksizes[nW]
//!     - leftbegin = n/4 - ln/4, leftend = leftbegin + ln/2
//!     - rightbegin = n/2 + n/4 - rn/4, rightend = rightbegin + rn/2
//!     - d[0..leftbegin] = 0
//!     - d[leftbegin..leftend] *= windowLW[0..ln/2]
//!     - d[rightbegin..rightend] *= windowNW[rn/2-1..0] (reversed)
//!     - d[rightend..n] = 0
//!
//! `winno[i] = ov_ilog(blocksizes[i]) - 7` (block.c:207), and `vwin[k]` is
//! the half-window for blocksize `2^(k+7)` holding `bs/2` floats: vwin256
//! (128 floats, `WIN_HALF_256`), vwin512 (`WIN_HALF_512`), vwin2048
//! (`WIN_HALF_2048`), vwin4096 (`WIN_HALF_4096`). See [`window_half`].

use crate::tables::window::{WIN_HALF_256, WIN_HALF_512, WIN_HALF_2048, WIN_HALF_4096};

/// Largest long block over all supported blocksize pairs (q-1: 4096).
pub(crate) const MAX_LONG_BLOCK: usize = 4096;
/// Largest long half-block (q-1: 2048). Also the largest psy `n`.
pub(crate) const MAX_LONG_HALF: usize = MAX_LONG_BLOCK / 2;
/// Largest short block over all supported blocksize pairs (q-1: 512).
pub(crate) const MAX_SHORT_BLOCK: usize = 512;
/// Largest short half-block (q-1: 256).
pub(crate) const MAX_SHORT_HALF: usize = MAX_SHORT_BLOCK / 2;

/// A `ci->blocksizes[0..2]` pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BlockSizes {
    /// `blocksizes[0]`
    pub short: usize,
    /// `blocksizes[1]`
    pub long: usize,
}

impl BlockSizes {
    /// 44.1/48 kHz templates at q0..q10 (`blocksize_*_44[1..]`).
    #[cfg(test)]
    pub(crate) const BS_256_2048: BlockSizes = BlockSizes {
        short: 256,
        long: 2048,
    };
    /// 44.1/48 kHz templates at q-1 (template index 0, `base_quality < 0`).
    /// (The encoder takes its pair from `QualityConfig::blocksizes`; these
    /// named constants are for tests.)
    #[cfg(test)]
    pub(crate) const BS_512_4096: BlockSizes = BlockSizes {
        short: 512,
        long: 4096,
    };

    /// `blocksizes[W]` for `W = is_long`.
    #[inline]
    pub(crate) const fn get(self, is_long: bool) -> usize {
        if is_long { self.long } else { self.short }
    }

    #[inline]
    pub(crate) const fn short_half(self) -> usize {
        self.short / 2
    }

    #[inline]
    pub(crate) const fn long_half(self) -> usize {
        self.long / 2
    }

    /// Samples between the start of a long block's right half (`centerW`)
    /// and the short-overlap region that hands off to a following short
    /// block; equivalently the length of the un-windowed middle section
    /// `[leftend..long/2)` of a long block entered from a short one
    /// (`long/4 - short/4`: 448 for 256/2048, 896 for 512/4096).
    #[inline]
    pub(crate) const fn long_short_gap(self) -> usize {
        self.long / 4 - self.short / 4
    }
}

/// libvorbis `vwin[ov_ilog(bs) - 7]`: the rising half-window (`bs/2` floats)
/// for blocksize `bs`. Panics unless `bs` is 256, 512, 2048 or 4096.
pub(crate) fn window_half(bs: usize) -> &'static [f32] {
    match bs {
        256 => &WIN_HALF_256,
        512 => &WIN_HALF_512,
        2048 => &WIN_HALF_2048,
        4096 => &WIN_HALF_4096,
        _ => panic!("window: unsupported blocksize {bs}"),
    }
}

/// `_vorbis_apply_window(d, winno, blocksizes, lW, W, nW)` for `d` of
/// length `bs.get(w)`.
///
/// `lw`/`w`/`nw`: is the previous/current/next block long? For short blocks
/// (`w == false`) `lw` and `nw` are forced to short, as in libvorbis.
pub(crate) fn apply_window_bs(d: &mut [f32], bs: BlockSizes, lw: bool, w: bool, nw: bool) {
    let lw = if w { lw } else { false };
    let nw = if w { nw } else { false };

    let n = bs.get(w);
    let ln = bs.get(lw);
    let rn = bs.get(nw);
    debug_assert_eq!(d.len(), n);

    let window_lw = window_half(ln);
    let window_nw = window_half(rn);

    let leftbegin = n / 4 - ln / 4;
    let leftend = leftbegin + ln / 2;
    let rightbegin = n / 2 + n / 4 - rn / 4;
    let rightend = rightbegin + rn / 2;

    for i in 0..leftbegin {
        d[i] = 0.0;
    }

    let mut p: usize = 0;
    for i in leftbegin..leftend {
        d[i] *= window_lw[p];
        p += 1;
    }

    let mut p: usize = rn / 2 - 1;
    for i in rightbegin..rightend {
        d[i] *= window_nw[p];
        p = p.wrapping_sub(1);
    }

    for i in rightend..n {
        d[i] = 0.0;
    }
}

/// Holds state for windowing between blocks.
pub(crate) struct WindowingBuffer {
    bs: BlockSizes,
    /// Previous `bs.long/2` samples (long block overlap buffer); only the
    /// first `bs.long_half()` entries are used.
    prev_long: [f32; MAX_LONG_HALF],
    /// Previous `bs.short/2` samples (short block overlap buffer); only the
    /// first `bs.short_half()` entries are used.
    prev_short: [f32; MAX_SHORT_HALF],
    /// Was the previous block a long block?
    prev_is_long: bool,
}

impl WindowingBuffer {
    /// Buffer for an arbitrary supported blocksize pair.
    pub(crate) fn with_blocksizes(bs: BlockSizes) -> Self {
        assert!(bs.long <= MAX_LONG_BLOCK && bs.short <= MAX_SHORT_BLOCK);
        Self {
            bs,
            prev_long: [0.0f32; MAX_LONG_HALF],
            prev_short: [0.0f32; MAX_SHORT_HALF],
            prev_is_long: false,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn blocksizes(&self) -> BlockSizes {
        self.bs
    }

    /// Set the pre-stream extrapolated samples for the short block's left half.
    /// `prestream[i]` = virtual sample at -(i+1) (index 0 = sample just before stream start).
    /// These are placed in `prev_short` in reversed order:
    /// `prev_short[short_half - 1 - i] = prestream[i]`
    /// so that `prev_short[short_half-1]` = sample at -1 (closest to stream start).
    /// At most `short_half` (128 or 256) samples are used.
    pub(crate) fn set_prestream(&mut self, prestream: &[f32]) {
        let sh = self.bs.short_half();
        let len = prestream.len().min(sh);
        for i in 0..len {
            self.prev_short[sh - 1 - i] = prestream[i];
        }
    }

    /// Produce a windowed long block into `out` (length `bs.long`).
    ///
    /// `current` is the next `bs.long/2` PCM samples.
    /// `pre_current`: for the short→long transition (lw=false), the
    ///   `bs.long_short_gap()` PCM samples immediately preceding `current` in
    ///   the stream (448 for 256/2048, 896 for 512/4096). These fill the
    ///   un-windowed middle section `[leftend..long/2]` of the analysis
    ///   frame, matching libvorbis's continuous PCM buffer. Ignored (pass
    ///   `None`) for long→long transitions.
    /// `nw_is_long`: is the next block long?
    pub(crate) fn push_long_block_into(
        &mut self,
        current: &[f32],
        pre_current: Option<&[f32]>,
        nw_is_long: bool,
        out: &mut [f32],
    ) {
        let bs = self.bs;
        let lh = bs.long_half();
        let sh = bs.short_half();
        assert_eq!(current.len(), lh, "push_long_block: current must be long/2");
        assert_eq!(out.len(), bs.long, "push_long_block: out must be long");
        let lw = self.prev_is_long;
        out.fill(0.0);

        if lw {
            // prev was long: copy prev_long into left half
            out[..lh].copy_from_slice(&self.prev_long[..lh]);
        } else {
            // prev was short: center the short overlap in the left half.
            // leftbegin = long/4 - short/4 (448 / 896)
            // leftend   = leftbegin + short/2 (576 / 1152)
            let leftbegin = bs.long_short_gap();
            let leftend = leftbegin + sh;
            out[leftbegin..leftend].copy_from_slice(&self.prev_short[..sh]);
            // The region [leftend..long/2] is NOT zeroed by the Vorbis window
            // function; libvorbis's continuous PCM buffer has real samples
            // there. Fill it from `pre_current`.
            if let Some(pre) = pre_current {
                let mid_len = lh - leftend;
                let fill_len = pre.len().min(mid_len);
                out[leftend..leftend + fill_len].copy_from_slice(&pre[..fill_len]);
            }
            // Samples [0..leftbegin] remain 0 (zeroed by window anyway).
        }

        // Right half: current samples
        out[lh..].copy_from_slice(current);

        // Apply the transition window
        apply_window_bs(out, bs, lw, true, nw_is_long);

        // Update state
        self.prev_long[..lh].copy_from_slice(current);
        self.prev_is_long = true;

        // For a long-to-short transition (nw_is_long=false), the next short
        // block needs prev_short populated from the samples straddling the
        // next centerW. centerW advances by long/4 + short/4, so the next
        // short's left half (short/2 samples) is
        // current[long/4 - short/4 ..][..short/2].
        if !nw_is_long {
            let leftbegin = bs.long_short_gap();
            self.prev_short[..sh].copy_from_slice(&current[leftbegin..leftbegin + sh]);
        }
    }

    /// Produce a windowed short block into `out` (length `bs.short`).
    ///
    /// `current` is the next `bs.short/2` PCM samples.
    pub(crate) fn push_short_block_into(&mut self, current: &[f32], out: &mut [f32]) {
        let bs = self.bs;
        let sh = bs.short_half();
        assert_eq!(
            current.len(),
            sh,
            "push_short_block: current must be short/2"
        );
        assert_eq!(out.len(), bs.short, "push_short_block: out must be short");

        // Build un-windowed short block: [prev_short | current]
        out[..sh].copy_from_slice(&self.prev_short[..sh]);
        out[sh..].copy_from_slice(current);

        // Apply window: W=0 → lW=0, nW=0 (forced by _vorbis_apply_window)
        apply_window_bs(out, bs, false, false, false);

        // Update state
        self.prev_short[..sh].copy_from_slice(current);
        self.prev_is_long = false;
    }
}

/// Test helpers shared by the DSP modules: deterministic input and the
/// libvorbis ground-truth manifest produced by
/// `tools/oracle-encoder/dsp_oracle manifest` (see that file for details).
#[cfg(test)]
pub(crate) mod dsp_oracle {
    const MANIFEST: &str = include_str!("../tools/oracle-encoder/dsp_manifest.txt");

    /// Same LCG + float mapping as `lcg_fill` in dsp_oracle.c (exact in f32).
    pub(crate) fn lcg_input(n: usize, seed: u32) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                ((s >> 8) as f32 / 8388608.0 - 1.0) * 0.5
            })
            .collect()
    }

    fn fnv(bytes: impl Iterator<Item = u8>) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    pub(crate) fn hash_f32(v: &[f32]) -> u64 {
        fnv(v.iter().flat_map(|x| x.to_bits().to_le_bytes()))
    }

    pub(crate) fn hash_u32(v: &[u32]) -> u64 {
        fnv(v.iter().flat_map(|x| x.to_le_bytes()))
    }

    /// Assert that manifest entry `name` has `count` elements hashing to `hash`.
    pub(crate) fn assert_manifest(name: &str, count: usize, hash: u64) {
        let line = MANIFEST
            .lines()
            .find(|l| l.split_whitespace().next() == Some(name))
            .unwrap_or_else(|| panic!("{name} missing from dsp_manifest.txt"));
        let mut it = line.split_whitespace().skip(1);
        let want_count: usize = it.next().unwrap().parse().unwrap();
        let want_hash = u64::from_str_radix(it.next().unwrap(), 16).unwrap();
        assert_eq!(count, want_count, "{name}: element count");
        assert_eq!(
            hash, want_hash,
            "{name}: Rust output differs from libvorbis (hash {hash:016x} vs {want_hash:016x})"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::dsp_oracle::{assert_manifest, hash_f32, lcg_input};
    use super::*;
    use crate::tables::window::SIN_WINDOW_2048;

    #[test]
    fn dsp_manifest_window_tables_match_libvorbis() {
        for bs in [256, 512, 2048, 4096] {
            let w = window_half(bs);
            assert_eq!(w.len(), bs / 2);
            assert_manifest(&format!("table.vwin.{bs}"), bs / 2, hash_f32(w));
        }
    }

    #[test]
    fn dsp_manifest_apply_window_matches_libvorbis() {
        for bs in [BlockSizes::BS_256_2048, BlockSizes::BS_512_4096] {
            for w in [false, true] {
                for lw in [false, true] {
                    for nw in [false, true] {
                        let n = bs.get(w);
                        let seed = 31337 + (w as u32) * 4 + (lw as u32) * 2 + nw as u32;
                        let mut d = lcg_input(n, seed);
                        apply_window_bs(&mut d, bs, lw, w, nw);
                        let name = format!(
                            "window.{}_{}.lW{}_W{}_nW{}",
                            bs.short, bs.long, lw as u8, w as u8, nw as u8
                        );
                        assert_manifest(&name, n, hash_f32(&d));
                    }
                }
            }
        }
    }

    /// Model of libvorbis's continuous PCM buffer: block `i` (size
    /// `n_i = blocksizes[W_i]`, centre `c_i`) is `x[c_i - n_i/2 .. c_i +
    /// n_i/2]` windowed with `(W_{i-1}, W_i, W_{i+1})`, and
    /// `c_{i+1} = c_i + n_i/4 + n_{i+1}/4`. `WindowingBuffer`, fed the way
    /// encode.rs feeds it, must reproduce that bit-for-bit for both pairs.
    fn check_stream_model(bs: BlockSizes) {
        let pattern = [0, 0, 1, 1, 0, 1, 0, 0, 1, 1, 1, 0, 0, 1, 0];
        let origin = bs.long; // room before c_0 for the first block's left half
        let x = lcg_input(origin + bs.long * pattern.len(), 4242 + bs.long as u32);
        let mut buf = WindowingBuffer::with_blocksizes(bs);
        let mut c = origin;
        let prestream: Vec<f32> = (0..bs.short_half()).map(|k| x[c - 1 - k]).collect();
        buf.set_prestream(&prestream);

        for i in 0..pattern.len() - 1 {
            let w = pattern[i] == 1;
            let lw = i > 0 && pattern[i - 1] == 1;
            let nw = pattern[i + 1] == 1;
            let n = bs.get(w);

            let mut want = x[c - n / 2..c + n / 2].to_vec();
            apply_window_bs(&mut want, bs, lw, w, nw);

            let mut got = vec![f32::NAN; n];
            if w {
                let gap = bs.long_short_gap();
                let pre = if lw { None } else { Some(&x[c - gap..c]) };
                buf.push_long_block_into(&x[c..c + n / 2], pre, nw, &mut got);
            } else {
                buf.push_short_block_into(&x[c..c + n / 2], &mut got);
            }
            for k in 0..n {
                assert_eq!(
                    got[k].to_bits(),
                    want[k].to_bits(),
                    "{bs:?} block {i} (lW={lw} W={w} nW={nw}) sample {k}"
                );
            }
            c += n / 4 + bs.get(nw) / 4;
        }
    }

    #[test]
    fn windowing_buffer_matches_stream_model_256_2048() {
        check_stream_model(BlockSizes::BS_256_2048);
    }

    #[test]
    fn windowing_buffer_matches_stream_model_512_4096() {
        check_stream_model(BlockSizes::BS_512_4096);
    }

    const HALF_BLOCK: usize = 1024;
    const SHORT_HALF: usize = 128;

    fn push_long(buf: &mut WindowingBuffer, current: &[f32]) -> Vec<f32> {
        let mut out = vec![0.0f32; 2 * HALF_BLOCK];
        buf.push_long_block_into(current, None, true, &mut out);
        out
    }

    #[test]
    fn windowing_all_ones_gives_window_values() {
        let mut buf = WindowingBuffer::with_blocksizes(BlockSizes::BS_256_2048);
        let block = [1.0f32; HALF_BLOCK];

        // First block: prev is zeros, current is ones
        let out = push_long(&mut buf, &block);

        // First half (prev = zeros) should be zero after windowing
        for v in &out[..HALF_BLOCK] {
            assert_eq!(*v, 0.0, "first half should be zero (prev=0 * window)");
        }
        // Second half (current = ones) should equal the window values
        for (idx, (got, exp)) in out[HALF_BLOCK..]
            .iter()
            .zip(&SIN_WINDOW_2048[HALF_BLOCK..])
            .enumerate()
        {
            assert!(
                (got - exp).abs() < 1e-6,
                "out[{}] = {} expected {}",
                HALF_BLOCK + idx,
                got,
                exp
            );
        }
    }

    #[test]
    fn windowing_second_block_all_ones() {
        let mut buf = WindowingBuffer::with_blocksizes(BlockSizes::BS_256_2048);
        let block = [1.0f32; HALF_BLOCK];

        // After first block, prev = [1..1]
        let _first = push_long(&mut buf, &block);

        // Second block: both prev and current are ones → output = window
        let out = push_long(&mut buf, &block);
        for (i, (got, exp)) in out.iter().zip(SIN_WINDOW_2048.iter()).enumerate() {
            assert!(
                (got - exp).abs() < 1e-6,
                "out[{i}] = {} expected {}",
                got,
                exp
            );
        }
    }

    #[test]
    fn short_block_window_is_symmetric() {
        let mut buf = WindowingBuffer::with_blocksizes(BlockSizes::BS_256_2048);
        let block = [1.0f32; SHORT_HALF];

        let mut out = [0.0f32; 2 * SHORT_HALF];
        buf.push_short_block_into(&block, &mut out);

        // First half is prev=zeros, second half is ones
        // After windowing:
        //   leftbegin=0, leftend=128, rightbegin=128, rightend=256
        //   d[0..128] *= WIN_HALF_256[0..128]  (but d[0..128] = 0, so stays 0)
        //   d[128..256] *= WIN_HALF_256[127..0] reversed
        for v in &out[..SHORT_HALF] {
            assert_eq!(*v, 0.0, "short block first half should be zero (prev=0)");
        }
        // Second half should equal WIN_HALF_256 reversed
        for (i, &v) in out[SHORT_HALF..].iter().enumerate() {
            let exp = WIN_HALF_256[SHORT_HALF - 1 - i];
            assert!(
                (v - exp).abs() < 1e-6,
                "short block out[{}] = {} expected {}",
                SHORT_HALF + i,
                v,
                exp
            );
        }
    }
}
