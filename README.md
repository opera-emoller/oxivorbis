# oxivorbis

[![crates.io](https://img.shields.io/crates/v/oxivorbis.svg)](https://crates.io/crates/oxivorbis)
[![docs.rs](https://docs.rs/oxivorbis/badge.svg)](https://docs.rs/oxivorbis)

A pure-Rust Ogg Vorbis **encoder** that produces the same bytes as
libvorbis 1.3.7. It supports VBR mode at oggenc qualities -1 to 10,
including fractional ones.

- No `unsafe`, no C, no build script. The only runtime dependency is
  [`ogg`](https://crates.io/crates/ogg), used for page framing and CRC.
- Works on `wasm32-unknown-unknown`.
- Has a one-shot API and a streaming API, and takes `i16` or `f32`
  input. Streaming memory stays bounded however long the input is.
- No `Result`s and no panics. Rates, channel layouts and qualities are
  typed, and every input encodes.
- An optional `parallel` feature encodes long inputs several times
  faster. Its output is byte-identical to the serial path.

Pairs naturally with [lewton](https://github.com/RustAudio/lewton), the
pure-Rust Vorbis decoder.

## Install

```toml
[dependencies]
oxivorbis = "0.1"
# or, for multi-core encoding of long inputs:
# oxivorbis = { version = "0.1", features = ["parallel"] }
```

The minimum supported Rust version is 1.85 (edition 2024).

## Quick start

Encode a whole buffer in one call:

```rust
use oxivorbis::{Channels, Options, SampleRate};

// One second of interleaved 16-bit stereo PCM (silence, for brevity).
let pcm = vec![0i16; 44_100 * 2];

// Quality 5, deterministic default serial, vendor "oxivorbis <version>".
let ogg: Vec<u8> = oxivorbis::encode(&pcm, SampleRate::Hz44100, Channels::Stereo, &Options::default());
assert_eq!(&ogg[..4], b"OggS");
```

Stream chunks of any size, including odd sample counts that split a
stereo frame. Each call returns the Ogg pages it completed:

```rust,no_run
use oxivorbis::{Channels, Encoder, Options, SampleRate};
use std::io::Write;

fn main() -> std::io::Result<()> {
    let mut enc = Encoder::new(SampleRate::Hz44100, Channels::Stereo, &Options::default());
    let mut out = std::fs::File::create("out.ogg")?;
    for chunk in vec![0i16; 10 * 44_100 * 2].chunks(4096) {
        // e.g. blocks coming from a decoder
        out.write_all(enc.write(chunk))?; // pages completed so far (may be empty)
    }
    out.write_all(&enc.finish())?; // the rest of the stream
    Ok(())
}
```

`f32` input works the same way. Samples are nominally in -1.0..=1.0 and
are passed to the encoder unscaled:

```rust
use oxivorbis::{Channels, Encoder, Options, SampleRate};

let floats: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.0576).sin() * 0.5).collect();

let ogg = oxivorbis::encode_f32(&floats, SampleRate::Hz48000, Channels::Mono, &Options::default());

// Encoder::write_f32 is the streaming form; it can be mixed with write().
let mut enc = Encoder::new(SampleRate::Hz48000, Channels::Mono, &Options::default());
let mut streamed = enc.write_f32(&floats).to_vec();
streamed.extend_from_slice(&enc.finish());
assert_eq!(streamed, ogg);
```

Set the quality, stream serial, vendor and comments:

```rust
use oxivorbis::{Channels, Options, Quality, SampleRate};

let opts = Options::default()
    .with_quality(Quality::new(7.5).expect("in -1..=10")) // oggenc -q 7.5
    .with_serial(0x1234_5678)
    .with_vendor("my-app 2.0")
    .with_comment("TITLE", "Example")
    .with_comment("ARTIST", "Someone");

// Rates and channel counts from a file header convert with TryFrom.
let rate = SampleRate::try_from(44_100u32).expect("44100 or 48000");
let channels = Channels::try_from(2u16).expect("1 or 2");

let pcm = vec![0i16; 44_100 * 2];
let ogg = oxivorbis::encode(&pcm, rate, channels, &opts);
assert_eq!(&ogg[..4], b"OggS");
```

[`examples/`](examples/) has two runnable programs. `encode_wav`
converts a 16-bit WAV file to `.ogg`. `stream` reads s16le from stdin
and writes Ogg to stdout:

```sh
cargo run --release --example encode_wav -- input.wav output.ogg 6
ffmpeg -i song.flac -f s16le -ar 44100 -ac 2 - | cargo run --release --example stream -- 44100 2 > song.ogg
```

## Supported input

| | |
|---|---|
| Sample format | Interleaved `i16` (scaled by 1/32768) or `f32` (passed through unscaled) |
| Sample rate   | 44100 Hz **or** 48000 Hz (`SampleRate`) |
| Channels      | Mono **or** stereo (`Channels`) |
| Quality       | -1.0 to 10.0 on oggenc's `-q` scale (fractional ok); default 5 |

- **Quality** follows oggenc's `-q` scale. `Quality::new(q)` returns
  `None` for NaN or for values outside -1..=10. The value is converted
  to libvorbis's `base_quality` exactly as oggenc 1.4.3 does it, with
  `(float)((double)(float)q * 0.1)`. So `Quality::new(9.344)` picks the
  same setup, and the same nominal bitrate in the ID header, as
  `oggenc -q 9.344`. Qualities below 0 select libvorbis's q-1 template
  (512/4096-sample blocks) and match byte for byte as well.
- **f32 edge cases:** values outside ±1.0, subnormals and `-0.0` are
  encoded the way libvorbis encodes them. Parity is tested up to ±10⁴.
  At extreme magnitudes, quantized values pass ±2³¹, where libvorbis's
  float-to-int conversions are undefined behaviour in C. oxivorbis's
  conversions saturate instead (Rust `as`), and the integer sums over
  them wrap, as C's do in practice. The result is always a valid,
  decodable stream, but it is not guaranteed to match libvorbis there.
  This is tested up to ±`f32::MAX` at every quality and configuration
  with overflow checks on. NaN and ±infinity make libvorbis emit a
  corrupt stream, so oxivorbis encodes them as `0.0`.
- **Odd sample counts in stereo:** `Encoder::write` holds an unpaired
  trailing sample until the next call. `finish` and the one-shot
  functions drop a final unpaired sample.
- Other rates and more than two channels are not supported.
  `SampleRate` and `Channels` are `#[non_exhaustive]`, so support can
  be added later without breaking changes.

## Parity contract

- **Audio:** every audio packet, granule position and page boundary is
  identical to what the unmodified libvorbis 1.3.7 and libogg 1.3.5
  sources produce when built without FMA contraction, with a stable
  `qsort`, and fed the same samples in
  64-frame `vorbis_analysis_wrote` chunks. The `qsort` condition matters
  at q0 to q3 only: libvorbis's noise normalization sorts values that
  can tie, and oxivorbis breaks ties the stable way. The reference build
  links a stable merge sort. glibc's `qsort`, a merge sort, gives the
  same order. macOS libc's `qsort` can order ties differently, so stock
  libvorbis on macOS can differ from oxivorbis at q0 to q3. How you split the input across
  `write` calls does not affect the output. The concatenated output of
  `write`/`write_f32` and `finish` is exactly what `encode`/`encode_f32`
  return.
- **Headers:** identical, given the same stream serial, vendor string
  and comments. oxivorbis's defaults are its own. The serial is
  `oxivorbis::DEFAULT_SERIAL`, the vendor is `oxivorbis::DEFAULT_VENDOR`
  (`"oxivorbis <version>"`), and there are no comments. Default output is
  therefore fully deterministic. It is *not* byte-identical to
  `oggenc -q X`, which picks a random serial and writes a different
  vendor and comments. To reproduce another encoder's headers, set
  `Options::{serial, vendor, comments}`.
- **oggenc's own audio** is not a byte-for-byte reference. oggenc feeds
  libvorbis 1024-frame chunks, which changes the LPC pre-extrapolation
  at the start of the stream. Its audio therefore matches oxivorbis only
  where that makes no difference, such as silence. Packaged libvorbis
  builds may also use FMA.

**Status:** against a reference build of the unmodified libvorbis 1.3.7
and libogg 1.3.5 sources (vendored and checksum-verified under `tools/`),
a real-audio corpus of 6653 files is byte-identical in all four
rate/channel configurations at q-1, q5 and q10 (79,836 encodes), and at
44.1 kHz stereo also at q0, q2, q3 and q8. A differential fuzzer has
run more than 190,000 generated cases clean, with 16-bit and f32 input
and inputs of up to 30 s, and CI compares 30 to 120 s of non-silent
audio on every push. Parity is verified on macOS arm64, Linux x86_64
(glibc) and wasm32 (under wasmtime). oxivorbis's output is also identical
on Windows x86_64 (MSVC).

This works because libvorbis's encoder uses f32 throughout. Without FMA
contraction, Rust's `f32` arithmetic on every tier-1 target gives the
same IEEE 754 results as that C build. The per-block work is that
arithmetic, `sqrt` (correctly rounded everywhere) and table lookups.
The MDCT, FFT and block-window trig tables are taken bit for bit from
libvorbis. A few setup-time tables do call the platform libm, as
libvorbis does: the envelope analysis windows (`sin`), and the
psychoacoustic model's bark and octave scales, ATH and tone-curve maps
(`atan`, `log`, `exp`). libm results can differ by an ulp between
platforms. Every decision those values feed (rounded band edges and
comparisons) has been checked to be robust to ±1 ulp in all supported
configurations, so the output does not depend on the platform's libm.
See [CONTRIBUTING.md](CONTRIBUTING.md) for how parity is enforced.

## Features

| Feature    | Default | Effect |
|------------|---------|--------|
| `parallel` | off     | Analyzes blocks on the global Rayon pool. Adds `rayon` as a dependency. |

The `parallel` feature spreads per-block analysis (MDCT, FFT,
psychoacoustics, floor, residue) across the global
[Rayon](https://crates.io/crates/rayon) thread pool, or across the
current pool when called inside `ThreadPool::install`. Envelope analysis,
bit-packing and Ogg page emission stay serial. The API is unchanged and
the output bytes are identical to the serial build.

Blocks are processed in pipelined batches of 32 per Rayon thread, capped
at 256 long blocks (about 6 s of 44.1 kHz audio), with up to three
batches in flight. In exchange for the speed, `write` hands back pages
later than the serial build, and streaming uses more memory. Both are
bounded, independent of input length and, past 8 threads, of the core
count. Measured streaming 10 minutes of 44.1 kHz stereo at q5:

| Rayon threads | Output lag behind input | Peak RSS |
|---------------|-------------------------|----------|
| serial build  | 0.35 s                  | 3.6 MB   |
| 1             | 2.4 s                   | 6.0 MB   |
| 10            | 15.6 s                  | 19 MB    |
| 64            | 15.6 s                  | 30 MB    |

The lag bound is about 16 s of audio at any thread count and quality.
Memory grows slowly past 10 threads from per-thread scratch buffers.

Encoding from inside a Rayon pool also works, for example one file per
`par_iter` item or under `ThreadPool::install`, including a 1-thread
pool. On a pool thread each batch runs on Rayon's work-stealing joins
instead of blocking, so there is no deadlock and no oversubscription.

Leave `parallel` off in these cases:

- **WASM:** Rayon in browsers needs `SharedArrayBuffer` and
  cross-origin isolation headers. The default build has no such
  requirements and compiles for `wasm32-unknown-unknown` as is.
- **You already parallelize across files**, for example in an asset
  pipeline. That is safe with `parallel` on, but it isn't needed: one
  file per core already keeps the CPU busy.
- **Short clips:** sub-second inputs have too few blocks to keep all
  cores busy.

The crate's other features (`oracle`, `__internal`) exist only for its
own test suite and are not part of the API.

## Performance

Encoding a 261 s real stereo track (44.1 kHz s16le from stdin, Ogg to
`/dev/null`, hyperfine, mean of 6–8 runs). The reference is `oggenc`
1.4.3 / libvorbis 1.3.7 as packaged: Homebrew on macOS, Ubuntu on
Linux. libvorbis's build defaults to `-O3 -ffast-math`, which lets the
compiler fuse and reorder float operations. oxivorbis can't do that and
stay bit-exact, so its speed comes from doing less work rather than
looser arithmetic.

| Machine | Quality | oggenc | oxivorbis serial | oxivorbis `parallel` |
|---|---|---|---|---|
| Apple M4 (10 cores) | q-1 | 1.67 s | **0.93 s** | **0.20 s** |
| | q5 | 1.29 s | **0.77 s** | **0.18 s** |
| | q10 | 1.46 s | **0.90 s** | **0.20 s** |
| Intel Core Ultra 7 356H (16 cores, serial pinned to a P-core) | q-1 | 2.17 s | **1.26 s** | **0.20 s** |
| | q5 | 1.80 s | **1.17 s** | **0.20 s** |
| | q10 | 2.06 s | **1.40 s** | **0.23 s** |

Serial oxivorbis is about 1.5–1.8× as fast as oggenc. With `parallel` it is
7–11× faster on long inputs, at about 1.6× the total CPU time of serial.

Memory: streaming through `Encoder` peaks at about 3–4 MB serial and
about 17–19 MB with `parallel` (M4/x86), whatever the input length.
One-shot `encode` also holds the whole input and output. A new
`Encoder` costs about 20 µs once the setup for its rate, channels and
quality is cached, and under about 1 ms the first time in a process.

## License

Apache-2.0 (see [`LICENSE`](LICENSE)). oxivorbis is a port of portions of
libvorbis 1.3.7, which is BSD-3-Clause and copyright the Xiph.Org
Foundation. The upstream attribution is in [`NOTICE`](NOTICE).
