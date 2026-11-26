//! High-level encode orchestration: i16 PCM → Ogg Vorbis bitstream.
//!
//! Implements the `encode_impl` function that wires all phases together.

#![allow(clippy::needless_range_loop)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::field_reassign_with_default)]
#![allow(clippy::manual_memcpy)]

use crate::bitpack::BitWriter;
use crate::headers::{write_comment_header_with_strings, write_id_header_with, write_setup_packet};
use crate::lpc::{lpc_from_data, lpc_predict};
use crate::mapping0;
use crate::mapping0::{BlockMode, mapping0_forward};
use crate::ogg_pages::OggStreamWriter;
use crate::psy::{VorbisLookPsy, vp_psy_init};
use crate::setup::encoder_setup_for;
use crate::window::{BlockSizes, WindowingBuffer};
use crate::{Channels, SampleRate};

// Samples per vorbis_analysis_buffer/wrote call in the reference encoder
// (ffmpeg's frame_size, the oracle's READ_CHUNK).
const ANALYSIS_CHUNK: usize = 64;
// LPC order used by libvorbis _preextrapolate_helper
const LPC_ORDER: usize = 16;

// ---------------------------------------------------------------------------
// encode_impl / encode_with_serial: main encode entry point
// ---------------------------------------------------------------------------

/// Port of libvorbis `_preextrapolate_helper`.
///
/// Given the PCM for one channel, compute `center_w` (= blocksizes[1]/2)
/// pre-stream samples.
/// Returns a Vec of `center_w` samples where index `k` corresponds to virtual
/// sample `pcm[centerW - 1 - k]` in libvorbis (i.e., predicted[0] = sample at -1).
///
/// These are filled into `win_bufs[c].prev_short` via `set_prestream`.
fn preextrapolate_channel(pcm: &[f32], center_w: usize) -> Vec<f32> {
    let n = pcm.len();

    let result = vec![0.0f32; center_w];

    // Safety check: need more than 2*LPC_ORDER samples
    if n <= LPC_ORDER * 2 {
        return result;
    }

    // work = reversed pcm (work[j] = pcm[n-j-1])
    let work: Vec<f32> = pcm.iter().rev().cloned().collect();

    // LPC from work[0..n] (= reversed pcm = forward prediction in reverse time)
    let mut lpc_coeffs = [0.0f32; LPC_ORDER];
    lpc_from_data(&work, &mut lpc_coeffs, n, LPC_ORDER);

    if crate::debug_flag!("LW_DEBUG_LPC") {
        let vals: Vec<String> = lpc_coeffs.iter().map(|v| format!("{:.10e}", v)).collect();
        eprintln!("LW_LPC_COEFFS: [{}]", vals.join(","));
    }

    // Prime: work[n-LPC_ORDER..n] (the last LPC_ORDER reversed samples = first LPC_ORDER actual)
    let prime: Vec<f32> = work[n - LPC_ORDER..n].to_vec();

    if crate::debug_flag!("LW_DEBUG_LPC") {
        let vals: Vec<String> = prime.iter().map(|v| format!("{:.10e}", v)).collect();
        eprintln!("LW_LPC_PRIME: [{}]", vals.join(","));
    }

    // Predict center_w samples forward in reversed domain
    let mut predicted = vec![0.0f32; center_w];
    lpc_predict(&lpc_coeffs, &prime, LPC_ORDER, &mut predicted, center_w);

    if crate::debug_flag!("LW_DEBUG_LPC") {
        let vals: Vec<String> = predicted[..8]
            .iter()
            .map(|v| format!("{:.10e}", v))
            .collect();
        eprintln!("LW_LPC_PREDICTED_0to7: [{}]", vals.join(","));
    }

    // Write-back in libvorbis: pcm[centerW - 1 - k] = predicted[k]
    // (equivalent to pcm[0..centerW] = predicted[centerW-1..0])
    // So result[k] = predicted[k] (k=0 is virtual sample at -1)
    predicted
}

/// q0..q10 (256/2048) pre-extrapolation, for the envelope unit tests.
#[cfg(test)]
pub(crate) fn pre_extrap_for_test(pcm: &[f32]) -> Vec<f32> {
    preextrapolate_channel(pcm, BlockSizes::BS_256_2048.long_half())
}

/// Port of libvorbis post-stream LPC extrapolation (block.c lines 487-527).
///
/// At EOS, libvorbis predicts `blocksizes[1]*3` samples beyond the actual audio
/// using LPC of order 32 trained on the last `min(eofflag, blocksizes[1])`
/// samples. The predictions fill out the right side of the last data block(s)
/// so the encoder doesn't see a discontinuity from audio to zero.
const LPC_ORDER_POST: usize = 32;

/// Post-extrapolate one channel the way libvorbis does at
/// `vorbis_analysis_wrote(0)`.
///
/// libvorbis works on its own analysis buffer `pcm[0..eofflag]`, where
/// `eofflag = pcm_current` is what is left after the pre-EOS block drains.
/// That buffer is the tail of the virtual stream
/// `[pre-extrapolated centerW samples] ++ audio`: for short inputs (nothing
/// or little drained yet) it still starts inside the LPC pre-extrapolated
/// region. The predictor is trained on `pcm[eofflag-n..eofflag]` with
/// `n = min(eofflag, blocksizes[1])`, primed with the last 32 samples, and
/// the whole thing is skipped (zero-fill) when `eofflag <= 64`.
///
/// `pre[k]` is the pre-extrapolated sample at virtual index `-(k+1)`
/// (the layout `preextrapolate_channel` returns). Returns the
/// `blocksizes[1]*3` predicted samples following the audio.
fn postextrapolate_channel(pre: &[f32], audio: &[f32], eofflag: usize, bs_long: usize) -> Vec<f32> {
    let post_len = bs_long * 3;
    let mut out = vec![0.0f32; post_len];
    if eofflag <= LPC_ORDER_POST * 2 {
        return out; // libvorbis falls back to zero-fill (memset)
    }
    let n_train = eofflag.min(bs_long);
    // Materialize the last n_train samples of [pre ++ audio].
    let total = pre.len() + audio.len();
    debug_assert!(n_train <= total);
    let mut buf = Vec::with_capacity(n_train);
    for v in (total - n_train)..total {
        buf.push(if v < pre.len() {
            pre[pre.len() - 1 - v]
        } else {
            audio[v - pre.len()]
        });
    }
    let mut lpc_coeffs = [0.0f32; LPC_ORDER_POST];
    lpc_from_data(&buf, &mut lpc_coeffs, n_train, LPC_ORDER_POST);
    lpc_predict(
        &lpc_coeffs,
        &buf[n_train - LPC_ORDER_POST..],
        LPC_ORDER_POST,
        &mut out,
        post_len,
    );
    out
}

pub(crate) fn random_serial() -> u32 {
    // Wider seed than just subsec_nanos: fold seconds and the address of a
    // local stack variable into the mix so two encodes started in the same
    // millisecond don't collide. Callers that need true uniqueness across
    // hosts should pass an explicit serial via encode_with_serial.
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let stack_addr = &d as *const _ as usize as u64;
    let mix = (d.as_nanos() as u64) ^ d.as_secs().rotate_left(17) ^ stack_addr;
    (mix as u32) ^ ((mix >> 32) as u32)
}

pub(crate) fn encode_impl(samples: &[i16], rate: SampleRate, channels: Channels) -> Vec<u8> {
    encode_with_serial_and_meta(samples, rate, channels, random_serial(), None, None)
}

pub(crate) fn encode_with_serial(
    samples: &[i16],
    rate: SampleRate,
    channels: Channels,
    serial: u32,
) -> Vec<u8> {
    encode_with_serial_and_meta(samples, rate, channels, serial, None, None)
}

pub(crate) fn encode_with_serial_and_meta(
    samples: &[i16],
    rate: SampleRate,
    channels: Channels,
    serial: u32,
    vendor: Option<&[u8]>,
    encoder_tag: Option<&[u8]>,
) -> Vec<u8> {
    encode_with_base_quality_and_meta(
        samples,
        rate,
        channels,
        Q5_BASE_QUALITY,
        serial,
        vendor,
        encoder_tag,
    )
}

/// libvorbis base_quality for oggenc `-q 5` (`(float)(5.0f * 0.1)` == 0.5f).
pub(crate) const Q5_BASE_QUALITY: f32 = 0.5;

/// Convert an oggenc-scale quality (-1..10) to the libvorbis base_quality
/// float exactly the way vorbis-tools 1.4.3 `oggenc -q` does.
///
/// Determined from the Homebrew arm64 `oggenc` binary (disassembly of the
/// `-q` option handler) and confirmed empirically by byte-comparing
/// `oggenc -q X` silence encodes against the oracle at fractional X:
///
/// ```c
/// float quality;                       /* opt->quality */
/// sscanf(optarg, "%f", &quality);      /* correctly rounded to float */
/// if (quality > 10) quality = 10;      /* "quality setting too high" */
/// quality *= 0.1;                      /* float*double -> double, stored float */
/// vorbis_encode_setup_vbr(&vi, ch, rate, quality);
/// ```
///
/// i.e. `((q as f32) as f64 * 0.1) as f32` — the multiply is by the double
/// `0.1`, not `0.1f`, and there is a single rounding back to f32. There is
/// no lower clamp in oggenc; libvorbis rejects base_quality < -0.1.
pub(crate) fn oggenc_base_quality(quality: f32) -> f32 {
    let q = if quality > 10.0 { 10.0 } else { quality };
    (q as f64 * 0.1) as f32
}

pub(crate) fn encode_with_base_quality_and_meta(
    samples: &[i16],
    rate: SampleRate,
    channels: Channels,
    base_quality: f32,
    serial: u32,
    vendor: Option<&[u8]>,
    encoder_tag: Option<&[u8]>,
) -> Vec<u8> {
    let ch = match channels {
        Channels::Mono => 1usize,
        Channels::Stereo => 2usize,
    };
    let rate_hz: i64 = match rate {
        SampleRate::Hz44100 => 44100,
        SampleRate::Hz48000 => 48000,
    };

    assert!(
        (crate::quality::MIN_BASE_QUALITY..=1.0).contains(&base_quality),
        "base_quality {base_quality} outside libvorbis's -0.1..=1.0 (oggenc -q -1..10)"
    );

    // Quality-derived parameters + setup header (codebooks, floors,
    // residues, mappings, modes), cached per (rate, channels, quality).
    let enc_setup = encoder_setup_for(rate_hz as u32, ch, base_quality);
    let cfg = &enc_setup.cfg;
    let setup = &enc_setup.setup;
    // 256/2048 for q0..q10, 512/4096 for q-1.
    let bs = BlockSizes {
        short: cfg.blocksizes[0],
        long: cfg.blocksizes[1],
    };
    // libvorbis centerW = blocksizes[1]/2: the pre-extrapolated prefix of
    // its analysis buffer and the offset of audio[0] in envelope space.
    let center_w = bs.long_half();
    let post_len = bs.long * 3;

    // libvorbis VBR setups always have mode 0 = short (blockflag=0) and
    // mode 1 = long (blockflag=1).
    let short_mode_number = setup
        .modes
        .iter()
        .position(|m| !m.blockflag)
        .expect("setup must contain a short-block mode");
    let long_mode_number = setup
        .modes
        .iter()
        .position(|m| m.blockflag)
        .expect("setup must contain a long-block mode");
    let long_mode = &setup.modes[long_mode_number];
    let long_mapping = &setup.mappings[long_mode.mapping];
    let short_mode = &setup.modes[short_mode_number];
    let short_mapping = &setup.mappings[short_mode.mapping];

    // Build psy state per libvorbis block type (ci->psy_param[0..4]):
    // impulse / padding (short, n2 = bs0/2), transition / long (n2 = bs1/2).
    let gi = &cfg.psy_global;
    let [vi_impulse, vi_padding, vi_transition, vi_long] = cfg.psy.clone();
    let psy_look_long: VorbisLookPsy = vp_psy_init(vi_long, gi, bs.long_half(), rate_hz);
    let psy_look_transition: VorbisLookPsy =
        vp_psy_init(vi_transition, gi, bs.long_half(), rate_hz);
    let psy_look_short: VorbisLookPsy = vp_psy_init(vi_impulse, gi, bs.short_half(), rate_hz);
    let psy_look_padding: VorbisLookPsy = vp_psy_init(vi_padding, gi, bs.short_half(), rate_hz);

    // De-interleave input into per-channel buffers
    let total_samples = if ch > 1 {
        samples.len() / ch
    } else {
        samples.len()
    };
    let pcm_channels: Vec<Vec<f32>> = (0..ch)
        .map(|c| {
            if ch == 1 {
                samples.iter().map(|&s| s as f32 / 32768.0).collect()
            } else {
                samples
                    .chunks_exact(ch)
                    .map(|frame| frame[c] as f32 / 32768.0)
                    .collect()
            }
        })
        .collect();

    // Block layout: libvorbis opens with 1 or 2 short (impulse) blocks (per
    // envelope detection of leading transients), then a long transition
    // block, then long mainline blocks, then a flush block at EOS.
    //
    //   first_long_start = (n_short_blocks - 1) * short/2 + 576 (256/2048)
    //   - 576 for n_short=1 (sine/silence)
    //   - 704 for n_short=2 (ramp transient)
    //
    // n_short_blocks is determined by running envelope detection on the
    // pre-extrapolated PCM buffer (LPC virtual pre-stream + audio); see
    // envelope.rs for the port.
    // Pre-compute the W-flag sequence for the entire stream by running
    // libvorbis's envelope-driven `_ve_envelope_search` rule across all
    // marks. The pattern is a Vec<i32> of 0=short / 1=long, one entry per
    // emitted block (including any EOS flush blocks).
    // libvorbis's envelope_search reads its own current pcm buffer, which
    // includes post-extrapolated samples after EOS. The post-extrap LPC
    // training window is `min(eofflag, blocksizes[1])` where eofflag is
    // pcm_current at vorbis_analysis_wrote(0) — i.e. the residual buffer
    // after pre-EOS drains, which is the tail of [pre-extrap ++ audio] (see
    // `postextrapolate_channel`). Getting eofflag wrong produces diverging
    // post-extrap samples → diverging envelope marks at the very end of the
    // stream. We resolve the chicken-and-egg (eofflag depends on marks via
    // `simulate_eofflag_bs`, marks depend on env_pcm post-extrap which depends
    // on eofflag) by iterating to a fixed point. In practice 2 passes
    // suffice (long streams converge in 1 since n_train clamps to bs1
    // either way).
    // Pre-extrapolation fires once pcm_current - centerW > blocksizes[1],
    // i.e. after blocksizes[1] + one 64-sample analysis chunk (2112 for
    // 256/2048, 4160 for 512/4096).
    let lpc_n = bs.long + ANALYSIS_CHUNK;
    let lpc_train_end = lpc_n.min(total_samples);
    let pre_per_channel: Vec<Vec<f32>> = (0..ch)
        .map(|c| preextrapolate_channel(&pcm_channels[c][..lpc_train_end], center_w))
        .collect();
    let build_post = |eofflag: usize| -> Vec<Vec<f32>> {
        (0..ch)
            .map(|c| {
                postextrapolate_channel(&pre_per_channel[c], &pcm_channels[c], eofflag, bs.long)
            })
            .collect()
    };
    let build_env_pcm = |post: &[Vec<f32>]| -> Vec<Vec<f32>> {
        let mut env_pcm: Vec<Vec<f32>> = Vec::with_capacity(ch);
        for c in 0..ch {
            let mut buf = Vec::with_capacity(center_w + total_samples + post_len);
            buf.extend(pre_per_channel[c].iter().rev());
            buf.extend_from_slice(&pcm_channels[c]);
            buf.extend_from_slice(&post[c]);
            env_pcm.push(buf);
        }
        env_pcm
    };
    // The post-extrapolation depends on eofflag only through
    // n = min(eofflag, bs1) and the `eofflag > 2*order` guard.
    let post_key = |eofflag: usize| (eofflag.min(bs.long), eofflag > LPC_ORDER_POST * 2);

    // Initial guess: nothing drained before EOS (eofflag = centerW + audio);
    // exact for short inputs, and long inputs clamp to n = bs1 anyway.
    let mut eofflag_lpc = center_w + total_samples;
    let mut iterations = 0;
    let (block_w, env_marks, block_curmarks, last_block_nw, post_predicted) = loop {
        let post = build_post(eofflag_lpc);
        let env_pcm = build_env_pcm(&post);
        let marks = crate::envelope::compute_marks(&env_pcm, gi);
        // Re-derive eofflag using these marks; if it implies a different
        // post-extrapolation, rebuild env_pcm (and thus the marks).
        let eof_candidate = crate::envelope::simulate_eofflag_bs(
            bs,
            &marks,
            total_samples as i64,
            ANALYSIS_CHUNK as i64,
        ) as usize;
        iterations += 1;
        if post_key(eof_candidate) == post_key(eofflag_lpc) || iterations >= 8 {
            debug_assert!(post_key(eof_candidate) == post_key(eofflag_lpc));
            eofflag_lpc = eof_candidate;
            // libvorbis terminates emission at v->centerW >= v->eofflag.
            // eofflag is the END OF REAL AUDIO position. In env_pcm coords
            // that's center_w (pre-extrap) + total_samples; post-extrap
            // content extends the marks array but doesn't extend eofflag.
            let n_eof = (center_w + total_samples) as i64;
            let (pattern, curmarks, last_nw) =
                crate::envelope::full_w_pattern_with_curmark_bs(bs, &marks, n_eof);
            break (pattern, marks, curmarks, last_nw, post);
        }
        eofflag_lpc = eof_candidate;
    };
    if crate::debug_flag!("LW_DEBUG_EOFFLAG") {
        eprintln!(
            "R_EOFFLAG eofflag_lpc={} total_samples={} iterations={}",
            eofflag_lpc, total_samples, iterations
        );
    }

    // Map W-flag sequence to per-block sample positions (block_start). Each
    // block reads `bs[W]/2` "current half" samples starting at block_start.
    // block_start advances by `bs[prev_W]/4 + bs[W]/4` from the previous
    // block's start (libvorbis centerW evolution; first block at 0).
    let block_starts: Vec<usize> = {
        let mut starts = Vec::with_capacity(block_w.len());
        let mut start: usize = 0;
        let mut prev_w: i32 = 0;
        for (i, &w) in block_w.iter().enumerate() {
            if i > 0 {
                start += bs.get(prev_w == 1) / 4 + bs.get(w == 1) / 4;
            }
            starts.push(start);
            prev_w = w;
        }
        starts
    };

    // Per-short-block IMPULSE vs PADDING decision (libvorbis _ve_envelope_mark).
    // A short block is IMPULSE if any envelope mark falls within centerW±bs/2.
    // centerW for envelope-mark space includes the LPC pre-extrap offset
    // (center_w = blocksizes[1]/2 samples).
    // libvorbis decides IMPULSE vs PADDING for each short block via
    // _ve_envelope_mark, which scans the envelope-mark buffer in a window
    // around centerW. For the very first short block, libvorbis fires its
    // preecho mark at j≈15 due to LPC pre-extrap edge effects on signals
    // that grow in amplitude across audio[0..SHORT_BLOCK] (sine 440 starts
    // at sin(0)=0 and reaches ~0.5 by sample 64). Our _ve_amp port has
    // 1-ULP drift that misses this mark on tonal input. Backstop: for the
    // first short block, also force IMPULSE when the audio's RANGE within
    // [0..SHORT_BLOCK] exceeds a threshold. Range (not max) is what
    // libvorbis actually keys off; flat-but-loud signals like a constant
    // tone (snd_time_countdown.wav starts at 1341 and barely moves) are
    // PADDING in libvorbis even though their amplitude is non-trivial.
    let block_is_impulse: Vec<bool> = block_starts
        .iter()
        .zip(block_w.iter())
        .zip(block_curmarks.iter())
        .map(|((&start, &w), &curmark)| {
            if w != 0 {
                false
            } else {
                let center_w_in_env = (start + center_w) as i64;
                crate::envelope::short_is_impulse_bs(bs, &env_marks, curmark, center_w_in_env)
            }
        })
        .collect();
    let total_blocks = block_w.len();

    if crate::debug_flag!("LW_DEBUG_BLOCKW") {
        let mut s = String::from("R_BLOCKW: [");
        for (i, &w) in block_w.iter().enumerate() {
            if i > 0 {
                s.push(' ');
            }
            s.push_str(&format!(
                "{}{}",
                w,
                if block_is_impulse[i] { "I" } else { "" }
            ));
        }
        s.push(']');
        eprintln!("{}", s);
    }

    // OGG writer
    let mut ogg = OggStreamWriter::new(serial);

    // Write the three header packets (force page flushes per Vorbis spec)
    {
        let mut w = BitWriter::new();
        write_id_header_with(rate, channels, cfg.bitrate_nominal, cfg.blocksizes, &mut w);
        ogg.write_packet(&w.into_bytes(), 0, false, true);
    }
    {
        let mut w = BitWriter::new();
        write_comment_header_with_strings(&mut w, vendor, encoder_tag);
        ogg.write_packet(&w.into_bytes(), 0, false, false);
    }
    {
        let mut w = BitWriter::new();
        write_setup_packet(&enc_setup.packet, &mut w);
        ogg.write_packet(&w.into_bytes(), 0, false, true);
    }

    // Windowing buffers per channel
    let mut win_bufs: Vec<WindowingBuffer> = (0..ch)
        .map(|_| WindowingBuffer::with_blocksizes(bs))
        .collect();

    // Pre-stream LPC extrapolation (port of libvorbis _preextrapolate_helper).
    // Fill in virtual samples before the stream start so the first short
    // block's left half contains LPC-predicted continuations instead of
    // zeros. libvorbis triggers the helper once pcm_current - centerW >
    // blocksizes[1], i.e. after bs1 + one 64-sample chunk of audio (lpc_n),
    // and trains on that audio reversed (pre_per_channel above). Only the
    // short/2 samples nearest the stream start reach the first block.
    for c in 0..ch {
        // pre_per_channel[c][k] = virtual sample at -(k+1)
        win_bufs[c].set_prestream(&pre_per_channel[c][..bs.short_half()]);
    }

    // Per-block analysis state. Under `feature = "parallel"`, blocks are
    // collected into a Vec<BlockJob> and processed via Rayon; under default
    // (serial) features, mapping0_forward is called inline with mutable
    // floor_states/ampmax/scratch accumulating across blocks.
    #[cfg(feature = "parallel")]
    let floor_states_seed: Vec<crate::floor1::Floor1State> = setup
        .floor_states
        .iter()
        .map(|s| crate::floor1::floor1_look(s.vi.clone()))
        .collect();

    #[cfg(not(feature = "parallel"))]
    let mut floor_states: Vec<crate::floor1::Floor1State> = setup
        .floor_states
        .iter()
        .map(|s| crate::floor1::floor1_look(s.vi.clone()))
        .collect();
    #[cfg(not(feature = "parallel"))]
    let mut ampmax = -9999.0f32;
    #[cfg(not(feature = "parallel"))]
    let mut mapping_scratch = mapping0::MappingScratch::new(ch);

    // Cumulative decoded samples (for granule position)
    let mut cumulative_granule: u64 = 0;

    #[cfg(feature = "parallel")]
    enum PsyLookId {
        Short,
        Padding,
        Long,
        Transition,
    }
    #[cfg(feature = "parallel")]
    struct BlockJob {
        windowed_blocks: Vec<Vec<f32>>,
        block_mode: BlockMode,
        granule_pos: u64,
        is_last: bool,
        psy_look_id: PsyLookId,
        is_long: bool,
    }
    #[cfg(feature = "parallel")]
    let mut jobs: Vec<BlockJob> = Vec::with_capacity(total_blocks);

    // Copy `dst.len()` samples of channel `c` starting at `start` from
    // [audio ++ post-extrapolation ++ zeros].
    let read_range = |c: usize, start: usize, dst: &mut [f32]| {
        let audio = &pcm_channels[c];
        let post = &post_predicted[c];
        let mut k = 0;
        let mut idx = start;
        if idx < total_samples {
            let n = dst.len().min(total_samples - idx);
            dst[..n].copy_from_slice(&audio[idx..idx + n]);
            k = n;
            idx += n;
        }
        if k < dst.len() {
            let p = idx - total_samples;
            if p < post.len() {
                let n = (dst.len() - k).min(post.len() - p);
                dst[k..k + n].copy_from_slice(&post[p..p + n]);
                k += n;
            }
            dst[k..].fill(0.0);
        }
    };
    // Current-half scratch, reused across blocks and channels.
    let mut cur_buf = vec![0.0f32; bs.long_half()];

    for block_idx in 0..total_blocks {
        let cur_w = block_w[block_idx];
        let prev_w = if block_idx == 0 {
            0
        } else {
            block_w[block_idx - 1]
        };
        // The last block's nW comes from libvorbis's envelope search on the
        // post-EOS buffer (0 when it hits a mark or runs out of data).
        let next_w = if block_idx + 1 < total_blocks {
            block_w[block_idx + 1]
        } else {
            last_block_nw
        };
        let is_short = cur_w == 0;
        let is_last = block_idx == total_blocks - 1;

        let prev_is_long = prev_w == 1;
        let nw_is_long = next_w == 1;

        let block_start = block_starts[block_idx];

        let windowed_blocks: Vec<Vec<f32>>;
        let block_mode: BlockMode;

        if is_short {
            let sh = bs.short_half();
            windowed_blocks = (0..ch)
                .map(|c| {
                    let current = &mut cur_buf[..sh];
                    read_range(c, block_start, current);
                    let mut out = vec![0.0f32; bs.short];
                    win_bufs[c].push_short_block_into(current, &mut out);
                    out
                })
                .collect();

            if block_idx == 0
                && crate::debug_dump::dump_enabled()
                && crate::debug_dump::try_claim_first_short_block()
            {
                std::fs::create_dir_all("/tmp/lewtoff-debug").ok();
                crate::debug_dump::write_f32_bin(
                    "/tmp/lewtoff-debug/r_windowed.bin",
                    &windowed_blocks[0],
                );
            }

            block_mode = BlockMode {
                mode_number: short_mode_number,
                modebits: setup.modebits,
                is_long: false,
                prev_window: false,
                next_window: false,
            };
        } else {
            // For a long block whose previous block was short, the un-windowed
            // middle section of the analysis frame needs the long/4 - short/4
            // samples preceding `block_start` (448 for 256/2048, 896 for
            // 512/4096: the gap between the prev short block's right edge and
            // this long block's current half).
            let mid_len = bs.long_short_gap();
            let pre_current_data: Option<Vec<Vec<f32>>> = if !prev_is_long {
                Some(
                    (0..ch)
                        .map(|c| {
                            let pre_start = block_start.saturating_sub(mid_len);
                            let pre_end = block_start;
                            let mut pre = vec![0.0f32; mid_len];
                            read_range(c, pre_start, &mut pre[..pre_end - pre_start]);
                            pre
                        })
                        .collect(),
                )
            } else {
                None
            };

            let lh = bs.long_half();
            windowed_blocks = (0..ch)
                .map(|c| {
                    let current = &mut cur_buf[..lh];
                    read_range(c, block_start, current);
                    let pre = pre_current_data.as_ref().map(|v| v[c].as_slice());
                    let mut out = vec![0.0f32; bs.long];
                    win_bufs[c].push_long_block_into(current, pre, nw_is_long, &mut out);
                    out
                })
                .collect();

            block_mode = BlockMode {
                mode_number: long_mode_number,
                modebits: setup.modebits,
                is_long: true,
                prev_window: prev_is_long,
                next_window: nw_is_long,
            };
        }

        // libvorbis sets vb->granulepos = v->granulepos at packet emit time,
        // BEFORE adding this block's movementW (= bs[W]/4 + bs[nW]/4). So the
        // first packet has granulepos = 0, and each subsequent packet's
        // granulepos accumulates movementW from PRIOR blocks. At EOS, libvorbis
        // caps granulepos so final = total_samples (subtracting the centerW
        // over-shoot beyond eofflag).
        debug_assert!(
            is_last || cumulative_granule <= total_samples as u64,
            "cumulative_granule overshoot on non-final packet: {} > {}",
            cumulative_granule,
            total_samples
        );
        let granule_pos = if is_last {
            cumulative_granule.min(total_samples as u64)
        } else {
            cumulative_granule
        };
        let movement_w = (bs.get(cur_w == 1) / 4 + bs.get(next_w == 1) / 4) as u64;
        if is_last {
            // Cap at total_samples on the final block (mirroring libvorbis's
            // movementW - (centerW - eofflag) clamp).
            cumulative_granule = (cumulative_granule + movement_w).min(total_samples as u64);
        } else {
            cumulative_granule += movement_w;
        }

        // libvorbis: BLOCKTYPE_IMPULSE (psy[0]) when short block has an
        // envelope mark in its neighborhood; BLOCKTYPE_PADDING (psy[1])
        // otherwise. BLOCKTYPE_TRANSITION (psy[2]) when long block has !lW
        // or !nW; BLOCKTYPE_LONG (psy[3]) only when both lW and nW are long.
        #[cfg(not(feature = "parallel"))]
        {
            let psy_look = if is_short {
                if block_is_impulse[block_idx] {
                    &psy_look_short
                } else {
                    &psy_look_padding
                }
            } else if block_mode.prev_window && block_mode.next_window {
                &psy_look_long
            } else {
                &psy_look_transition
            };
            let mapping = if is_short {
                short_mapping
            } else {
                long_mapping
            };
            let mut w = BitWriter::new();
            mapping0_forward(
                &windowed_blocks,
                psy_look,
                gi,
                &mut ampmax,
                &mut floor_states,
                &setup.residue_types,
                &setup.residue_setups,
                &setup.residue_looks,
                mapping,
                &block_mode,
                &setup.books,
                &mut mapping_scratch,
                &mut w,
            );
            let packet_bytes = w.into_bytes();
            ogg.write_packet(&packet_bytes, granule_pos, is_last, false);
        }

        #[cfg(feature = "parallel")]
        {
            let psy_look_id = if is_short {
                if block_is_impulse[block_idx] {
                    PsyLookId::Short
                } else {
                    PsyLookId::Padding
                }
            } else if block_mode.prev_window && block_mode.next_window {
                PsyLookId::Long
            } else {
                PsyLookId::Transition
            };
            jobs.push(BlockJob {
                windowed_blocks,
                block_mode,
                granule_pos,
                is_last,
                psy_look_id,
                is_long: !is_short,
            });
        }
    }

    #[cfg(feature = "parallel")]
    {
        use rayon::prelude::*;

        // Pre-pass A (parallel): each block computes max_ch(clamped
        // local_ampmax) by running just the FFT slice. (Caching gmdct/logfft
        // into the main pass to avoid the redundant work was tried and
        // proved a wash — ~90MB of cache pressure ate the FFT savings.)
        let max_local_per_block: Vec<f32> = jobs
            .par_iter()
            .map(|job| mapping0::compute_block_local_ampmax_max(&job.windowed_blocks))
            .collect();

        // Pre-pass B (sequential): chain the recurrence
        //     decayed[i] = vp_ampmax_decay(prev[i], n2_i, rate)
        //     ending[i]  = max(decayed[i], max_local[i])
        //     prev[i+1]  = ending[i]
        // to recover the input ampmax mapping0_forward would see in serial.
        let mut input_ampmax_per_block: Vec<f32> = Vec::with_capacity(jobs.len());
        {
            let mut prev = -9999.0f32;
            for (job, max_local) in jobs.iter().zip(max_local_per_block.iter()) {
                input_ampmax_per_block.push(prev);
                let n2 = bs.get(job.is_long) / 2;
                let decayed = crate::psy::vp_ampmax_decay(prev, gi, n2, rate_hz);
                prev = decayed.max(*max_local);
            }
        }

        // Main pass (parallel): each block runs mapping0_forward against
        // a thread-local MappingScratch and floor_states. Floor1 stats
        // (phrasebits/postbits/frames) are instrumentation only — they
        // accumulate per-thread but don't affect packet bytes.
        let packets: Vec<Vec<u8>> = jobs
            .par_iter()
            .zip(input_ampmax_per_block.par_iter())
            .map_init(
                || (mapping0::MappingScratch::new(ch), floor_states_seed.clone()),
                |(scratch, floor_states_local), (job, input_ampmax)| {
                    let psy_look = match job.psy_look_id {
                        PsyLookId::Short => &psy_look_short,
                        PsyLookId::Padding => &psy_look_padding,
                        PsyLookId::Long => &psy_look_long,
                        PsyLookId::Transition => &psy_look_transition,
                    };
                    let mapping = if job.is_long {
                        long_mapping
                    } else {
                        short_mapping
                    };
                    let mut ampmax_local = *input_ampmax;
                    let mut w = BitWriter::new();
                    mapping0_forward(
                        &job.windowed_blocks,
                        psy_look,
                        gi,
                        &mut ampmax_local,
                        floor_states_local,
                        &setup.residue_types,
                        &setup.residue_setups,
                        &setup.residue_looks,
                        mapping,
                        &job.block_mode,
                        &setup.books,
                        scratch,
                        &mut w,
                    );
                    w.into_bytes()
                },
            )
            .collect();

        // Sequential packet emission preserves Ogg page ordering.
        for (job, packet) in jobs.iter().zip(packets.iter()) {
            ogg.write_packet(packet, job.granule_pos, job.is_last, false);
        }
    }

    ogg.into_bytes()
}
