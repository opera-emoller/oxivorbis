# Streaming `Encoder` design

**Status: implemented.** This file is kept as the design record. It was written before
the code, from an audit of the old whole-input `encode.rs` against the vendored libvorbis.
The implementation lives in:

- `src/encode.rs`: `EncoderCore`, the streaming state machine. It covers priming,
  `wrote`, `blockout`, the envelope search and mark, compaction, finish-time
  post-extrapolation and the parallel pipeline.
- `src/envelope.rs`: `EnvelopeLookup::step`, the incremental `_ve_amp`.
- `src/lib.rs`: the public `Encoder` (`new` / `write` / `write_f32` / `finish`). The one-shot
  `encode` / `encode_f32` are thin wrappers over the same core.
- `tests/streaming.rs`, `tests/parity.rs` (`oracle_parity_short_input`) and `tests/parity_f32.rs`.

Where the shipped code differs from the plan below, a **Now:** note says so. Block-size
constants come from the quality setup (256/2048 for q0 and up, 512/4096 for q-1).

## Parity contract

The output must match libvorbis 1.3.7 fed in **64-frame `vorbis_analysis_wrote` chunks with full
`blockout` drains after each call**, which is what `tools/oracle-encoder/main.c` does. It must
match for *any* chunking the user passes to `write`.

- The only quantity that depends on chunk size is the pre-extrapolation LPC training length:
  `n_lpc = min((bs1/64 + 1) * 64, total)`. That is 2112 for 256/2048 blocks and 4160 for 512/4096.
  oggenc feeds 1024-frame reads, so it would give 3072. That is why oggenc parity only holds for silence.
- Envelope marks, block sizes, eofflag, post-extrapolation `n_train`, granules and pages do not
  depend on chunk size, as long as the encoder drains greedily.
- Rule: start pre-extrapolation once `n_lpc` frames are buffered. Train on exactly the first `n_lpc`
  frames, even if one `write` delivered more. If `finish()` comes before that, train on everything fed.

## Known one-shot bug to fix first

**Now:** fixed. `oracle_parity_short_input` checks every length from 0 to 130 frames, every
7th length up to 2100, and 2047/2048/2049, all byte-identical. That includes empty input.

Inputs shorter than 2048 frames diverge from libvorbis. libvorbis trains the post-extrapolation LPC
on `pcm[eofflag-n .. eofflag]` with `n = min(eofflag, bs1)`. That is its own buffer, which for short
streams still contains the 1024 pre-extrapolated samples. `postextrapolate_channel_with_n` is given
audio only and clamps `n` to the audio length.

- Fix: prepend the pre-extrapolated samples. This was verified for every length from 1 to 8000.
- Empty input is still 2 bytes off and is unexplained.

## Whole-input dependencies in the old `encode_with_serial_and_meta`

**Now:** every row in the table below has been replaced by its streaming state. The
fixed-point loop, `compute_marks`, `simulate_eofflag` and the global W pattern were deleted.

| Today | What libvorbis actually needs | Streaming state |
|---|---|---|
| Full-PCM f32 copy | Buffer from `centerW - bs1/2` up to `pcm_current` | Sliding per-channel buffer, with compaction |
| Pre-extrapolation from `audio[..2112]` (done twice) | First `n_lpc` frames | Priming stage |
| Fixed-point loop between `n_train` and the marks | No circularity: pre-EOS marks use real audio only | None |
| Global `compute_marks` | Mark j reads env `[64j, 64j+128)`, final once mark j+1 is computed | `EnvelopeLookup` plus a marks deque |
| `simulate_eofflag` | `eofflag = (1024 + fed) - (center_w - bs1/2)` at `finish` | `center_w` (absolute) |
| Global W pattern | One decision of lookahead (nW before emitting W); `blockbound = C + bs[W]/4 + 3*bs[nW]/4` | `w`, `lw`, `center_w`, `cursor`, `curmark` |
| Post-extrapolation on the global tail | At `finish`: last `min(eofflag, bs1)` buffer samples, predicting `3*bs1` | Appended to the buffer |
| Granule clamp using total length | Only after EOS | `granule: u64` |
| ampmax | Previous value only | `ampmax: f32` |
| Parallel: every block held in memory | Nothing | Bounded batch |

Worst-case lookahead at Q5 is about 2432 frames past `centerW`. It doubles at q-1.

## Struct sketch

**Now:** the real struct is `EncoderCore` in `src/encode.rs`. It is close to this sketch,
with these differences:

- The split stereo frame is held as `partial: Option<f32>`, after conversion, so i16 and f32
  writes can be mixed.
- Frames are passed to `wrote` in 64-frame chunks, with an `unwritten` remainder.
- The buffers are compacted once 4096 frames below the live window have built up.
- The Ogg writer's output buffer is lent to the caller from `write`.

```rust
pub struct Encoder {
    cfg: Box<EncoderSetup>,      // setup, psy looks, gi, bs, rate, ch
    stage: Stage,                // Priming | Running | Done
    pcm: Vec<Vec<f32>>, base: i64, fed: u64,
    partial: [i16; 1], partial_len: u8,   // split stereo frame
    env: EnvelopeLookup, marks: VecDeque<bool>, marks_base: i64, env_next_j: i64,
    cursor: i64, curmark: i64, lw: u8, w: u8, center_w: i64, eof_abs: Option<i64>,
    ampmax: f32, floor_states: Vec<Floor1State>, scratch: MappingScratch,
    granule: u64, ogg: OggStreamWriter,  // over Vec<u8>, drained with mem::take
    #[cfg(feature = "parallel")] pending: Vec<BlockJob>,
}
```

- `write` loop:
  1. Deinterleave and scale by 1/32768 (`write`), or pass the floats through (`write_f32`).
  2. Priming leads to Running.
  3. Drain: marks for every `j < end/64 - 4`; a literal `_ve_envelope_search` cursor walk;
     the `blockbound` check; emit block W from slices of `env[C - bs[W]/2 .. C + bs[W]/2)`
     (this replaces `WindowingBuffer` and `set_prestream`); advance; drop data below `center_w - bs1/2`.
- `finish`:
  1. Pre-extrapolate if still priming.
  2. Set eofflag and post-extrapolate.
  3. Drain with EOS, which sets nW=0 on -1.
  4. Last packet granule is `min(granule, fed)`.
  5. Flush the parallel batch, then the Ogg stream.
- One-shot `encode` becomes a wrapper: `new`, then `write(all)`, then `finish`. Delete the fixed-point
  loop and the global passes, keeping them only as test cross-checks.
- Parallel: only `ampmax` is order-dependent in `mapping0_forward`.
  - Batch roughly 8 × threads blocks. **Now:** 32 × threads. The batches are pipelined:
    the Rayon pool runs pass C of the previous batch and pass A of the current one while the
    calling thread windows the next batch. About four batches are alive at once.
  - Pass A (parallel): local ampmax per block.
  - Pass B (sequential): chain ampmax.
  - Pass C (parallel): analysis.
  - The chain is exact, so batch boundaries can't change bytes.

## API

```rust
impl Encoder {
    pub fn new(rate: SampleRate, channels: Channels, quality: f32) -> Self;
    pub fn write(&mut self, interleaved: &[i16]) -> &[u8]; // pages completed by this call
    pub fn finish(self) -> Vec<u8>;
}
```

- `write` borrows an internal buffer: no allocation per call and no `Result`.
- A `W: Write` adapter can come later.
- WASM: `random_serial()` uses `SystemTime::now()`, which panics on wasm32-unknown-unknown.
  It needs a fallback plus a public `with_serial`.

**Now:** the API is `Encoder::new(rate, channels, &Options)`, plus `write`, `write_f32` and
`finish`. `Options` holds the quality (a typed `Quality` on oggenc's `-q` scale), the serial,
the vendor and the comments, with `with_*` setters. There is no random serial: the default is
the constant `DEFAULT_SERIAL`, so default output is deterministic and wasm32 needs no clock.
There is no `Write` adapter yet.

## Tests

- **Chunking invariance:** seeded random chunkings, including 1-frame chunks, 2111 + rest, and odd
  sample counts that split stereo frames. Lengths: 0, 1, 63, 64, 65, 2047, 2048, 2049, 2111..2113, around
  `3*bs1`, and 10 s. Signals: silence, sine, ramp, transients, noise. Cover mono/stereo, 44.1/48k, and Q5/q-1.
  The concatenated output must equal `encode()`.
- **Streaming and memory:** a 60 s input emits pages before `finish`, and buffered frames stay
  bounded (about 12k) over a 10-minute input.
- **Parallel batching:** batch sizes 1, 3 and 256 give the same bytes.
- **Migration cross-check:** compare the new scheduler with `full_w_pattern_with_curmark` and
  `simulate_eofflag` across the corpus, then delete the check.

## Plan (about 5-6 days)

**Now:** done, and the old global scheduler code has been deleted. Parity is guarded by the
oracle test suite, the corpus sweep and `tools/parity-fuzz` (see `CONTRIBUTING.md`).

0. Short-input fix plus oracle probe for lengths 1..2047 (0.5 d).
1. Streaming `EnvelopeLookup::step` (0.5 d).
2. Block scheduler, a literal port of `_ve_envelope_search`, `blockbound` and `_ve_envelope_mark` (1-1.5 d). **Highest risk.**
3. Sliding buffer, priming, finish-time post-extrapolation, slice windowing (1 d).
4. `Encoder` serial path, Ogg over `Vec`, one-shot as wrapper (1 d).
5. Parallel batching (0.5 d).
6. Public API, WASM serial, README (0.5 d).
7. Tests (0.5-1 d).
