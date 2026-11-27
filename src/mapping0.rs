//! mapping0_forward: literal port of libvorbis 1.3.7 `lib/mapping0.c`
//! `mapping0_forward` function (lines 230-697).
//!
//! Drives the full encoding pipeline for one audio block:
//!   PCM windowed → MDCT → psy → floor1 → quantize/couple → residue → bits

#![allow(clippy::needless_range_loop)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::assign_op_pattern)]
#![allow(non_snake_case)]
#![allow(unused_variables)]
#![allow(unused_mut)]

use crate::bitpack::BitWriter;
use crate::debug_dump as dd;
use crate::drft::drft_forward_n;
use crate::floor1::{FLOOR1_MAX_POSTS, Floor1State, floor1_encode, floor1_fit};
use crate::mdct::mdct_forward_n;
use crate::psy::{
    VorbisInfoPsyGlobal, VorbisLookPsy, to_db, vp_ampmax_decay, vp_noisemask, vp_offset_and_mix,
    vp_tonemask,
};
use crate::residue::{
    ResidueLook, ResidueSetup, res1_class, res1_forward, res2_class, res2_forward,
};
use crate::setup::Mapping;
use crate::window::{MAX_LONG_BLOCK, MAX_LONG_HALF};

// ---------------------------------------------------------------------------
// mapping0_forward
//
// Arguments:
//   pcm_blocks   - per-channel windowed PCM blocks (blocksizes[W] samples)
//   psy_look     - psy analysis state (single shared look used for all channels)
//   gi           - global psy info (for ampmax decay, coupling)
//   ampmax       - mutable current amplitude max (updated in place)
//   floor_states - per-channel mutable floor1 states (already mutable borrow)
//   residue_types - per-residue type
//   residue_setups - per-residue setup
//   residue_looks  - per-residue runtime look
//   mapping      - the mapping config for this mode
//   mode_number  - mode index to encode in the packet header
//   modebits     - number of bits for mode number
//   books        - full codebook slice
//   opb          - output bit writer
// ---------------------------------------------------------------------------

/// libvorbis's `x + .345` dB fudge in `mapping0_forward`. The literal is a
/// double, so C forms the sum in double and rounds it to float once on
/// assignment: `(float)((double)x + .345)`. Adding `0.345f32` instead can
/// differ by an ulp.
#[inline(always)]
pub(crate) fn add_345(x: f32) -> f32 {
    (x as f64 + 0.345) as f32
}

/// Parameters for mode/window flags in the audio packet header.
pub(crate) struct BlockMode {
    pub mode_number: usize,
    pub modebits: u32,
    /// Is this a long block? (false = short block, n = blocksizes[0])
    pub is_long: bool,
    /// Was the previous block long? (only written for long blocks)
    pub prev_window: bool,
    /// Is the next block long? (only written for long blocks)
    pub next_window: bool,
}

/// Reusable per-block scratch owned by the encoder loop. Each `Vec` is
/// preallocated to `MAX_LONG_HALF` (the max half-block size) and re-sliced to
/// the current block's `n2` per call. Avoids ~10 heap allocs per block in
/// `mapping0_forward`.
pub(crate) struct MappingScratch {
    pub gmdct: Vec<Vec<f32>>,
    pub logfft: Vec<Vec<f32>>,
    pub local_ampmax: Vec<f32>,
    pub logmdct: Vec<f32>,
    pub logmask: Vec<f32>,
    pub noise_bufs: Vec<Vec<f32>>,
    pub tone_bufs: Vec<Vec<f32>>,
    pub iwork: Vec<Vec<i32>>,
    pub nonzero: Vec<i32>,
    /// Per channel: the fitted floor posts, valid when `floor_used`.
    pub floor_posts: Vec<[i32; FLOOR1_MAX_POSTS]>,
    pub floor_used: Vec<bool>,
    /// res2's channel-interleaved copy of the residue (`channels * n2`).
    pub res_work: Vec<i32>,
    /// In-place FFT buffer (`MAX_LONG_BLOCK`; sliced to the block size).
    pub fft_buf: Vec<f32>,
    pub psy: crate::psy::PsyScratch,
}

impl MappingScratch {
    pub fn new(channels: usize) -> Self {
        let make_vv_f32 = || (0..channels).map(|_| vec![0.0f32; MAX_LONG_HALF]).collect();
        let make_vv_i32 = || (0..channels).map(|_| vec![0i32; MAX_LONG_HALF]).collect();
        Self {
            gmdct: make_vv_f32(),
            logfft: make_vv_f32(),
            local_ampmax: vec![0.0f32; channels],
            logmdct: vec![0.0f32; MAX_LONG_HALF],
            logmask: vec![0.0f32; MAX_LONG_HALF],
            noise_bufs: make_vv_f32(),
            tone_bufs: make_vv_f32(),
            iwork: make_vv_i32(),
            nonzero: vec![0i32; channels],
            floor_posts: vec![[0i32; FLOOR1_MAX_POSTS]; channels],
            floor_used: vec![false; channels],
            res_work: vec![0i32; channels * MAX_LONG_HALF],
            fft_buf: vec![0.0f32; MAX_LONG_BLOCK],
            psy: crate::psy::PsyScratch::new(),
        }
    }
}

/// MDCT and log-magnitude FFT of one channel's windowed PCM (port of
/// mapping0.c lines 254-337): `n/2` lines each into `gmdct` and `logfft`
/// (`n` = `pcm.len()`; `fft_buf` holds at least `n`). Returns the
/// channel's local ampmax, clamped to at most 0 as libvorbis does.
pub(crate) fn channel_spectra(
    pcm: &[f32],
    gmdct: &mut [f32],
    logfft: &mut [f32],
    fft_buf: &mut [f32],
    dump: bool,
) -> f32 {
    let n = pcm.len();
    let n2 = n / 2;
    // Scale factor for dB computation (port of `scale = 4.f/n; scale_dB = todB(&scale) + .345`)
    let scale = 4.0f32 / n as f32;
    let scale_dB = add_345(to_db(scale));

    // MDCT (mdct_forward(&b->transform[vb->W][0], pcm, gmdct[i]))
    mdct_forward_n(pcm, &mut gmdct[..n2]);

    // Compute log-magnitude spectrum via real FFT on the windowed PCM.
    // Port of mapping0.c lines 308-337:
    //   drft_forward(&b->fft_look[vb->W], pcm);
    //   logfft[0] = scale_dB + todB(pcm) + .345;
    //   for j=1; j<n-1; j+=2:
    //     temp = pcm[j]^2 + pcm[j+1]^2
    //     logfft[(j+1)>>1] = scale_dB + 0.5*todB(&temp) + .345
    //     local_ampmax[i] = max(...)
    let fft_buf = &mut fft_buf[..n];
    fft_buf.copy_from_slice(pcm);
    drft_forward_n(fft_buf);
    if dump {
        std::fs::create_dir_all("/tmp/lewtoff-debug").ok();
        dd::write_f32_bin("/tmp/lewtoff-debug/r_drft.bin", fft_buf);
    }
    let lf = &mut logfft[..n2];
    // DC bin
    let mut local = add_345(scale_dB + to_db(fft_buf[0]));
    lf[0] = local;
    // Bins 1..n/2: element-wise (vectorizes), then the running max in
    // libvorbis's order.
    for (o, p) in lf[1..].iter_mut().zip(fft_buf[1..n - 1].chunks_exact(2)) {
        let temp = p[0] * p[0] + p[1] * p[1];
        *o = add_345(scale_dB + 0.5_f32 * to_db(temp));
    }
    for &t in &lf[1..] {
        if t > local {
            local = t;
        }
    }
    if dump {
        dd::write_f32_bin("/tmp/lewtoff-debug/r_logfft.bin", lf);
    }
    if local > 0.0 { 0.0 } else { local }
}

/// `mapping0_forward` for one block of windowed PCM (`blocksizes[W]`
/// samples per channel): the per-channel transforms
/// ([`channel_spectra`]), then [`mapping0_forward_spectra`].
#[cfg_attr(feature = "parallel", allow(dead_code))]
pub(crate) fn mapping0_forward(
    pcm_blocks: &[Vec<f32>], // per-channel windowed PCM, blocksizes[W] samples
    psy_look: &VorbisLookPsy,
    gi: &VorbisInfoPsyGlobal,
    ampmax: &mut f32,
    floor_states: &mut [Floor1State],
    residue_types: &[u16],
    residue_setups: &[ResidueSetup],
    residue_looks: &[ResidueLook],
    mapping: &Mapping,
    block_mode: &BlockMode,
    books: &[crate::codebook::Codebook],
    scratch: &mut MappingScratch,
    opb: &mut BitWriter,
) {
    let n = pcm_blocks[0].len();
    let n2 = n / 2;
    let channels = pcm_blocks.len();
    let do_dump = !block_mode.is_long && dd::dump_enabled() && dd::try_claim_mapping0_dump();

    if crate::debug_flag!("LW_DEBUG_PCM") {
        for i in 0..channels {
            let vals: Vec<String> = pcm_blocks[i].iter().map(|v| format!("{:.8}", v)).collect();
            eprintln!("LW_WINDOWED_PCM ch={} n={}: [{}]", i, n, vals.join(","));
        }
    }
    if crate::debug_flag!("LW_DEBUG_PCM_LAST") && block_mode.is_long {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let idx = N.fetch_add(1, Ordering::Relaxed);
        if idx == 30 {
            // Detailed dump for packet 35 / block 32 (long transition before
            // mid-stream short cluster).
            for j in 0..n {
                eprintln!(
                    "LW_WINDOWED_DETAIL[{}]=0x{:08x}",
                    j,
                    pcm_blocks[0][j].to_bits()
                );
            }
        }
        let mut s = format!("LW_WINDOWED_LAST idx={} n={}:", idx, n);
        for j in (0..n).step_by(64) {
            s.push_str(&format!(
                " [{}]={:.6e}(0x{:08x})",
                j,
                pcm_blocks[0][j],
                pcm_blocks[0][j].to_bits()
            ));
        }
        eprintln!("{}", s);
    }
    for i in 0..channels {
        scratch.local_ampmax[i] = channel_spectra(
            &pcm_blocks[i],
            &mut scratch.gmdct[i],
            &mut scratch.logfft[i],
            &mut scratch.fft_buf,
            do_dump && i == 0,
        );
        if crate::debug_flag!("LW_DEBUG_FFT") {
            eprintln!(
                "ch={} n={} local_ampmax={:.2} logfft[0..5]={:?}",
                i,
                n,
                scratch.local_ampmax[i],
                &scratch.logfft[i][..5.min(n2)]
            );
        }
    }
    mapping0_forward_spectra(
        n,
        channels,
        psy_look,
        gi,
        ampmax,
        floor_states,
        residue_types,
        residue_setups,
        residue_looks,
        mapping,
        block_mode,
        books,
        scratch,
        opb,
        do_dump,
    );
}

/// The rest of `mapping0_forward` once `scratch` holds each channel's
/// [`channel_spectra`] output (`gmdct`, `logfft`, `local_ampmax`) for a
/// block of `n` samples: ampmax update, masking, floor fit, coupling,
/// quantization and the packet's bits.
pub(crate) fn mapping0_forward_spectra(
    n: usize,
    channels: usize,
    psy_look: &VorbisLookPsy,
    gi: &VorbisInfoPsyGlobal,
    ampmax: &mut f32,
    floor_states: &mut [Floor1State],
    residue_types: &[u16],
    residue_setups: &[ResidueSetup],
    residue_looks: &[ResidueLook],
    mapping: &Mapping,
    block_mode: &BlockMode,
    books: &[crate::codebook::Codebook],
    scratch: &mut MappingScratch,
    opb: &mut BitWriter,
    do_dump: bool,
) {
    let n2 = n / 2;
    // Disjoint &mut borrows of scratch fields. Rust 2024's improved field
    // borrow analysis lets these be live simultaneously. Every buffer is
    // fully written (over its first n2 lines) before it is read.
    let gmdct: &mut Vec<Vec<f32>> = &mut scratch.gmdct;
    let logfft: &mut Vec<Vec<f32>> = &mut scratch.logfft;
    let local_ampmax: &mut Vec<f32> = &mut scratch.local_ampmax;
    let scratch_logmdct: &mut Vec<f32> = &mut scratch.logmdct;
    let scratch_logmask: &mut Vec<f32> = &mut scratch.logmask;
    let noise_bufs: &mut Vec<Vec<f32>> = &mut scratch.noise_bufs;
    let tone_bufs: &mut Vec<Vec<f32>> = &mut scratch.tone_bufs;
    let iwork: &mut Vec<Vec<i32>> = &mut scratch.iwork;
    let nonzero: &mut Vec<i32> = &mut scratch.nonzero;
    let floor_posts = &mut scratch.floor_posts;
    let floor_used = &mut scratch.floor_used;
    let res_work = &mut scratch.res_work;
    let psy_scratch = &mut scratch.psy;

    // Apply ampmax decay at the START of each block (port of block.c _vp_ampmax_decay call).
    // libvorbis decays g->ampmax before calling mapping0_forward, using the CURRENT block's n2.
    // This ensures that a long block (n2=1024) decays more than a short block (n2=128).
    *ampmax = vp_ampmax_decay(*ampmax, gi, n2, psy_look.rate);
    for i in 0..channels {
        if local_ampmax[i] > *ampmax {
            *ampmax = local_ampmax[i];
        }
    }

    // Noise + tone masking per channel, then floor1 fit
    // Port of: lines 362-576 in mapping0.c (minus bitrate management, we use offset_select=1 only)
    // We mutate gmdct in place (vp_offset_and_mix + vp_couple_quantize_normalize);
    // logmdct is recomputed each channel from the gmdct read BEFORE mutation,
    // so we don't need a separate gmdct_work copy.

    for i in 0..channels {
        let submap = mapping.chmuxlist[i];

        // Compute logmdct in-place into scratch (per-channel; reused across channels).
        let logmdct = &mut scratch_logmdct[..n2];
        for j in 0..n2 {
            logmdct[j] = add_345(to_db(gmdct[i][j].abs()));
        }
        let logmdct = &*logmdct; // immutable view downstream
        if crate::debug_flag!("LW_DEBUG_MDCT_RAW") {
            for &b in &[14usize, 186, 220, 260, 312, 372, 490] {
                if b < n2 {
                    eprintln!(
                        "LW_MDCT_RAW ch={} n={} bin={}: gmdct={:.10e} logmdct={:.4}",
                        i, n, b, gmdct[i][b], logmdct[b]
                    );
                }
            }
        }

        let logmask = &mut scratch_logmask[..n2];
        logmask.fill(0.0);

        // Noise mask
        vp_noisemask(psy_look, logmdct, &mut noise_bufs[i], psy_scratch);

        if crate::debug_flag!("LW_DEBUG_NOISE220") && block_mode.is_long && i == 0 {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static N: AtomicUsize = AtomicUsize::new(0);
            let idx = N.fetch_add(1, Ordering::Relaxed);
            if idx == 4 {
                let mut bytes = Vec::with_capacity(n2 * 4);
                for v in logmdct.iter() {
                    bytes.extend_from_slice(&v.to_le_bytes());
                }
                std::fs::write("/tmp/r_logmdct_blk11.bin", &bytes).ok();
                let mut bytes = Vec::with_capacity(n2 * 4);
                for v in &noise_bufs[i] {
                    bytes.extend_from_slice(&v.to_le_bytes());
                }
                std::fs::write("/tmp/r_noise_blk11.bin", &bytes).ok();
                eprintln!(
                    "R_NOISE220 idx={}: bin220={:.10} bin219={:.10} bin221={:.10}",
                    idx, noise_bufs[i][220], noise_bufs[i][219], noise_bufs[i][221],
                );
            }
        }

        // Tone mask
        vp_tonemask(
            psy_look,
            &logfft[i],
            &mut tone_bufs[i],
            *ampmax,
            local_ampmax[i],
            psy_scratch,
        );

        if crate::debug_flag!("LW_DEBUG_TONE") {
            let vals: Vec<String> = tone_bufs[i].iter().map(|v| format!("{:.6}", v)).collect();
            eprintln!("LW_TONE: [{}]", vals.join(","));
            let nvals: Vec<String> = noise_bufs[i].iter().map(|v| format!("{:.6}", v)).collect();
            eprintln!("LW_NOISE: [{}]", nvals.join(","));
        }
        if crate::debug_flag!("LW_DEBUG_FULLNOISE") {
            let nvals: Vec<String> = noise_bufs[i].iter().map(|v| format!("{:.6}", v)).collect();
            eprintln!("LW_BLOCK0_NOISE: [{}]", nvals.join(","));
        }
        if crate::debug_dump::dump_enabled() && i == 0 {
            use std::sync::atomic::{AtomicBool, Ordering};
            static FIRED: AtomicBool = AtomicBool::new(false);
            if !FIRED.swap(true, Ordering::Relaxed) {
                let mut nbytes = Vec::new();
                for v in noise_bufs[i].iter() {
                    nbytes.extend_from_slice(&v.to_le_bytes());
                }
                let _ = std::fs::write("/tmp/lewtoff-debug/r_noise.bin", &nbytes);
                let mut tbytes = Vec::new();
                for v in tone_bufs[i].iter() {
                    tbytes.extend_from_slice(&v.to_le_bytes());
                }
                let _ = std::fs::write("/tmp/lewtoff-debug/r_tone.bin", &tbytes);
                let mut obytes = Vec::new();
                for v in psy_look.noiseoffset[1].iter() {
                    obytes.extend_from_slice(&v.to_le_bytes());
                }
                let _ = std::fs::write("/tmp/lewtoff-debug/r_noiseoffset_1.bin", &obytes);
            }
        }
        // Offset + mix (offset_select=1 = nominal, not bitrate managed).
        // gmdct[i] is mutated in place; later read by vp_couple_quantize_normalize.
        vp_offset_and_mix(
            psy_look,
            &noise_bufs[i],
            &tone_bufs[i],
            1,
            logmask,
            gmdct[i].as_mut_slice(),
            logmdct,
        );

        // floor1_fit
        let floor_state = &floor_states[mapping.floorsubmap[submap]];
        if crate::debug_flag!("LW_DEBUG_LOGMDCT") {
            let vals: Vec<String> = logmdct
                .iter()
                .map(|v| format!("0x{:08x}", v.to_bits()))
                .collect();
            eprintln!("LW_BLOCK0_LOGMDCT_BITS: [{}]", vals.join(","));
            let mvals: Vec<String> = logmask
                .iter()
                .map(|v| format!("0x{:08x}", v.to_bits()))
                .collect();
            eprintln!("LW_BLOCK0_LOGMASK_BITS: [{}]", mvals.join(","));
        }
        if crate::debug_flag!("LW_DEBUG_PSY") {
            eprintln!(
                "LW_LOGMASK ch={} n={} ampmax={:.2} [0..5]: {:.2} {:.2} {:.2} {:.2} {:.2}",
                i, n, *ampmax, logmask[0], logmask[1], logmask[2], logmask[3], logmask[4]
            );
            if block_mode.is_long {
                eprintln!(
                    "LW_LOGMASK_BINS n={}: bin186={:.6} bin220={:.6} bin260={:.6} bin312={:.6} bin372={:.6}",
                    n, logmask[186], logmask[220], logmask[260], logmask[312], logmask[372]
                );
                eprintln!(
                    "LW_LOGMDCT_BINS n={}: bin186={:.6} bin220={:.6} bin260={:.6} bin312={:.6} bin372={:.6}",
                    n, logmdct[186], logmdct[220], logmdct[260], logmdct[312], logmdct[372]
                );
            }
        }
        if do_dump && i == 0 {
            dd::write_f32_bin("/tmp/lewtoff-debug/r_mask.bin", logmask);
        }
        floor_used[i] = floor1_fit(floor_state, logmdct, logmask, &mut floor_posts[i]);
        let posts = &floor_posts[i][..floor_state.posts as usize];
        if do_dump && i == 0 && floor_used[i] {
            dd::write_i32_bin("/tmp/lewtoff-debug/r_floor_posts.bin", posts);
        }
        if crate::debug_flag!("LW_DEBUG_FLOOR") {
            if floor_used[i] {
                eprintln!(
                    "LW_FLOOR ch={} n={} ampmax={:.2} floor_posts={:?}",
                    i, n, *ampmax, posts
                );
            } else {
                eprintln!(
                    "LW_FLOOR ch={} n={} ampmax={:.2} floor_posts=None",
                    i, n, *ampmax
                );
            }
        }
    }

    // --- Encode the packet ---
    // Port of: lines 592-688 in mapping0.c
    // (we don't do bitrate management: only k = PACKETBLOBS/2)

    // Encode packet type (audio = 0)
    opb.write(0, 1);
    // Encode mode number
    opb.write(block_mode.mode_number as u32, block_mode.modebits);
    // For long blocks (W=1): encode lW and nW window flags.
    // For short blocks (W=0): no window flags (the C code: if(vb->W) {...}).
    if block_mode.is_long {
        opb.write(if block_mode.prev_window { 1 } else { 0 }, 1);
        opb.write(if block_mode.next_window { 1 } else { 0 }, 1);
    }

    let bits_before_floor = if do_dump { opb.bit_len() } else { 0 };
    for i in 0..channels {
        let submap = mapping.chmuxlist[i];
        let floor_state_idx = mapping.floorsubmap[submap];
        let floor_state = &mut floor_states[floor_state_idx];

        nonzero[i] = floor1_encode(
            opb,
            floor_state,
            if floor_used[i] {
                Some(&mut floor_posts[i][..])
            } else {
                None
            },
            &mut iwork[i],
            books,
            n,
        );
    }
    if do_dump {
        let floor_bits = opb.bit_len() - bits_before_floor;
        dd::write_txt(
            "/tmp/lewtoff-debug/r_floor_bits.txt",
            &format!("{}\n", floor_bits),
        );
    }

    // Couple + quantize + normalize
    // Use blobno = PACKETBLOBS/2 = 7 (nominal quality blob)
    let blobno = crate::psy::PACKETBLOBS / 2;
    let w_flag = if block_mode.is_long { 1 } else { 0 };
    let sliding_lowpass = gi.sliding_lowpass[w_flag][blobno];

    crate::psy::vp_couple_quantize_normalize(
        blobno,
        gi,
        psy_look,
        &mapping.vp_mapping,
        gmdct.as_mut_slice(),
        iwork.as_mut_slice(),
        nonzero.as_mut_slice(),
        sliding_lowpass,
        channels,
    );

    // Encode residue by submap
    for submap_idx in 0..mapping.submaps {
        let res_idx = mapping.residuesubmap[submap_idx];
        let res_type = residue_types[res_idx];
        let res_setup = &residue_setups[res_idx];
        let res_look = &residue_looks[res_idx];

        // Channels in this submap, in channel order. Residue coding is the
        // last reader of iwork, so the bundle is coded in place.
        let mut zerobundle = [false; MAX_BUNDLE];
        let mut ch_in_bundle = 0usize;
        for j in 0..channels {
            if mapping.chmuxlist[j] == submap_idx {
                zerobundle[ch_in_bundle] = nonzero[j] != 0;
                ch_in_bundle += 1;
            }
        }
        let zerobundle = &zerobundle[..ch_in_bundle];
        let mut bundle: Vec<&mut [i32]> = iwork
            .iter_mut()
            .zip(&mapping.chmuxlist)
            .filter(|&(_, &m)| m == submap_idx)
            .map(|(v, _)| &mut v[..n2])
            .collect();

        match res_type {
            0 | 1 => {
                // res1 (and res0, treated same for encode purposes)
                let partword = {
                    let mut in_slices: Vec<&[i32]> = bundle.iter().map(|v| &**v).collect();
                    res1_class(
                        res_setup,
                        res_look,
                        &mut in_slices,
                        zerobundle,
                        ch_in_bundle,
                    )
                };
                if let Some(pw) = partword {
                    if crate::debug_dump::dump_enabled() {
                        use std::sync::atomic::{AtomicBool, Ordering};
                        static FIRED: AtomicBool = AtomicBool::new(false);
                        if !FIRED.swap(true, Ordering::Relaxed) {
                            let mut bytes = Vec::new();
                            for v in bundle[0].iter() {
                                bytes.extend_from_slice(&v.to_le_bytes());
                            }
                            let _ =
                                std::fs::write("/tmp/lewtoff-debug/r_residue_input.bin", &bytes);
                            // partword dump
                            let mut pw_bytes = Vec::new();
                            for ch_pw in pw.iter() {
                                for v in ch_pw.iter() {
                                    pw_bytes.extend_from_slice(&v.to_le_bytes());
                                }
                            }
                            let _ = std::fs::write("/tmp/lewtoff-debug/r_partword.bin", &pw_bytes);
                        }
                    }
                    res1_forward(
                        opb,
                        res_setup,
                        res_look,
                        &mut bundle,
                        zerobundle,
                        ch_in_bundle,
                        &pw,
                        books,
                    );
                }
            }
            2 => {
                // res2: interleaved stereo
                let in_slices: Vec<&[i32]> = bundle.iter().map(|v| &**v).collect();
                let partword =
                    res2_class(res_setup, res_look, &in_slices, zerobundle, ch_in_bundle);
                if let Some(pw) = partword {
                    res2_forward(
                        opb,
                        res_setup,
                        res_look,
                        &in_slices,
                        zerobundle,
                        ch_in_bundle,
                        n2,
                        &pw,
                        books,
                        res_work,
                    );
                }
            }
            _ => {}
        }
    }
}

/// Most channels one residue submap can bundle (lewtoff encodes mono or
/// stereo).
const MAX_BUNDLE: usize = 2;

#[cfg(test)]
mod tests {
    use super::add_345;

    /// `x + .345` must round like C (double sum, one rounding to float), not
    /// like an f32 add. Pinned from `cc -O0 -ffp-contract=off`:
    /// `float x = -0.99999994f; float r = x + .345;` gives 0xbf27ae13, where
    /// `x + .345f` gives 0xbf27ae14.
    #[test]
    fn add_345_matches_c_double_literal() {
        let x = f32::from_bits(0xbf7f_ffff);
        assert_eq!(add_345(x).to_bits(), 0xbf27_ae13);
        assert_eq!((x + 0.345_f32).to_bits(), 0xbf27_ae14);
        // Agrees with the f32 add wherever that is exact enough.
        assert_eq!(add_345(-100.0), -100.0_f32 + 0.345_f32);
    }
}
