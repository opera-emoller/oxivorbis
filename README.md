# oxivorbis

[![crates.io](https://img.shields.io/crates/v/oxivorbis.svg)](https://crates.io/crates/oxivorbis)
[![docs.rs](https://docs.rs/oxivorbis/badge.svg)](https://docs.rs/oxivorbis)

A pure-Rust Ogg Vorbis **encoder** that produces the same bytes as
libvorbis 1.3.7, and is faster than it: 1.5–1.8× `oggenc` on one core,
7–11× with the `parallel` feature.

- VBR at oggenc qualities -1 to 10 (fractional too), and libvorbis's
  bitrate management: average bitrate, hard min/max ("CBR"), quality with
  limits, as oggenc's `-b`, `-m`, `-M` and `--managed` set them up.
- Any rate from 1 Hz to 200 kHz, 1 to 255 channels (5.1 coupled as
  libvorbis does it), `i16` or `f32`, interleaved or planar.
- One-shot and streaming APIs; streaming memory stays bounded.
- No `unsafe`, no C, no build script, no panics. The only runtime
  dependency is [`ogg`](https://crates.io/crates/ogg). Works on
  `wasm32-unknown-unknown`.
- Comes with an oggenc-style [command-line encoder](#command-line-encoder)
  and a [C API](#use-from-c-and-other-languages).

Pairs naturally with [lewton](https://github.com/RustAudio/lewton), the
pure-Rust Vorbis decoder.

## Install

```toml
[dependencies]
oxivorbis = "0.1"
# or, for multi-core encoding of long inputs:
# oxivorbis = { version = "0.1", features = ["parallel"] }
```

The minimum supported Rust version is 1.85.

## Usage

Encode a whole buffer in one call:

```rust
use oxivorbis::{Channels, Options, Quality, SampleRate};

let pcm = vec![0i16; 44_100 * 2]; // 1 s of interleaved stereo
let opts = Options::default()
    .with_quality(Quality::new(7.5).expect("in -1..=10")) // oggenc -q 7.5
    .with_comment("TITLE", "Example");
let ogg: Vec<u8> = oxivorbis::encode(&pcm, SampleRate::HZ_44100, Channels::STEREO, &opts);
assert_eq!(&ogg[..4], b"OggS");
```

Or stream chunks of any size. Each call returns the pages it completed,
and the result is the same bytes as the one-shot call:

```rust
use oxivorbis::{Channels, Encoder, Options, SampleRate};

let mut enc = Encoder::new(SampleRate::HZ_48000, Channels::MONO, &Options::default());
let mut ogg = Vec::new();
for _ in 0..10 {
    let chunk: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
    ogg.extend_from_slice(enc.write_f32(&chunk)); // or write() for i16
}
ogg.extend_from_slice(&enc.finish());
assert_eq!(&ogg[..4], b"OggS");
```

`OggWriter` writes pages straight to any `std::io::Write` sink and takes
planar input. `S16LeWriter` encodes raw s16le bytes, e.g. through
`std::io::copy` from stdin:

```rust,no_run
use oxivorbis::{Channels, OggWriter, Options, SampleRate};
use std::{fs::File, io::BufWriter};

fn main() -> std::io::Result<()> {
    let sink = BufWriter::new(File::create("out.ogg")?);
    let mut ogg = OggWriter::new(sink, SampleRate::HZ_48000, Channels::STEREO, &Options::default())?;
    let (left, right) = (vec![0.0f32; 48_000], vec![0.0f32; 48_000]);
    ogg.write_planar_f32(&[&left, &right])?;
    ogg.finish()?;
    Ok(())
}
```

Bitrate modes are checked once, when the encoder is created, and return
an error where libvorbis has no setup for the request:

```rust
use oxivorbis::{Bitrate, BitrateMode, Channels, Options, SampleRate};

// oggenc --managed -b 128 -m 128 -M 128
let cbr = Bitrate::new(BitrateMode::Average { kbps: 128, min_kbps: Some(128), max_kbps: Some(128) });
let pcm = vec![0i16; 44_100 * 2];
let ogg = oxivorbis::encode_with_bitrate(&pcm, SampleRate::HZ_44100, Channels::STEREO, &Options::default(), &cbr)
    .expect("in libvorbis's range");
assert_eq!(&ogg[..4], b"OggS");
```

Input is in Vorbis channel order; `reorder_wav_to_vorbis` converts from
WAV order. [`examples/`](examples/) has runnable programs (WAV to Ogg,
stdin streaming, planar input, transcoding MP3/FLAC via Symphonia), and
[docs/reference.md](docs/reference.md) covers every mode, limit and
channel layout in detail.

## Command-line encoder

[`oxivorbis-cli`](tools/oxivorbis-cli/README.md) installs an
`oxivorbis` binary that works much like `oggenc`, for WAV (8 to 64-bit,
integer or float, RF64) and raw PCM:

```sh
cargo install oxivorbis-cli   # add --features parallel for multi-core
oxivorbis -q 6 -a "Artist" -t "Title" track.wav -o out.ogg
ffmpeg -i in.flac -f wav - | oxivorbis - > out.ogg
```

## Use from C and other languages

[`ffi/`](ffi/README.md) builds `liboxivorbis` with a C header
([`ffi/include/oxivorbis.h`](ffi/include/oxivorbis.h)), byte-identical
to the Rust API, with streaming, callbacks and the bitrate modes.
Prebuilt libraries for macOS, Linux and Windows are on the
[releases page](https://github.com/opera-emoller/oxivorbis/releases):

```c
oxv_buffer ogg;
if (oxv_encode_i16(pcm, samples, 44100, 2, NULL, &ogg) == OXV_STATUS_OK) {
    fwrite(ogg.data, 1, ogg.len, f);
    oxv_buffer_free(&ogg);
}
```

## Parity with libvorbis

Every audio packet, granule position and page boundary is identical to
the unmodified libvorbis 1.3.7 / libogg 1.3.5 sources built without FMA
contraction and fed 64-frame chunks. Headers match given the same serial,
vendor and comments; oxivorbis's defaults are fixed, so its output is
deterministic (and not byte-identical to `oggenc`, which uses a random
serial).

The output for a given input and options is part of the API: it changes
only in a semver-breaking release (for example, a move to a newer
libvorbis as the reference).

This is tested against a vendored reference build: a 6653-file real-audio
corpus across rate templates, qualities, channel layouts and bitrate
modes (over 280,000 encodes), about 965,000 differential-fuzzer cases at
1 to 12 channels, and a parity suite on every CI push, on macOS arm64,
Linux x86_64 and wasm32. See
[docs/reference.md](docs/reference.md#parity-contract) for the exact
contract and edge cases, and [CONTRIBUTING.md](CONTRIBUTING.md) for how
it is enforced.

## Performance

A 261 s stereo track at 44.1 kHz, compared with `oggenc` 1.4.3 /
libvorbis 1.3.7 as packaged (Homebrew, Ubuntu):

| q5 | `oggenc` | oxivorbis | oxivorbis `parallel` |
|---|---|---|---|
| Apple M4 (10 cores) | 1.29 s | **0.77 s** | **0.18 s** |
| Intel Core Ultra 7 356H (16 cores) | 1.80 s | **1.17 s** | **0.20 s** |

libvorbis builds with `-ffast-math`; oxivorbis can't and stay bit-exact,
so the speed comes from doing less work, not looser arithmetic.
Streaming peaks at about 4 MB serial and 19 MB with `parallel`, whatever
the input length. The managed bitrate modes are about 4× slower than VBR
(still about 2× `oggenc --managed`).

`parallel` spreads per-block analysis over the Rayon pool, with identical
output. Leave it off for WASM, short clips, or when you already encode
one file per core. More numbers and its latency/memory trade-offs are in
[docs/reference.md](docs/reference.md#performance).

## License

Apache-2.0 (see [`LICENSE`](LICENSE)). oxivorbis is a port of portions of
libvorbis 1.3.7, which is BSD-3-Clause and copyright the Xiph.Org
Foundation. The upstream attribution is in [`NOTICE`](NOTICE).
