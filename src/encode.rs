//! Streaming encode core: interleaved PCM in, Ogg Vorbis pages out.
//!
//! [`EncoderCore`] is a literal port of libvorbis 1.3.7's analysis state
//! machine (`vorbis_analysis_wrote` / `vorbis_analysis_blockout` /
//! `_ve_envelope_search` / `_ve_envelope_mark`), driven the way
//! `tools/oracle-encoder/main.c` drives libvorbis: input is handed to the
//! state machine in 64-frame `wrote` chunks, each followed by a greedy
//! `blockout` drain. The chunking is internal, so the output is identical
//! for any way the caller splits its input across `write` calls (see
//! `docs/streaming-design.md`).
//!
//! Coordinates are absolute: index 0 is the start of libvorbis's analysis
//! buffer at init, i.e. `centerW = blocksizes[1]/2` pre-extrapolated
//! samples followed by the audio. libvorbis keeps its buffer relative to
//! `total_shift = centerW - blocksizes[1]/2` and `memmove`s it after every
//! block; we keep absolute indices and compact the physical buffers lazily.
//! All shifts are multiples of the 64-sample envelope step, so every
//! `x / searchstep` in the C code maps to the same absolute mark index.
//!
//! The one-shot functions in `lib.rs` are thin wrappers: `new`, one feed of
//! the whole input, `finish`.

#![allow(clippy::too_many_arguments)]

use std::sync::Arc;

use crate::bitpack::BitWriter;
use crate::envelope::{EnvelopeLookup, VE_POST, VE_WIN};
use crate::floor1::Floor1State;
use crate::headers::{write_comment_header, write_id_header_with, write_setup_packet};
use crate::lpc::{lpc_from_data, lpc_predict};
use crate::mapping0::{BlockMode, MappingScratch, mapping0_forward};
use crate::ogg_pages::OggStreamWriter;
use crate::psy::{VorbisLookPsy, vp_psy_init};
use crate::setup::{EncoderSetup, encoder_setup_for};
use crate::window::{BlockSizes, apply_window_bs};
use crate::{Channels, SampleRate};

/// Frames per `vorbis_analysis_wrote` call in the reference driver
/// (`READ_CHUNK` in tools/oracle-encoder/main.c). Only the
/// pre-extrapolation trigger (`n_lpc = (bs1/64 + 1) * 64`) depends on it.
const ANALYSIS_CHUNK: usize = 64;
/// `ve->searchstep`.
const SEARCHSTEP: i64 = 64;
/// LPC order of libvorbis `_preextrapolate_helper`.
const LPC_ORDER: usize = 16;
/// LPC order of the end-of-stream extrapolation in `vorbis_analysis_wrote`.
const LPC_ORDER_POST: usize = 32;
/// Compact the PCM / mark buffers once this many frames below the live
/// window have accumulated. Bounds the buffer at roughly
/// `COMPACT_FRAMES + 2*bs1` frames independent of input length.
const COMPACT_FRAMES: i64 = 4096;

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

/// Port of libvorbis `_preextrapolate_helper` for one channel.
///
/// `audio` is the channel's audio fed so far (`pcm[centerW..pcm_current]`).
/// Returns `center_w` predicted samples where index `k` is the virtual
/// sample `pcm[centerW - 1 - k]` (i.e. `predicted[0]` is the sample at -1).
fn preextrapolate_channel(audio: &[f32], center_w: usize) -> Vec<f32> {
    let n = audio.len();
    // work = reversed pcm (work[j] = pcm[pcm_current-j-1]); only its first
    // n entries (the reversed audio) feed the LPC fit and the prime.
    let work: Vec<f32> = audio.iter().rev().cloned().collect();
    let mut lpc_coeffs = [0.0f32; LPC_ORDER];
    lpc_from_data(&work, &mut lpc_coeffs, n, LPC_ORDER);
    if crate::debug_flag!("LW_DEBUG_LPC") {
        let vals: Vec<String> = lpc_coeffs.iter().map(|v| format!("{:.10e}", v)).collect();
        eprintln!("LW_LPC_COEFFS: [{}]", vals.join(","));
    }
    let mut predicted = vec![0.0f32; center_w];
    lpc_predict(
        &lpc_coeffs,
        &work[n - LPC_ORDER..n],
        LPC_ORDER,
        &mut predicted,
        center_w,
    );
    predicted
}

/// q0..q10 (256/2048) pre-extrapolation, for the envelope unit tests.
#[cfg(test)]
pub(crate) fn pre_extrap_for_test(pcm: &[f32]) -> Vec<f32> {
    preextrapolate_channel(pcm, BlockSizes::BS_256_2048.long_half())
}

/// libvorbis `vbi->blocktype`; selects `psy_look[blocktype]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockType {
    Impulse = 0,
    Padding = 1,
    Transition = 2,
    Long = 3,
}

/// What `vorbis_analysis_blockout` hands to `vorbis_analysis`.
#[derive(Clone, Copy, Debug)]
struct Block {
    /// Absolute `centerW` of the block.
    center: i64,
    lw: bool,
    w: bool,
    nw: bool,
    blocktype: BlockType,
    granulepos: u64,
    eos: bool,
}

#[cfg(feature = "parallel")]
struct BlockJob {
    pcm: Vec<Vec<f32>>,
    mode: BlockMode,
    blocktype: BlockType,
    granulepos: u64,
    eos: bool,
}

/// Default parallel batch: enough blocks to keep every worker busy while
/// holding only a few hundred KB of windowed PCM.
#[cfg(feature = "parallel")]
fn default_batch() -> usize {
    8 * rayon::current_num_threads().max(1)
}

pub(crate) struct EncoderCore {
    setup: Arc<EncoderSetup>,
    /// `psy_look[blocktype]`: impulse, padding, transition, long.
    psy_looks: [VorbisLookPsy; 4],
    bs: BlockSizes,
    ch: usize,
    short_mode: usize,
    long_mode: usize,

    // --- PCM (libvorbis v->pcm) ---
    /// Physical per-channel buffers; `pcm[c][i]` is absolute index `base + i`.
    pcm: Vec<Vec<f32>>,
    base: i64,
    /// `v->pcm_current` (absolute): frames handed to `wrote` so far.
    pcm_current: i64,
    /// Frames appended to `pcm` but not yet passed to `wrote` (< 64).
    unwritten: usize,
    /// Left sample of a stereo frame split across `write` calls.
    partial: Option<f32>,
    /// `v->preextrapolate`.
    preextrapolated: bool,
    /// `v->eofflag`: 0 = open, > 0 = absolute end of real audio, -1 = done.
    eofflag: i64,

    // --- envelope (ve) ---
    env: EnvelopeLookup,
    /// `ve->mark`; `marks[i]` is absolute mark index `marks_base + i`.
    marks: Vec<bool>,
    marks_base: i64,
    /// `ve->current`, `ve->cursor`, `ve->curmark` (absolute).
    ve_current: i64,
    cursor: i64,
    curmark: i64,

    // --- block sequencing ---
    lw: bool,
    w: bool,
    /// `v->centerW` (absolute).
    center_w: i64,
    /// `v->granulepos`.
    granulepos: i64,

    // --- analysis ---
    ampmax: f32,
    #[cfg(not(feature = "parallel"))]
    floor_states: Vec<Floor1State>,
    #[cfg(not(feature = "parallel"))]
    scratch: MappingScratch,
    /// Windowed block scratch, `[short, long]` x channel.
    #[cfg(not(feature = "parallel"))]
    win: [Vec<Vec<f32>>; 2],
    #[cfg(feature = "parallel")]
    floor_seed: Vec<Floor1State>,
    #[cfg(feature = "parallel")]
    jobs: Vec<BlockJob>,
    #[cfg(feature = "parallel")]
    batch: usize,
    first_block_done: bool,
    /// W of every emitted block (unit tests).
    #[cfg(test)]
    w_log: Vec<bool>,

    ogg: OggStreamWriter,
    /// `ogg.output()` was handed to the caller and must be cleared before
    /// more pages are appended.
    output_returned: bool,
}

impl EncoderCore {
    pub(crate) fn new(
        rate: SampleRate,
        channels: Channels,
        base_quality: f32,
        serial: u32,
        vendor: &[u8],
        comments: &[Vec<u8>],
    ) -> Self {
        let ch = match channels {
            Channels::Mono => 1usize,
            Channels::Stereo => 2usize,
        };
        let rate_hz: i64 = match rate {
            SampleRate::Hz44100 => 44100,
            SampleRate::Hz48000 => 48000,
        };
        // Guaranteed by `Quality`.
        debug_assert!(
            (crate::quality::MIN_BASE_QUALITY..=1.0).contains(&base_quality),
            "base_quality {base_quality} outside libvorbis's -0.1..=1.0 (oggenc -q -1..10)"
        );

        let setup = encoder_setup_for(rate_hz as u32, ch, base_quality);
        let cfg = &setup.cfg;
        // 256/2048 for q0..q10, 512/4096 for q-1.
        let bs = BlockSizes {
            short: cfg.blocksizes[0],
            long: cfg.blocksizes[1],
        };
        // libvorbis VBR setups have mode 0 = short and mode 1 = long.
        let short_mode = setup
            .setup
            .modes
            .iter()
            .position(|m| !m.blockflag)
            .expect("setup must contain a short-block mode");
        let long_mode = setup
            .setup
            .modes
            .iter()
            .position(|m| m.blockflag)
            .expect("setup must contain a long-block mode");

        // ci->psy_param[0..4]: impulse / padding (n2 = bs0/2), transition /
        // long (n2 = bs1/2).
        let gi = &cfg.psy_global;
        let [vi_impulse, vi_padding, vi_transition, vi_long] = cfg.psy.clone();
        let psy_looks = [
            vp_psy_init(vi_impulse, gi, bs.short_half(), rate_hz),
            vp_psy_init(vi_padding, gi, bs.short_half(), rate_hz),
            vp_psy_init(vi_transition, gi, bs.long_half(), rate_hz),
            vp_psy_init(vi_long, gi, bs.long_half(), rate_hz),
        ];

        let mut ogg = OggStreamWriter::new(serial);
        {
            let mut w = BitWriter::new();
            write_id_header_with(rate, channels, cfg.bitrate_nominal, cfg.blocksizes, &mut w);
            ogg.write_packet(&w.into_bytes(), 0, false, true);
        }
        {
            let mut w = BitWriter::new();
            write_comment_header(&mut w, vendor, comments);
            ogg.write_packet(&w.into_bytes(), 0, false, false);
        }
        {
            let mut w = BitWriter::new();
            write_setup_packet(&setup.packet, &mut w);
            ogg.write_packet(&w.into_bytes(), 0, false, true);
        }

        let center0 = bs.long_half() as i64;
        let cap = COMPACT_FRAMES as usize + 4 * bs.long;
        let pcm = (0..ch)
            .map(|_| {
                // pcm[0..centerW] is calloc'd: zero until pre-extrapolated.
                let mut v = Vec::with_capacity(cap);
                v.resize(bs.long_half(), 0.0f32);
                v
            })
            .collect();
        let floor_states: Vec<Floor1State> = setup
            .setup
            .floor_states
            .iter()
            .map(|s| crate::floor1::floor1_look(s.vi.clone()))
            .collect();
        let minenergy = gi.preecho_minenergy;

        Self {
            psy_looks,
            bs,
            ch,
            short_mode,
            long_mode,
            pcm,
            base: 0,
            pcm_current: center0,
            unwritten: 0,
            partial: None,
            preextrapolated: false,
            eofflag: 0,
            env: EnvelopeLookup::new(ch, minenergy),
            marks: Vec::with_capacity((cap as i64 / SEARCHSTEP) as usize + 8),
            marks_base: 0,
            ve_current: 0,
            cursor: center0,
            curmark: 0,
            lw: false,
            w: false,
            center_w: center0,
            granulepos: 0,
            ampmax: -9999.0,
            #[cfg(not(feature = "parallel"))]
            floor_states,
            #[cfg(not(feature = "parallel"))]
            scratch: MappingScratch::new(ch),
            #[cfg(not(feature = "parallel"))]
            win: [vec![vec![0.0; bs.short]; ch], vec![vec![0.0; bs.long]; ch]],
            #[cfg(feature = "parallel")]
            floor_seed: floor_states,
            #[cfg(feature = "parallel")]
            jobs: Vec::new(),
            #[cfg(feature = "parallel")]
            batch: default_batch(),
            first_block_done: false,
            #[cfg(test)]
            w_log: Vec::new(),
            ogg,
            output_returned: false,
            setup,
        }
    }

    // -----------------------------------------------------------------
    // Input
    // -----------------------------------------------------------------

    /// Append interleaved samples (any count; a trailing half stereo frame
    /// is held until the next call) and run every block that becomes
    /// available.
    pub(crate) fn feed<T: Copy>(&mut self, samples: &[T], conv: impl Fn(T) -> f32) {
        debug_assert_eq!(self.eofflag, 0, "feed after finish");
        let mut s = samples;
        if let Some(left) = self.partial {
            let Some((&right, rest)) = s.split_first() else {
                return;
            };
            self.partial = None;
            self.pcm[0].push(left);
            self.pcm[1].push(conv(right));
            self.frames_appended(1);
            s = rest;
        }
        let ch = self.ch;
        let frames = s.len() / ch;
        let mut i = 0;
        while i < frames {
            let n = (ANALYSIS_CHUNK - self.unwritten).min(frames - i);
            let chunk = &s[i * ch..(i + n) * ch];
            if ch == 1 {
                self.pcm[0].extend(chunk.iter().map(|&x| conv(x)));
            } else {
                let (l, r) = self.pcm.split_at_mut(1);
                let (l, r) = (&mut l[0], &mut r[0]);
                for f in chunk.chunks_exact(2) {
                    l.push(conv(f[0]));
                    r.push(conv(f[1]));
                }
            }
            self.frames_appended(n);
            i += n;
        }
        if s.len() % ch != 0 {
            self.partial = Some(conv(s[s.len() - 1]));
        }
    }

    fn frames_appended(&mut self, n: usize) {
        self.unwritten += n;
        if self.unwritten == ANALYSIS_CHUNK {
            self.wrote(ANALYSIS_CHUNK);
        }
    }

    /// `vorbis_analysis_wrote(v, vals)` with `vals > 0`, then a full
    /// `blockout` drain (what the reference driver does after each chunk).
    fn wrote(&mut self, vals: usize) {
        debug_assert_eq!(vals, self.unwritten);
        self.unwritten = 0;
        self.pcm_current += vals as i64;
        if !self.preextrapolated
            && self.pcm_current - self.bs.long_half() as i64 > self.bs.long as i64
        {
            self.preextrapolate();
        }
        self.drain();
    }

    /// `_preextrapolate_helper`. Runs before any block is emitted, so the
    /// buffer is still unshifted (absolute == physical index).
    fn preextrapolate(&mut self) {
        self.preextrapolated = true;
        debug_assert_eq!(self.base, 0);
        let cw = self.bs.long_half();
        let cur = self.pcm_current as usize;
        if cur - cw > LPC_ORDER * 2 {
            for c in 0..self.ch {
                let pred = preextrapolate_channel(&self.pcm[c][cw..cur], cw);
                for (k, &p) in pred.iter().enumerate() {
                    self.pcm[c][cw - 1 - k] = p;
                }
            }
        }
    }

    /// Hand the rest of the input to libvorbis and signal EOS:
    /// `wrote(remaining)` (if any), then `wrote(0)` with its LPC
    /// post-extrapolation, each followed by a drain. Returns the Ogg bytes
    /// not yet handed out by [`Self::take_output`].
    pub(crate) fn finish(mut self) -> Vec<u8> {
        if self.output_returned {
            self.ogg.clear_output();
        }
        // A dangling half stereo frame can't be encoded; drop it (the
        // one-shot API has always ignored a trailing odd sample).
        self.partial = None;
        if self.unwritten > 0 {
            self.wrote(self.unwritten);
        }
        if !self.preextrapolated {
            self.preextrapolate();
        }
        let post_len = self.bs.long * 3;
        self.eofflag = self.pcm_current;
        let eof_rel = self.eofflag - self.total_shift();
        let phys_eof = (self.eofflag - self.base) as usize;
        for c in 0..self.ch {
            let buf = &mut self.pcm[c];
            debug_assert_eq!(buf.len(), phys_eof);
            buf.resize(phys_eof + post_len, 0.0);
            if eof_rel > (LPC_ORDER_POST * 2) as i64 {
                // n = min(eofflag, blocksizes[1]) samples of libvorbis's
                // buffer, which for short streams still reaches into the
                // pre-extrapolated prefix.
                let n = (eof_rel as usize).min(self.bs.long);
                let mut lpc = [0.0f32; LPC_ORDER_POST];
                lpc_from_data(&buf[phys_eof - n..phys_eof], &mut lpc, n, LPC_ORDER_POST);
                let (head, tail) = buf.split_at_mut(phys_eof);
                lpc_predict(
                    &lpc,
                    &head[phys_eof - LPC_ORDER_POST..],
                    LPC_ORDER_POST,
                    tail,
                    post_len,
                );
            }
            // else: libvorbis zero-fills (already zero from resize).
        }
        self.pcm_current += post_len as i64;
        self.drain();
        debug_assert_eq!(self.eofflag, -1, "EOS drain must emit the last block");
        #[cfg(feature = "parallel")]
        self.flush_jobs();
        self.ogg.into_bytes()
    }

    /// Ogg bytes completed since the previous call (pages only; the page
    /// being filled stays buffered).
    pub(crate) fn take_output(&mut self) -> &[u8] {
        self.output_returned = true;
        self.ogg.output()
    }

    /// Start of a `write` call: forget the bytes the previous call returned.
    pub(crate) fn begin_write(&mut self) {
        if self.output_returned {
            self.ogg.clear_output();
            self.output_returned = false;
        }
    }

    /// Frames currently held in the analysis buffer (diagnostic).
    pub(crate) fn buffered_frames(&self) -> usize {
        self.pcm[0].len()
    }

    #[cfg(feature = "parallel")]
    pub(crate) fn set_batch(&mut self, blocks: usize) {
        self.batch = blocks.max(1);
    }

    // -----------------------------------------------------------------
    // libvorbis state machine
    // -----------------------------------------------------------------

    /// libvorbis keeps `centerW == blocksizes[1]/2` in its relative buffer;
    /// this is how far that buffer has been shifted.
    #[inline]
    fn total_shift(&self) -> i64 {
        self.center_w - self.bs.long_half() as i64
    }

    fn drain(&mut self) {
        while let Some(block) = self.blockout() {
            #[cfg(test)]
            self.w_log.push(block.w);
            self.analyze(&block);
            self.compact();
        }
    }

    #[inline]
    fn mark(&self, i: i64) -> bool {
        let k = i - self.marks_base;
        k >= 0 && (k as usize) < self.marks.len() && self.marks[k as usize]
    }

    #[inline]
    fn set_mark(&mut self, i: i64, v: bool) {
        let k = (i - self.marks_base) as usize;
        if k >= self.marks.len() {
            self.marks.resize(k + 1, false);
        }
        self.marks[k] = v;
    }

    /// `_ve_envelope_search`: extend the marks to the visible PCM, then walk
    /// the cursor. Returns 1 (next block long), 0 (short) or -1 (not enough
    /// data).
    fn envelope_search(&mut self) -> i32 {
        let setup = Arc::clone(&self.setup);
        let gi = &setup.cfg.psy_global;
        let first = self.ve_current / SEARCHSTEP;
        let last = self.pcm_current / SEARCHSTEP - VE_WIN as i64;
        for j in first..last {
            let off = (SEARCHSTEP * j - self.base) as usize;
            let ret = self.env.step(gi, &self.pcm, off);
            self.set_mark(j + VE_POST as i64, false);
            if ret & 1 != 0 {
                self.set_mark(j, true);
                self.set_mark(j + 1, true);
            }
            if ret & 2 != 0 {
                self.set_mark(j, true);
                // C tests the relative index; j > 0 relative only differs
                // from absolute before the first shift.
                if j > 0 {
                    self.set_mark(j - 1, true);
                }
            }
        }
        self.ve_current = last * SEARCHSTEP;

        let bs0 = self.bs.short as i64;
        let bs1 = self.bs.long as i64;
        let center_w = self.center_w;
        let test_w = center_w + self.bs.get(self.w) as i64 / 4 + bs1 / 2 + bs0 / 4;
        let mut j = self.cursor;
        while j < self.ve_current - SEARCHSTEP {
            if j >= test_w {
                return 1;
            }
            self.cursor = j;
            if self.mark(j / SEARCHSTEP) && j > center_w {
                self.curmark = j;
                return if j >= test_w { 1 } else { 0 };
            }
            j += SEARCHSTEP;
        }
        -1
    }

    /// `_ve_envelope_mark`: is there a transient near this (short) block?
    fn envelope_mark(&self, nw: bool) -> bool {
        let bs = self.bs;
        let q = |long: bool| bs.get(long) as i64 / 4;
        let mut begin_w = self.center_w - q(self.w);
        let mut end_w = self.center_w + q(self.w);
        if self.w {
            begin_w -= q(self.lw);
            end_w += q(nw);
        } else {
            begin_w -= q(false);
            end_w += q(false);
        }
        // ve->curmark is only shifted while non-negative; once it drops
        // below the buffer it can never fall inside [beginW, endW) again,
        // which also holds for its absolute value.
        if self.curmark >= begin_w && self.curmark < end_w {
            return true;
        }
        (begin_w / SEARCHSTEP..end_w / SEARCHSTEP).any(|i| self.mark(i))
    }

    /// `vorbis_analysis_blockout`: decide the next block, if there is
    /// enough data, and advance the state past it.
    fn blockout(&mut self) -> Option<Block> {
        if !self.preextrapolated || self.eofflag == -1 {
            return None;
        }
        let bp = self.envelope_search();
        let nw = if bp == -1 {
            if self.eofflag == 0 {
                return None;
            }
            false
        } else {
            bp == 1
        };
        let bsw = self.bs.get(self.w) as i64;
        let bsn = self.bs.get(nw) as i64;
        let center_next = self.center_w + bsw / 4 + bsn / 4;
        let blockbound = center_next + bsn / 2;
        if self.pcm_current < blockbound {
            return None;
        }

        let blocktype = if self.w {
            if !self.lw || !nw {
                BlockType::Transition
            } else {
                BlockType::Long
            }
        } else if self.envelope_mark(nw) {
            BlockType::Impulse
        } else {
            BlockType::Padding
        };
        let mut block = Block {
            center: self.center_w,
            lw: self.lw,
            w: self.w,
            nw,
            blocktype,
            granulepos: self.granulepos as u64,
            eos: false,
        };

        if self.eofflag != 0 && self.center_w >= self.eofflag {
            self.eofflag = -1;
            block.eos = true;
            return Some(block);
        }

        // Advance (movementW = centerNext - blocksizes[1]/2, relative).
        let movement = center_next - self.center_w;
        self.lw = self.w;
        self.w = nw;
        self.center_w = center_next;
        if self.eofflag != 0 {
            if self.eofflag - self.total_shift() <= 0 {
                self.eofflag = -1;
            }
            if self.center_w >= self.eofflag {
                // Do not add padding to the end of the stream.
                self.granulepos += movement - (self.center_w - self.eofflag);
            } else {
                self.granulepos += movement;
            }
        } else {
            self.granulepos += movement;
        }
        Some(block)
    }

    /// Drop PCM and marks that no future block, envelope step or
    /// extrapolation can reach (everything below libvorbis's relative 0).
    fn compact(&mut self) {
        let keep = self.total_shift();
        if keep - self.base >= COMPACT_FRAMES {
            let d = (keep - self.base) as usize;
            for buf in &mut self.pcm {
                buf.drain(..d);
            }
            self.base = keep;
        }
        let mkeep = (self.cursor.min(keep) / SEARCHSTEP - 2).max(0);
        if mkeep - self.marks_base >= COMPACT_FRAMES / SEARCHSTEP {
            let d = ((mkeep - self.marks_base) as usize).min(self.marks.len());
            self.marks.drain(..d);
            self.marks_base = mkeep;
        }
    }

    // -----------------------------------------------------------------
    // Analysis (vorbis_analysis -> mapping0_forward) and packet output
    // -----------------------------------------------------------------

    /// Copy `vb->pcm` (`[centerW - n/2, centerW + n/2)`) into `dst` and
    /// apply `_vorbis_apply_window`. Samples past the buffered data are only
    /// ever in the zeroed tail of the window.
    fn window_block(&self, block: &Block, c: usize, dst: &mut [f32]) {
        let n = dst.len() as i64;
        let begin = (block.center - n / 2 - self.base) as usize;
        let src = &self.pcm[c];
        let avail = src.len().saturating_sub(begin).min(dst.len());
        dst[..avail].copy_from_slice(&src[begin..begin + avail]);
        dst[avail..].fill(0.0);
        apply_window_bs(dst, self.bs, block.lw, block.w, block.nw);
    }

    fn block_mode(&self, block: &Block) -> BlockMode {
        BlockMode {
            mode_number: if block.w {
                self.long_mode
            } else {
                self.short_mode
            },
            modebits: self.setup.setup.modebits,
            is_long: block.w,
            prev_window: block.w && block.lw,
            next_window: block.w && block.nw,
        }
    }

    fn dump_first_block(&mut self, pcm0: &[f32], block: &Block) {
        if !self.first_block_done {
            self.first_block_done = true;
            if !block.w
                && crate::debug_dump::dump_enabled()
                && crate::debug_dump::try_claim_first_short_block()
            {
                std::fs::create_dir_all("/tmp/lewtoff-debug").ok();
                crate::debug_dump::write_f32_bin("/tmp/lewtoff-debug/r_windowed.bin", pcm0);
            }
        }
    }

    #[cfg(not(feature = "parallel"))]
    fn analyze(&mut self, block: &Block) {
        let mut win = std::mem::take(&mut self.win[block.w as usize]);
        for (c, dst) in win.iter_mut().enumerate() {
            self.window_block(block, c, dst);
        }
        self.dump_first_block(&win[0], block);
        let mode = self.block_mode(block);
        let setup = &self.setup.setup;
        let mapping = &setup.mappings[setup.modes[mode.mode_number].mapping];
        let mut w = BitWriter::new();
        mapping0_forward(
            &win,
            &self.psy_looks[block.blocktype as usize],
            &self.setup.cfg.psy_global,
            &mut self.ampmax,
            &mut self.floor_states,
            &setup.residue_types,
            &setup.residue_setups,
            &setup.residue_looks,
            mapping,
            &mode,
            &setup.books,
            &mut self.scratch,
            &mut w,
        );
        self.win[block.w as usize] = win;
        self.ogg
            .write_packet(&w.into_bytes(), block.granulepos, block.eos, false);
    }

    #[cfg(feature = "parallel")]
    fn analyze(&mut self, block: &Block) {
        let n = self.bs.get(block.w);
        let pcm: Vec<Vec<f32>> = (0..self.ch)
            .map(|c| {
                let mut v = vec![0.0f32; n];
                self.window_block(block, c, &mut v);
                v
            })
            .collect();
        self.dump_first_block(&pcm[0], block);
        self.jobs.push(BlockJob {
            pcm,
            mode: self.block_mode(block),
            blocktype: block.blocktype,
            granulepos: block.granulepos,
            eos: block.eos,
        });
        if self.jobs.len() >= self.batch {
            self.flush_jobs();
        }
    }

    /// Analyze the pending batch in parallel. Only `ampmax` carries state
    /// from block to block in `mapping0_forward`; it is recovered exactly by
    /// a cheap sequential chain over each block's local spectral peak, so
    /// batch boundaries cannot change the bytes.
    #[cfg(feature = "parallel")]
    fn flush_jobs(&mut self) {
        use rayon::prelude::*;
        if self.jobs.is_empty() {
            return;
        }
        let jobs = std::mem::take(&mut self.jobs);
        let gi = &self.setup.cfg.psy_global;
        let rate = self.psy_looks[0].rate;
        // Pass A (parallel): max over channels of each block's clamped
        // local ampmax.
        let local_max: Vec<f32> = jobs
            .par_iter()
            .map(|job| crate::mapping0::compute_block_local_ampmax_max(&job.pcm))
            .collect();
        // Pass B (sequential): the ampmax each block starts from in serial
        // order (decay with this block's n2, then max with its local peak).
        let mut input_ampmax = Vec::with_capacity(jobs.len());
        let mut prev = self.ampmax;
        for (job, &m) in jobs.iter().zip(&local_max) {
            input_ampmax.push(prev);
            let n2 = job.pcm[0].len() / 2;
            prev = crate::psy::vp_ampmax_decay(prev, gi, n2, rate).max(m);
        }
        self.ampmax = prev;
        // Pass C (parallel): full analysis per block.
        let setup = &self.setup.setup;
        let psy_looks = &self.psy_looks;
        let floor_seed = &self.floor_seed;
        let ch = self.ch;
        let packets: Vec<Vec<u8>> = jobs
            .par_iter()
            .zip(input_ampmax.par_iter())
            .map_init(
                || (MappingScratch::new(ch), floor_seed.clone()),
                |(scratch, floors), (job, &amp)| {
                    let mapping = &setup.mappings[setup.modes[job.mode.mode_number].mapping];
                    let mut ampmax = amp;
                    let mut w = BitWriter::new();
                    mapping0_forward(
                        &job.pcm,
                        &psy_looks[job.blocktype as usize],
                        gi,
                        &mut ampmax,
                        floors,
                        &setup.residue_types,
                        &setup.residue_setups,
                        &setup.residue_looks,
                        mapping,
                        &job.mode,
                        &setup.books,
                        scratch,
                        &mut w,
                    );
                    w.into_bytes()
                },
            )
            .collect();
        for (job, packet) in jobs.iter().zip(&packets) {
            self.ogg
                .write_packet(packet, job.granulepos, job.eos, false);
        }
    }
}

/// libvorbis-driver i16 scaling: `sample / 32768.0f`.
#[inline]
pub(crate) fn i16_to_f32(s: i16) -> f32 {
    s as f32 / 32768.0
}

/// `write_f32` sample mapping: finite values pass through untouched;
/// NaN and ±infinity become 0.0 (see `Encoder::write_f32`).
#[inline]
pub(crate) fn f32_sample(s: f32) -> f32 {
    if s.is_finite() { s } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_shot(pcm: &[i16], rate: SampleRate, channels: Channels, q: f32) -> Vec<u8> {
        let mut core = EncoderCore::new(rate, channels, q, 7, b"v", &[]);
        core.feed(pcm, i16_to_f32);
        core.finish()
    }

    /// SplitMix64 for reproducible test signals.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        fn unit(&mut self) -> f32 {
            (self.next() >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
        }
    }

    /// Mixed test signal: tone bed, noise, sharp bursts and silent gaps,
    /// chosen per 2048-frame segment so block switching gets exercised.
    fn signal(rng: &mut Rng, frames: usize, ch: usize) -> Vec<i16> {
        let mut out = Vec::with_capacity(frames * ch);
        let mut kind = 0;
        let mut amp = 0.0f32;
        let mut phase = 0.0f32;
        let freq = 0.01 + 0.2 * (rng.below(1000) as f32 / 1000.0);
        for i in 0..frames {
            if i % 2048 == 0 {
                kind = rng.below(5);
                amp = [0.0, 0.3, 0.05, 0.9, 0.001][rng.below(5)];
            }
            phase += freq;
            for c in 0..ch {
                let v = match kind {
                    0 => 0.0,
                    1 => (phase + c as f32).sin() * amp,
                    2 => rng.unit() * amp,
                    3 => {
                        if i % 2048 < 64 {
                            rng.unit() * amp
                        } else {
                            (phase * 3.0).sin() * 0.01
                        }
                    }
                    _ => rng.unit() * amp + (phase * 0.5).sin() * 0.2,
                };
                out.push((v * 32767.0) as i16);
            }
        }
        out
    }

    fn streaming(
        pcm: &[i16],
        rate: SampleRate,
        channels: Channels,
        q: f32,
        chunks: &mut Rng,
    ) -> Vec<u8> {
        let mut core = EncoderCore::new(rate, channels, q, 7, b"v", &[]);
        let mut out = Vec::new();
        let mut rest = pcm;
        while !rest.is_empty() {
            let n = match chunks.below(4) {
                0 => 1,
                1 => chunks.below(130),
                2 => chunks.below(5000),
                _ => rest.len(),
            }
            .min(rest.len());
            core.begin_write();
            core.feed(&rest[..n], i16_to_f32);
            out.extend_from_slice(core.take_output());
            rest = &rest[n..];
        }
        out.extend_from_slice(&core.finish());
        out
    }

    /// A full-scale stereo ramp opens with two short blocks and switches
    /// to short blocks mid-stream (envelope marks on the wrap-arounds).
    #[test]
    fn ramp_block_pattern() {
        let pcm: Vec<i16> = (0..44100 * 2)
            .map(|i: i32| (i % 65536 - 32768) as i16)
            .collect();
        let mut core = EncoderCore::new(SampleRate::Hz44100, Channels::Stereo, 0.5, 1, b"v", &[]);
        core.feed(&pcm, i16_to_f32);
        let log = core.w_log.clone();
        core.finish();
        assert!(
            !log[0] && !log[1] && log[2],
            "pattern starts {:?}",
            &log[..4]
        );
        assert!(
            log.iter().filter(|&&w| !w).count() > 4,
            "no mid-stream shorts"
        );
    }

    /// Chunked feeding (1-sample, odd, large) equals one-shot.
    #[test]
    fn chunked_matches_one_shot() {
        let mut rng = Rng(0x5EED);
        for (k, &frames) in [0usize, 1, 65, 2112, 2113, 4161, 6145, 30_000, 100_000]
            .iter()
            .enumerate()
        {
            let q = [0.5f32, 0.2, -0.05][k % 3];
            let channels = if k % 2 == 0 {
                Channels::Stereo
            } else {
                Channels::Mono
            };
            let ch = if channels == Channels::Stereo { 2 } else { 1 };
            let pcm = signal(&mut rng, frames, ch);
            let oneshot = one_shot(&pcm, SampleRate::Hz44100, channels, q);
            let chunked = streaming(&pcm, SampleRate::Hz44100, channels, q, &mut rng);
            assert!(chunked == oneshot, "frames={frames} q={q} {channels:?}");
        }
    }

    /// The analysis buffer stays bounded on long input.
    #[test]
    fn buffer_is_bounded() {
        for q in [0.5f32, -0.1] {
            let mut core = EncoderCore::new(SampleRate::Hz44100, Channels::Stereo, q, 1, b"v", &[]);
            let mut rng = Rng(3);
            let mut max = 0;
            for _ in 0..200 {
                let pcm = signal(&mut rng, 4410, 2);
                core.feed(&pcm, i16_to_f32);
                max = max.max(core.buffered_frames());
                assert!(core.marks.len() < 400, "marks grew to {}", core.marks.len());
            }
            let limit = COMPACT_FRAMES as usize + 3 * core.bs.long;
            assert!(max <= limit, "q={q}: buffered {max} > {limit}");
            core.finish();
        }
    }
}
