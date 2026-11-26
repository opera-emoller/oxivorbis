# lewtoff

[![crates.io](https://img.shields.io/crates/v/lewtoff.svg)](https://crates.io/crates/lewtoff)
[![docs.rs](https://docs.rs/lewtoff/badge.svg)](https://docs.rs/lewtoff)

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

The name comes from [lewton](https://github.com/RustAudio/lewton), the
pure-Rust Vorbis decoder. lewtoff is the encoder to go with it.

## Install

```toml
[dependencies]
lewtoff = "0.1"
# or, for multi-core encoding of long inputs:
# lewtoff = { version = "0.1", features = ["parallel"] }
```

The minimum supported Rust version is 1.85 (edition 2024).

## Quick start

Encode a whole buffer in one call:

```rust
use lewtoff::{Channels, Options, SampleRate};

// One second of interleaved 16-bit stereo PCM (silence, for brevity).
let pcm = vec![0i16; 44_100 * 2];

// Quality 5, deterministic default serial, vendor "lewtoff <version>".
let ogg: Vec<u8> = lewtoff::encode(&pcm, SampleRate::Hz44100, Channels::Stereo, &Options::default());
assert_eq!(&ogg[..4], b"OggS");
```

Stream chunks of any size, including odd sample counts that split a
stereo frame. Each call returns the Ogg pages it completed:

```rust,no_run
use lewtoff::{Channels, Encoder, Options, SampleRate};
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
use lewtoff::{Channels, Encoder, Options, SampleRate};

let floats: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.0576).sin() * 0.5).collect();

let ogg = lewtoff::encode_f32(&floats, SampleRate::Hz48000, Channels::Mono, &Options::default());

// Encoder::write_f32 is the streaming form; it can be mixed with write().
let mut enc = Encoder::new(SampleRate::Hz48000, Channels::Mono, &Options::default());
let mut streamed = enc.write_f32(&floats).to_vec();
streamed.extend_from_slice(&enc.finish());
assert_eq!(streamed, ogg);
```

Set the quality, stream serial, vendor and comments:

```rust
use lewtoff::{Channels, Options, Quality, SampleRate};

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
let ogg = lewtoff::encode(&pcm, rate, channels, &opts);
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
  At extreme magnitudes libvorbis's own float-to-int conversions are
  undefined, but lewtoff still produces a valid stream. NaN and
  ±infinity make libvorbis emit a corrupt stream, so lewtoff encodes
  them as `0.0`.
- **Odd sample counts in stereo:** `Encoder::write` holds an unpaired
  trailing sample until the next call. `finish` and the one-shot
  functions drop a final unpaired sample.
- Other rates and more than two channels are not supported.
  `SampleRate` and `Channels` are `#[non_exhaustive]`, so support can
  be added later without breaking changes.

## Parity contract

- **Audio:** every audio packet, granule position and page boundary is
  identical to what libvorbis 1.3.7 produces when it is built without
  FMA contraction and fed the same samples in 64-frame
  `vorbis_analysis_wrote` chunks. How you split the input across
  `write` calls does not affect the output. The concatenated output of
  `write`/`write_f32` and `finish` is exactly what `encode`/`encode_f32`
  return.
- **Headers:** identical, given the same stream serial, vendor string
  and comments. lewtoff's defaults are its own. The serial is
  `lewtoff::DEFAULT_SERIAL`, the vendor is `lewtoff::DEFAULT_VENDOR`
  (`"lewtoff <version>"`), and there are no comments. Default output is
  therefore fully deterministic. It is *not* byte-identical to
  `oggenc -q X`, which picks a random serial and writes a different
  vendor and comments. To reproduce another encoder's headers, set
  `Options::{serial, vendor, comments}`.
- **oggenc's own audio** is not a byte-for-byte reference. oggenc feeds
  libvorbis 1024-frame chunks, which changes the LPC pre-extrapolation
  at the start of the stream. Its audio therefore matches lewtoff only
  where that makes no difference, such as silence. Packaged libvorbis
  builds may also use FMA.

**Status:** a real-audio corpus of 6653 files is byte-identical to the
libvorbis 1.3.7 reference build in all four rate/channel configurations
at q-1, q5 and q10 (79,836 encodes), and at 44.1 kHz stereo also at q0,
q2, q3 and q8. A differential fuzzer has run more than 100k generated
cases clean. Parity is verified on macOS arm64 and Linux x86_64 (glibc).
lewtoff's output is also identical on Windows x86_64 (MSVC).

This works because libvorbis's encoder uses f32 throughout. Without FMA
contraction, Rust's `f32` arithmetic on every tier-1 target gives the
same IEEE 754 results as that C build. Transcendentals
(`sin`/`cos`/`log`/`exp`) are precomputed into tables taken from
libvorbis, so at runtime the encoder only does table lookups and
arithmetic, and platform libm differences don't matter. See
[CONTRIBUTING.md](CONTRIBUTING.md) for how parity is enforced.

## Features

| Feature    | Default | Effect |
|------------|---------|--------|
| `parallel` | off     | Analyzes blocks on the global Rayon pool. Adds `rayon` as a dependency. |

The `parallel` feature spreads per-block analysis (MDCT, FFT,
psychoacoustics, floor, residue) across the global
[Rayon](https://crates.io/crates/rayon) thread pool. Envelope analysis,
bit-packing and Ogg page emission stay serial. The API is unchanged and
the output bytes are identical to the serial build. Blocks are
processed in bounded, pipelined batches of 32 × the thread count, so
streaming memory stays bounded. In exchange, `write` hands back pages a
few seconds of audio later than the serial build.

Leave `parallel` off in these cases:

- **WASM:** Rayon in browsers needs `SharedArrayBuffer` and
  cross-origin isolation headers. The default build has no such
  requirements and compiles for `wasm32-unknown-unknown` as is.
- **You already parallelize across files**, for example in an asset
  pipeline. Rayon would not oversubscribe the CPU, but the per-batch
  bookkeeping is overhead you don't need.
- **Short clips:** sub-second inputs have too few blocks to keep all
  cores busy.

The crate's other features (`oracle`, `__internal`) exist only for its
own test suite and are not part of the API.

## Performance

Measured on a 10-core Apple M4 with Rust 1.95, encoding 60 s of 44.1 kHz
stereo at Q5 (s16le from stdin, Ogg to `/dev/null`, hyperfine via
`just bench`). The reference is `oggenc` 1.4.3 / libvorbis 1.3.7 from
Homebrew, an optimized build.

| Encoder              | Wall time | CPU time | Peak RSS, 10 min streamed |
|----------------------|-----------|----------|---------------------------|
| lewtoff (serial)     | ~305 ms   | ~300 ms  | ~3.5 MB                   |
| lewtoff (`parallel`) | ~73 ms    | ~520 ms  | ~22 MB                    |
| oggenc               | ~268 ms   | ~265 ms  | ~3 MB                     |

The input was a synthesized tone-plus-pink-noise signal. On real music
the serial build takes around 335 ms for the same length. Serial
lewtoff runs at roughly 0.8 to 0.9× the speed of optimized libvorbis.
The parallel build is about 3.5 to 4× faster than oggenc on long
inputs, and the speedup grows with input length. The RSS column uses
`Encoder` streaming a 10-minute input; memory does not grow with input
length.

## License

Apache-2.0 (see [`LICENSE`](LICENSE)). lewtoff is a port of portions of
libvorbis 1.3.7, which is BSD-3-Clause and copyright the Xiph.Org
Foundation. The upstream attribution is in [`NOTICE`](NOTICE).
