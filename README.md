# lewtoff

A pure-Rust Ogg Vorbis **encoder** that produces byte-for-byte identical
output to libvorbis 1.3.7 at quality Q5 for a constrained input space.
Named after [lewton](https://github.com/RustAudio/lewton) — the pure-Rust
Vorbis decoder. lewtoff is its encoder counterpart.

The crate forbids `unsafe` and pulls in a single runtime dependency
(the `ogg` crate, for page framing and CRC). An optional `parallel`
feature adds Rayon-based block-level fan-out — see
[Parallel encoding](#parallel-encoding) below.

## Status

All synthetic parity tests (silence, sine 440, ramps, mono+stereo at
44.1k and 48k) match libvorbis 1.3.7 byte-for-byte. A real-audio corpus
sweep (`tests/corpus_sweep.rs::corpus_parity_sweep`) walks any
`<repo_root>/corpus/` directory the contributor has symlinked in and
asserts the same parity for every file recursively; the most recent
sweep over a 5954-file corpus passed 100% byte-identical. The EOS
`eofflag` / post-extrap `n_train` derivation runs a faithful chunk-by-
chunk streaming simulation of libvorbis's `vorbis_analysis_blockout`
(see `envelope::simulate_eofflag`) so no per-input hardcodes are needed.

## Use

```rust
let pcm: &[i16] = /* interleaved 16-bit PCM */;
let ogg = lewtoff::encode(
    pcm,
    lewtoff::SampleRate::Hz44100,
    lewtoff::Channels::Stereo,
);
```

Public surface is one function plus two enums. No `Result` (the input
space is closed by construction), no streaming, no `Write` trait.

## Supported input

| | |
|---|---|
| Sample format | Interleaved `i16` PCM |
| Sample rate   | 44100 Hz **or** 48000 Hz |
| Channels      | Mono **or** Stereo |
| Quality       | Q5 (≈160 kbps stereo, ≈80 kbps mono) |

Outside this space, behavior is undefined.

## Parallel encoding

For long inputs (minutes of audio), enable the `parallel` feature to fan
per-block analysis (MDCT, FFT, psy, floor, residue) across the global
Rayon thread pool. Bit-packing and Ogg page emission stay serial; the
output is byte-identical to the serial path.

```toml
[dependencies]
lewtoff = { version = "0.1", features = ["parallel"] }
```

Public API is unchanged. The feature is off by default to keep the
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
— Rayon would compose without oversubscribing, but the inner
job-collection vec adds a few microseconds per call you don't need.

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
base_quality per libvorbis template interval (-0.05, 0.05 … 0.95, 1.0,
plus 0.5) × 44.1/48 kHz × mono/stereo × silence / sine / ramp /
noise+tone / 10 s transients. Only q5 runs today; the rest are
`#[ignore = "pending quality wiring"]` until lewtoff accepts a quality
(see `tests/common/mod.rs`). Run them with `--run-ignored=all`. The
corpus sweep takes the quality via `CORPUS_QUALITY` (default 0.5), e.g.
`just corpus 100 "" 0.3`.

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
