# oxivorbis

[![crates.io](https://img.shields.io/crates/v/oxivorbis.svg)](https://crates.io/crates/oxivorbis)
[![docs.rs](https://docs.rs/oxivorbis/badge.svg)](https://docs.rs/oxivorbis)

A pure-Rust Ogg Vorbis **encoder** that produces the same bytes as
libvorbis 1.3.7. It supports VBR mode at oggenc qualities -1 to 10,
including fractional ones, and libvorbis's bitrate management: average
bitrate, hard minimum/maximum ("CBR"), and quality mode with limits, as
oggenc's `-b`, `-m`, `-M` and `--managed` set them up.

**Faster than libvorbis itself: 1.5–1.7× `oggenc` on one core, and
7–9× with the `parallel` feature.**

| 261 s stereo track, q5 | `oggenc` (libvorbis) | oxivorbis | oxivorbis `parallel` |
|---|---|---|---|
| Apple M4 | 1.29 s | **0.77 s** | **0.18 s** |
| Intel Core Ultra 7 (x86_64) | 1.80 s | **1.17 s** | **0.20 s** |

Same output bytes as libvorbis, so the speed doesn't come from cutting
corners. Details and other qualities are under [Performance](#performance).

- No `unsafe`, no C, no build script. The only runtime dependency is
  [`ogg`](https://crates.io/crates/ogg), used for page framing and CRC.
- Works on `wasm32-unknown-unknown`.
- Has a one-shot API and a streaming API, and takes `i16` or `f32`
  input. Streaming memory stays bounded however long the input is.
- No `Result`s and no panics. Rates, channel counts and qualities are
  typed, and every input encodes.
- Mono, stereo, 5.1 surround and any other channel count up to 255,
  each exactly as libvorbis encodes it (5.1 with libvorbis's coupling and
  separate LFE coding).
- An optional `parallel` feature spreads long inputs across all cores
  (7–9× `oggenc` above). Its output is byte-identical to the serial path.
- Comes with an oggenc-style [command-line encoder](#command-line-encoder)
  for WAV and raw PCM.

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
let ogg: Vec<u8> = oxivorbis::encode(&pcm, SampleRate::HZ_44100, Channels::STEREO, &Options::default());
assert_eq!(&ogg[..4], b"OggS");
```

Stream chunks of any size, including odd sample counts that split a
stereo frame. Each call returns the Ogg pages it completed:

```rust,no_run
use oxivorbis::{Channels, Encoder, Options, SampleRate};
use std::io::Write;

fn main() -> std::io::Result<()> {
    let mut enc = Encoder::new(SampleRate::HZ_44100, Channels::STEREO, &Options::default());
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

let ogg = oxivorbis::encode_f32(&floats, SampleRate::HZ_48000, Channels::MONO, &Options::default());

// Encoder::write_f32 is the streaming form; it can be mixed with write().
let mut enc = Encoder::new(SampleRate::HZ_48000, Channels::MONO, &Options::default());
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
let rate = SampleRate::try_from(44_100u32).expect("1..=200000 Hz");
let channels = Channels::try_from(2u16).expect("1..=255");

let pcm = vec![0i16; 44_100 * 2];
let ogg = oxivorbis::encode(&pcm, rate, channels, &opts);
assert_eq!(&ogg[..4], b"OggS");
```

[`Options::with_bitrate`]: https://docs.rs/oxivorbis/latest/oxivorbis/struct.Options.html#method.with_bitrate

[`examples/`](examples/) has two runnable programs. `encode_wav`
converts a 16-bit WAV file to `.ogg`. `stream` reads s16le from stdin
and writes Ogg to stdout:

```sh
cargo run --release --example encode_wav -- input.wav output.ogg 6
ffmpeg -i song.flac -f s16le -ar 44100 -ac 2 - | cargo run --release --example stream -- 44100 2 > song.ogg
```

## Writing to files and streams

`OggWriter` owns an `Encoder` and any `std::io::Write` sink (a file,
socket, `Vec<u8>`, ...) and writes each page as soon as it is complete.
`finish` ends the stream, flushes the sink and returns it. Planar input
(one slice per channel, as most decoders and DSP crates produce) is
accepted directly, by `OggWriter` and by `Encoder::write_planar_f32` /
`write_planar_i16`, with the same bytes as interleaving it first:

```rust,no_run
use oxivorbis::{Channels, OggWriter, Options, SampleRate};
use std::{fs::File, io::BufWriter};

fn main() -> std::io::Result<()> {
    let sink = BufWriter::new(File::create("out.ogg")?);
    let mut ogg = OggWriter::new(sink, SampleRate::HZ_48000, Channels::STEREO, &Options::default())?;
    let (left, right) = (vec![0.0f32; 4800], vec![0.0f32; 4800]);
    for _ in 0..10 {
        ogg.write_planar_f32(&[&left, &right])?; // or write_i16 / write_f32
    }
    ogg.finish()?;
    Ok(())
}
```

`S16LeWriter` takes raw interleaved s16le bytes through `std::io::Write`,
so `std::io::copy(&mut stdin, &mut writer)` encodes a PCM stream. Sink
errors are returned as `io::Error`; after one, the stream in the sink
is incomplete. More runnable programs: `encode_file` (streams a WAV into
a file through `S16LeWriter`), `planar` (synthesized planar audio) and
`transcode` (MP3, FLAC, WAV or Vorbis decoded with Symphonia, a
dev-dependency only):

```sh
cargo run --release --example transcode -- song.mp3 song.ogg 6
```

## Bitrate modes

VBR at a quality is the default. [`Options::with_bitrate`] switches to
one of libvorbis's bitrate-driven setups instead; each mode is one way
oggenc 1.4.3 configures libvorbis, and the output matches libvorbis
configured by those oggenc flags:

| `BitrateMode`                                  | oggenc flags                     | What it does |
|------------------------------------------------|----------------------------------|--------------|
| `Nominal { kbps }`                             | `-b K`                           | VBR with the setup for about K kb/s; no reservoir |
| `Average { kbps, min_kbps: None, max_kbps: None }` (`BitrateMode::average(K)`) | `--managed -b K` | Average bitrate (ABR) through the bit reservoir |
| `Average { kbps, min_kbps, max_kbps }`         | `--managed -b K -m MIN -M MAX`   | ABR with hard limits; `min = avg = max` is libvorbis's "CBR" |
| `Limits { min_kbps, max_kbps }`                | `-m MIN` / `-M MAX` (no `-b`)    | Hard limits only, setup for their midpoint |
| `QualityLimits { min_kbps, max_kbps }`         | `-q Q -m MIN -M MAX`             | Quality mode at `Options::quality`, with hard limits |

```rust
use oxivorbis::{BitrateMode, Channels, Options, SampleRate};

let (rate, ch) = (SampleRate::HZ_44100, Channels::STEREO);
// oggenc --managed -b 128 -m 128 -M 128
let cbr = BitrateMode::Average { kbps: 128, min_kbps: Some(128), max_kbps: Some(128) };
let opts = Options::default().with_bitrate(rate, ch, cbr).expect("in libvorbis's range");
let ogg = oxivorbis::encode(&vec![0i16; 44_100 * 2], rate, ch, &opts);
assert_eq!(&ogg[..4], b"OggS");
```

- **Validation.** libvorbis only has bitrate setups for 8 to 50 kHz, and
  each rate and channel count supports a range of average bitrates
  (`BitrateMode::supported_kbps`: 45 to 500 kb/s for 44.1 kHz stereo, 8 to
  42 kb/s per channel at 8 kHz, ...). `with_bitrate` takes the input's rate
  and channels and returns a `BitrateError` where libvorbis would fail
  (`OV_EIMPL`) or for inconsistent requests (minimum above maximum, a
  limits mode without limits; `OV_EINVAL`). Encoding itself stays
  infallible: `Encoder::new` and `encode` never fail for options that
  passed the check. Options checked for one rate and used for another
  are re-checked, and panic only if libvorbis can't encode them there.
- **Quality with a hard maximum** (`-q 6 -M 192`), a common setup,
  works even when the quality averages more than the maximum. oggenc
  itself refuses `-q 6 -M 128` (it first passes the quality's nominal
  bitrate as the average, which libvorbis checks against the limits),
  but libvorbis's API allows it and the setup oggenc would end with is
  the same.
- **Reservoir tuning.** `Options::with_bitrate_tuning` takes a
  `BitrateTuning`: oggenc's `--advanced-encode-option`
  `bit_reservoir_bits`, `bit_reservoir_bias` and `bitrate_average_damping`
  (the `ovectl_ratemanage2_arg` fields).
- **ID header.** `bitrate_upper`, `bitrate_nominal` and `bitrate_lower`
  are what libvorbis writes for the same oggenc flags (e.g. `-1` for an
  unset limit with `-b`, `0` without it, `-1000` in quality mode).
- **Streaming and `parallel`** work as in VBR, with the same bytes.
  Choosing among a block's candidate packets is sequential (the
  reservoir carries over), but each block's analysis isn't, so the
  parallel pipeline analyzes batches in parallel and chooses serially.
- **Cost.** Managed modes (all but `Nominal`, which costs the same as
  VBR) encode every block 15 times, from the smallest to the largest
  candidate packet, as libvorbis does. That makes them about 4× slower
  than VBR (261 s stereo track at 160 kb/s: 3.0 s serial on the M4 vs
  0.77 s for q5; 5.0 s vs 1.18 s on the Intel), still about twice as fast
  as `oggenc --managed` (6.3 s on the M4). Memory per block is the 15
  candidate packets (about 7 KB per long block at 160 kb/s stereo) plus
  8 KB of floor posts per analysis scratch: streaming serially peaks
  0.1 MB higher than VBR, and with `parallel` about 5 MB higher (each
  batch's candidates wait for the serial choice).

libvorbis's other encoder ctls, `OV_ECTL_LOWPASS_SET`,
`OV_ECTL_IBLOCK_SET` and `OV_ECTL_COUPLING_SET` (oggenc's
`lowpass_frequency`, `impulse_noisetune` and `disable_coupling` advanced
options), are `Options::with_encoder_tuning` with an `EncoderTuning`,
in VBR and the bitrate modes alike (libvorbis ignores
`disable_coupling` in the bitrate modes).

Not supported: the deprecated `OV_ECTL_RATEMANAGE_*` ctls (superseded by
the `RATEMANAGE2` ones the modes use), and oggenc's `bitrate_average` /
`bitrate_hard_min` / `bitrate_hard_max` advanced options (the same knobs
as the modes' fields, re-applied after the setup).

## Command-line encoder

[`tools/oxivorbis-cli`](tools/oxivorbis-cli/README.md) builds an
`oxivorbis` binary that works much like `oggenc`:

```sh
cargo install --path tools/oxivorbis-cli   # add --features parallel for multi-core
oxivorbis song.wav                         # writes song.ogg at q3, as oggenc does
oxivorbis -q 6 -a "Artist" -l "Album" -t "Title" track.wav -o out.ogg
ffmpeg -i in.flac -f wav - | oxivorbis - > out.ogg
```

It reads WAV (8-bit, 16/24/32-bit integer, 32/64-bit float, extensible,
RF64, streamed) or raw PCM (`--raw -R <hz> -C <n> -F <fmt>`), and takes
several inputs at once. It supports oggenc's tag flags
(`-c TAG=VALUE -t -a -l -N -G -d`), refuses to overwrite existing files
without `-f`, and generates its own shell completions and man page
(`--generate`). 16-bit WAV input produces the same bytes as
`oxivorbis::encode` (WAV with 3 to 8 channels after reordering it from
WAV to Vorbis channel order, as oggenc does). See its README for each format's conversion, the
exit codes and the overwrite policy.

## Use from C and other languages

[`ffi/`](ffi/README.md) builds `liboxivorbis`, a C API (header
[`ffi/include/oxivorbis.h`](ffi/include/oxivorbis.h)) for C, C++, Python,
C#, Swift, Godot, Unity and anything else with a C FFI. Its output is
byte-identical to the Rust API.

```sh
cargo build --release -p oxivorbis-ffi   # liboxivorbis.{so,dylib,a} / oxivorbis.dll
```

```c
oxv_buffer ogg;
if (oxv_encode_i16(pcm, samples, 44100, 2, NULL, &ogg) == OXV_STATUS_OK) {
    fwrite(ogg.data, 1, ogg.len, f);
    oxv_buffer_free(&ogg);
}
```

It has a streaming encoder, a callback writer and an options handle, and
it reports errors as status codes. See [ffi/README.md](ffi/README.md) for
linking (pkg-config, CMake), ownership and threading rules, and a Python
ctypes binding.

## Supported input

| | |
|---|---|
| Sample format | Interleaved `i16` (scaled by 1/32768) or `f32` (passed through unscaled) |
| Sample rate   | Any rate from 1 Hz to 200 kHz (`SampleRate`), as libvorbis |
| Channels      | 1 to 255 (`Channels`), in Vorbis channel order; see [Multichannel and 5.1](#multichannel-and-51) |
| Quality       | -1.0 to 10.0 on oggenc's `-q` scale (fractional ok); default 5 |
| Bitrate modes | 8 kHz to 50 kHz, within each rate's range (see [Bitrate modes](#bitrate-modes)) |

- **Quality** follows oggenc's `-q` scale. `Quality::new(q)` returns
  `None` for NaN or for values outside -1..=10. The value is converted
  to libvorbis's `base_quality` exactly as oggenc 1.4.3 does it, with
  `(float)((double)(float)q * 0.1)`. So `Quality::new(9.344)` picks the
  same setup, and the same nominal bitrate in the ID header, as
  `oggenc -q 9.344`. Every quality is available at every rate and
  channel count, as in libvorbis.
- **Sample rate** picks the libvorbis mode template the way
  `vorbis_encode_init_vbr` does, and with it the block sizes and psy
  tuning. `SampleRate::new(hz)` accepts `1..=200_000`, the range libvorbis
  1.3.7 has templates for (it has none above 200 kHz). Constants cover the
  common rates (`SampleRate::HZ_8000` … `HZ_192000`):

  | Rate (Hz)      | libvorbis template | Block sizes (short/long) |
  |----------------|--------------------|--------------------------|
  | 1–7999         | `XX` (`setup_X.h`) | 512/512 |
  | 8000–8999      | `8`                | 512/512 |
  | 9000–14999     | `11`               | 512/512 |
  | 15000–18999    | `16`               | 1024/1024 below q0.5, else 512/1024 |
  | 19000–25999    | `22`               | as `16` |
  | 26000–39999    | `32`               | 512/4096 below q0, else 256/2048 |
  | 40000–50000    | `44`               | as `32` |
  | 50001–200000   | `X` (`setup_X.h`)  | as `32` |

  The `X` and `XX` catch-all templates have no bitrate table, so the ID
  header's nominal bitrate is -1 per channel there, as libvorbis writes it.
- **One quality at low rates:** below 26 kHz, `base_quality` `0x3f7ffffd`
  (oggenc `-q 9.999998`) makes libvorbis 1.3.7 read past the end of its
  mode tables (`j + del` rounds up to the number of settings), so its
  output there is undefined and depends on how the compiler laid out the
  tables. oxivorbis encodes that one request as libvorbis's top setting,
  q10. Every other float matches.
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
- **Partial frames:** `Encoder::write` holds the samples of an
  incomplete trailing frame (an unpaired stereo sample, up to five
  samples of a 5.1 frame) until the next call. `finish` and the one-shot
  functions drop a final partial frame.

## Multichannel and 5.1

`Channels::new(n)` accepts every count libvorbis 1.3.7 encodes, `1..=255`
(more fails in libvorbis with `OV_EINVAL`; the ID header stores the
count in 8 bits). Constants cover `Channels::MONO`, `STEREO`,
`SURROUND_5_1` and `MAX`. Every channel count works at every rate and
quality, so there is still nothing to fail at run time. Which libvorbis
template a request lands on, established by asking the reference
`vorbis_encode_init_vbr` (`tools/oracle-encoder/probe_channels`):

| Channels | Rate | libvorbis template | Coding |
|---|---|---|---|
| 1 | any | the rate's `*_uncoupled` | one floor and residue |
| 2 | any | the rate's `*_stereo` | coupled L/R |
| 6 | 40–70 kHz | `44_51` (`setup_44p51.h`) | 5.1: two submaps; below q5, four coupling steps (L/R, BL/BR, L/C, L/BL); the LFE has its own floor and a residue low-passed at 250 Hz |
| 6 | other rates | the rate's `*_uncoupled` | independent channels |
| 3–5, 7–255 | any | the rate's `*_uncoupled` | independent channels |

**Channel order.** Input is interleaved (or, through the planar
methods, sliced) in **Vorbis channel order**, Vorbis I spec section
4.3.9, and encoded as given: oxivorbis never reorders channels. For 5.1
that is front left, center, front right, rear left, rear right, LFE.

| Channels | Vorbis order |
|---|---|
| 3 | L, C, R |
| 4 | FL, FR, RL, RR |
| 5 | FL, C, FR, RL, RR |
| 6 | FL, C, FR, RL, RR, LFE |
| 7 | FL, C, FR, SL, SR, RC, LFE |
| 8 | FL, C, FR, SL, SR, RL, RR, LFE |

WAV files and most decoders deliver WAV/SMPTE order instead (5.1: L, R,
C, LFE, BL, BR). `Channels::wav_to_vorbis()` gives the permutation for
1 to 8 channels and `reorder_wav_to_vorbis` applies it in place; both
are optional:

```rust
use oxivorbis::{Channels, Options, SampleRate, reorder_wav_to_vorbis};

// One second of 5.1 from a WAV reader: L R C LFE BL BR per frame.
let mut pcm = vec![0i16; 48_000 * 6];
reorder_wav_to_vorbis(&mut pcm, Channels::SURROUND_5_1); // now L C R BL BR LFE
let ogg = oxivorbis::encode(&pcm, SampleRate::HZ_48000, Channels::SURROUND_5_1, &Options::default());
assert_eq!(&ogg[..4], b"OggS");
```

The [command-line encoder](#command-line-encoder) applies this
reordering to WAV input with 3 to 8 channels, as oggenc does, and takes
raw input as Vorbis order.

Encoding time grows linearly with the channel count: 5.1 takes about
2.9× as long as stereo at q5 (see [Performance](#performance)). Streaming
memory is a few hundred KB per channel; the `parallel` feature shrinks
its batches in proportion to the channel count, so a batch never holds
more PCM than a stereo one.

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
44.1 kHz stereo also at q0, q2, q3 and q8. At the other template
families the same corpus is byte-identical at 8 kHz and 11.025 kHz mono
and 16, 22.05, 32 and 96 kHz stereo, at q-1, q5 and q10 (119,754
encodes). A differential fuzzer has run more than 190,000 generated
cases clean at 44.1/48 kHz and more than 400,000 across 4, 8, 11.025,
16, 22.05, 32 and 96 kHz (mono and stereo, 16-bit and f32 input), and
the CI parity suite covers every rate range's edges from 1 Hz to
200 kHz. CI also compares 30 to 120 s of non-silent audio on every push.
With more than two channels, 5.1 versions of the corpus (distinct
content in every channel, rears from a second file, LFE low-passed) are
byte-identical at 44.1 kHz for all 6653 files at q0 and q3 and for 1000
files at 44.1 and 48 kHz at q-1, q5 and q10, as are uncoupled 6 channels
at 32 kHz, 3 at 44.1 kHz and 8 at 48 kHz (28,306 encodes); the fuzzer
has run about 255,000 cases clean at 3 to 12 channels (150,000 of them
5.1 at 40 to 70 kHz), and 255 channels are in the parity suite.
Parity is verified on macOS arm64, Linux x86_64 (glibc) and wasm32
(under wasmtime). oxivorbis's output is also identical
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
platforms. Every value and decision the psychoacoustic setup derives
from them (rounded band edges, comparisons, the stored tables) is
unchanged when each libm result moves by ±1 ulp, at every rate from
1 Hz to 200 kHz and every block size, so the output does not depend on
the platform's libm (`psy_setup_robust_to_libm_ulps`).
The bitrate modes are byte-identical to the oracle set up as oggenc sets
it up. The real-audio corpus is byte-identical at 44.1 kHz stereo with
`--managed -b 128 -m 128 -M 128`, `--managed -b 160`, `-q 5 -M 128` and
`-b 128`, and at 22.05 kHz mono with `--managed -b 40 -m 40 -M 40`,
`--managed -b 32`, `-q 5 -M 40` and `-m 24 -M 48` (53,224 encodes). The
differential fuzzer has run more than 100,000 cases with random bitrate
modes, limits, reservoir and encoder tuning across 8 to 50 kHz (i16 and
f32, macOS arm64 and Linux x86_64, and against an AddressSanitizer build
of the oracle), and CI runs 264 bitrate parity tests per platform.
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
at 256 long blocks (about 6 s of 44.1 kHz audio; proportionally fewer
with more than two channels), with up to three batches in flight. In exchange for the speed, `write` hands back pages
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

Other rates scale with the sample count. On the M4 at q5, the same
track takes 0.10 s at 8 kHz mono (oggenc 0.16 s), 0.45 s at 22.05 kHz
stereo (oggenc 0.72 s) and 1.45 s at 96 kHz stereo (oggenc 2.31 s).

Channels scale linearly. A 5.1 version of the same track (its stereo
as front L/R, center and LFE derived from it, rears from another track)
takes 3.30 s serial and 0.51 s with `parallel` at q5 on the Intel
machine, 2.9× and 2.7× the stereo times for 3× the channels; 3.76 s at
q-1 and 4.22 s at q2, where 5.1 is coupled.

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
