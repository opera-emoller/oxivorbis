# lewtoff

A pure-Rust Ogg Vorbis **encoder** whose audio packets are bit-identical
to libvorbis 1.3.7 (VBR mode, oggenc qualities -1–10, fractional
included) for a constrained input space.
Named after [lewton](https://github.com/RustAudio/lewton) — the pure-Rust
Vorbis decoder. lewtoff is its encoder counterpart.

The crate forbids `unsafe` and pulls in a single runtime dependency
(the `ogg` crate, for page framing and CRC). An optional `parallel`
feature adds Rayon-based block-level fan-out — see
[Parallel encoding](#parallel-encoding) below.

## Parity contract

- **Audio:** every audio packet, granule position and page boundary is
  identical to libvorbis 1.3.7 (built without FMA contraction) fed the
  same samples through `vorbis_analysis_buffer` in 64-frame
  `vorbis_analysis_wrote` chunks — what `tools/oracle-encoder` does. The
  chunking lewtoff's caller uses does not matter.
- **Headers:** identical given the same stream serial, vendor string and
  comments. lewtoff's defaults are its own (serial
  `lewtoff::DEFAULT_SERIAL`, vendor `"lewtoff <version>"`, no comments),
  so a default encode is *not* byte-identical to `oggenc -q X`, whose
  serial is random and whose vendor/comments differ. Set
  `Options::{serial, vendor, comments}` to reproduce another encoder's
  headers.
- oggenc itself feeds 1024-frame chunks, which changes libvorbis's start-
  of-stream LPC pre-extrapolation, so oggenc's audio matches lewtoff only
  for inputs where that does not matter (e.g. silence).

## Status

All synthetic parity tests (silence, sine 440, ramps, noise+tones,
transients; mono+stereo at 44.1k and 48k; base qualities -0.1–1.0, i.e.
oggenc `-q -1` … `-q 10`; input lengths from 0 frames up; i16 and f32
input) match the libvorbis 1.3.7 oracle byte-for-byte. A real-audio
corpus sweep (`tests/corpus_sweep.rs::corpus_parity_sweep`) walks any
`<repo_root>/corpus/` directory the contributor has symlinked in and
asserts the same parity for every file recursively; the most recent
Q5 sweep over a 5954-file corpus passed 100% byte-identical, and the 14
macOS system sounds pass at q-1 … q10 in all four rate/channel configs.
`tools/parity-fuzz` (a differential fuzzer against the oracle, streaming
each case through `Encoder` in random chunk sizes) runs clean over tens
of thousands of generated signals per quality; its minimized failures are
kept as regression vectors in `tests/vectors/fuzz/`
(`oracle_parity_fuzz_repros`).

The encoder is a literal port of libvorbis's streaming analysis state
machine (`vorbis_analysis_wrote` / `vorbis_analysis_blockout` /
`_ve_envelope_search`), driven internally in the same 64-frame chunks as
the oracle, so block sizes, end-of-stream handling and the LPC pre/post-
extrapolation fall out without whole-input passes. See
`docs/streaming-design.md`.

## Use

One-shot:

```rust
use lewtoff::{Channels, Options, Quality, SampleRate};

let pcm: &[i16] = /* interleaved 16-bit PCM */;

// Quality 5, deterministic default serial, vendor "lewtoff <version>".
let ogg = lewtoff::encode(pcm, SampleRate::Hz44100, Channels::Stereo, &Options::default());

// Any quality on oggenc's -q scale (fractional allowed), plus tags.
let opts = Options::default()
    .with_quality(Quality::new(7.5).expect("-1..=10"))
    .with_serial(0x1234_5678)
    .with_comment("TITLE", "Example")
    .with_comment("ARTIST", "Someone");
let ogg = lewtoff::encode(pcm, SampleRate::Hz44100, Channels::Stereo, &opts);

// f32 input, nominally -1.0..=1.0.
let floats: &[f32] = /* interleaved f32 PCM */;
let ogg = lewtoff::encode_f32(floats, SampleRate::Hz48000, Channels::Mono, &opts);
```

Streaming, in chunks of any size (including odd sample counts that split
stereo frames), with memory bounded independent of input length:

```rust
use lewtoff::{Channels, Encoder, Options, SampleRate};
use std::io::Write;

let mut enc = Encoder::new(SampleRate::Hz44100, Channels::Stereo, &Options::default());
let mut out = std::fs::File::create("out.ogg")?;
for chunk in pcm_source() {          // e.g. &[i16] blocks from a decoder
    out.write_all(enc.write(chunk))?; // the Ogg pages completed so far
}
// enc.write_f32(&floats) works the same way, and the two can be mixed.
out.write_all(&enc.finish())?;
```

The concatenated output of `write`/`write_f32` and `finish` is exactly
the bytes the one-shot functions return for the same samples and
options. Each `write` returns a slice of an internal buffer (no
allocation per call), valid until the next call.

There is no `Result` anywhere: rates, channel layouts and qualities are
typed (`SampleRate`, `Channels`, `Quality::new` returns `None` outside
-1..=10 or for NaN) and no input makes the encoder panic.

`Quality` is converted to libvorbis's `base_quality` exactly as oggenc
1.4.3 does — `(float)((double)(float)q * 0.1)` — so e.g. `9.344` picks
the same float (and the same ID-header nominal bitrate) as
`oggenc -q 9.344`, not the nearest float to 0.9344.

## Supported input

| | |
|---|---|
| Sample format | Interleaved `i16` (scaled by 1/32768) or `f32` (passed through unscaled) |
| Sample rate   | 44100 Hz **or** 48000 Hz |
| Channels      | Mono **or** Stereo |
| Quality       | -1.0 – 10.0 on oggenc's `-q` scale (fractional ok); default 5 |

- **f32:** values outside ±1.0, subnormals and `-0.0` are encoded as
  libvorbis would (parity is tested up to ±10⁴; at absurd magnitudes
  libvorbis's own float-to-int conversions are undefined, but lewtoff
  still produces a valid stream). NaN and ±infinity, which make libvorbis
  emit a corrupt stream, are encoded as `0.0`.
- **Odd sample counts in stereo:** `Encoder::write` carries an unpaired
  trailing sample over to the next call; `finish` and the one-shot
  functions drop a final unpaired sample.

Qualities -1 … <0 select libvorbis's q-1 template (512/4096-sample
blocks instead of 256/2048) and are byte-identical too.

## Parallel encoding

For long inputs (minutes of audio), enable the `parallel` feature to fan
per-block analysis (MDCT, FFT, psy, floor, residue) across the global
Rayon thread pool. Envelope analysis, bit-packing and Ogg page emission
stay serial; the output is byte-identical to the serial path. Blocks are
analyzed in bounded, pipelined batches (32 × the Rayon thread count; the
pool analyzes earlier batches while the calling thread windows the next),
so streaming memory stays bounded — a few MB — and `write` returns pages
a few batches (several seconds of audio) later than the serial build.

```toml
[dependencies]
lewtoff = { version = "0.1", features = ["parallel"] }
```

The API is the same. The feature is off by default to keep the
default build lean (single `ogg` runtime dep) and WASM-friendly — Rayon
needs `SharedArrayBuffer` and special host headers in browsers, so
WASM consumers should leave it off.

Speedup scales with audio length. On an 8-core M-series Mac, encoding
a 261s MP3:

| Path     | Wall  | User CPU | vs oggenc |
|----------|-------|----------|-----------|
| serial   | 1.45s | 1.42s    | ~0.83×    |
| parallel | 0.30s | 1.97s    | ~4.0×     |

Sub-second inputs see no speedup (too few blocks to feed all cores) but
no overhead penalty either. If you're already doing outer parallelism
across many files (e.g. a game-asset pipeline), leave `parallel` off
— Rayon would compose without oversubscribing, but the per-batch
job bookkeeping adds a little overhead you don't need.

## How parity is enforced

`tests/parity.rs` (behind the `oracle` feature) byte-diffs lewtoff's
output against two reference encoders:

- **`oracle_parity_*`** — the authoritative tests. They shell out to
  `tools/oracle-encoder/oracle-encoder`, a small C program that
  statically links the libvorbis 1.3.7 sources vendored at
  `tools/debug-libvorbis-dump/vendored-libvorbis/`, compiled with
  `-O0 -ffp-contract=off -std=gnu99` so the bytes are deterministic
  across hosts. Covers silence, a 440 Hz sine, and a full-range ramp.
- **`parity_*`** — the same comparison against the system `oggenc`
  (vorbis-tools), i.e. whatever libvorbis your package manager ships.
  Only the silence cases are enabled: packaged libvorbis builds are
  optimized and may use FMA, so tone-rich input (sine, ramp) can drift
  from the `-O0` reference. Those cases are `#[ignore]`d with a pointer
  to their `oracle_parity_*` counterparts.

Install the prerequisites and build the oracle once after cloning:

```sh
sudo apt install libogg-dev vorbis-tools    # or: brew install libogg vorbis-tools
./tools/oracle-encoder/build.sh
```

Then:

```sh
cargo nextest run --features oracle parity_    # matches both parity_* and oracle_parity_*
just parity                                    # same, for serial AND parallel paths
```

For the `corpus_parity_sweep` test, symlink your audio corpus at
`<repo_root>/corpus/` and run

```sh
cargo nextest run --features oracle --no-tests=warn corpus_parity_sweep -- --include-ignored
```

The directory is gitignored; the sweep walks it recursively and accepts
`.wav`, `.mp3`, `.ogg`, `.flac`, `.m4a`, `.aif`, and `.aiff`. Files are
decoded via `ffmpeg` (so it must be on `PATH`) to s16le 44.1kHz stereo,
then encoded by both lewtoff and the in-tree oracle-encoder and compared
byte-for-byte. Set
`CORPUS_LIMIT=N` to test only the first N files for a quick smoke run.

## Tests

```sh
cargo nextest run                              # unit + integration
cargo nextest run --features oracle parity_    # parity vs oracle-encoder + oggenc
just parity                                    # parity for serial AND parallel paths
```

Quality-parameterized parity lives in `tests/parity.rs` as the
`oracle_parity_q::<quality>::<config>::<signal>` matrix: one
base_quality per libvorbis template interval (0.05 … 0.95, plus 0.0,
0.5 and 1.0), the q-1 range (-0.1, -0.08, -0.05, -0.01, -0.0000001) ×
44.1/48 kHz × mono/stereo × silence / sine / ramp / noise+tone / 10 s
transients. `oracle_parity_short_input` sweeps input lengths 0..2100
frames (around the 2048-sample long block) at two qualities.
`parity_oggenc_quality_conversion` pins the oggenc `-q` → base_quality
float conversion against the system `oggenc`. The corpus sweep takes a
libvorbis base_quality via `CORPUS_QUALITY` (default 0.5), e.g.
`just corpus 100 "" 0.3`.

`tests/parity_f32.rs` checks `f32` input against the oracle's f32 mode
(`oracle-encoder <rate> <ch> <base_quality> f32`, which reads f32 LE and
passes the floats to `vorbis_analysis_buffer` unchanged): several
qualities and configs, values beyond ±1.0, subnormals, `-0.0`, NaN/inf
(encoded as 0.0) and odd-sized chunked writes. `tests/streaming.rs`
checks that any chunking of `write` / `write_f32` gives the one-shot
bytes, that pages are emitted before `finish`, that the analysis buffer
stays bounded over 10 minutes, and (with `parallel`) that the batch size
never changes the output. Its long-input tests are `#[ignore]`d in debug
builds; run them with `--release`.

The integration tests reach a few unstable internals (header writers,
exact-`base_quality` entry points) through the undocumented `__internal`
cargo feature, which the crate enables for its own tests via a
self dev-dependency, so no extra flags are needed.

CI (`.github/workflows/ci.yml`) installs `vorbis-tools`, builds the
oracle from the vendored sources, and runs both passes on Ubuntu, plus a `wasm32-unknown-unknown`
build to lock in target portability.

## Why this works

libvorbis's encoder is f32-throughout and does not use FMA when built
with `-ffp-contract=off`. Rust's `f32` arithmetic on every tier-1
target produces the same IEEE 754 results as that C build, provided no
operation is contracted into FMA. Transcendentals (`sin`/`cos`/`log`/
`exp`) are precomputed at build time — at runtime the encoder only
does table lookups and arithmetic, so libm differences across platforms
don't matter.

The detailed precision pitfalls hit during the port — f32-vs-f64
promotion, `rint` vs `.round()`, the IEEE_FLOAT32 `todB` inline, and so
on — live in the commit log.

## License

Apache-2.0 (see `LICENSE`). lewtoff is a clean-room port of portions of
libvorbis 1.3.7, which is BSD-3-Clause from the Xiph.Org Foundation; the
upstream attribution is in `NOTICE`. The vendored libvorbis source under
`tools/debug-libvorbis-dump/vendored-libvorbis/` retains its own
`COPYING`.
