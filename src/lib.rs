//! oxivorbis: a pure-Rust Ogg Vorbis encoder whose audio packets are
//! bit-identical to libvorbis 1.3.7 (VBR mode, oggenc qualities -1..10).
//!
//! Input is interleaved `i16` or `f32` PCM at 44.1 or 48 kHz, mono or
//! stereo. Encode a whole buffer at once with [`encode`] / [`encode_f32`],
//! or stream it through an [`Encoder`] in chunks of any size; both give the
//! same bytes.
//!
//! # One-shot
//!
//! ```
//! use oxivorbis::{Channels, Options, Quality, SampleRate};
//!
//! let pcm = vec![0i16; 44100 * 2]; // 1 s of stereo silence
//! let opts = Options::default()
//!     .with_quality(Quality::new(6.5).unwrap())
//!     .with_comment("TITLE", "Silence");
//! let ogg = oxivorbis::encode(&pcm, SampleRate::Hz44100, Channels::Stereo, &opts);
//! assert_eq!(&ogg[..4], b"OggS");
//! ```
//!
//! # Streaming
//!
//! ```
//! use oxivorbis::{Channels, Encoder, Options, SampleRate};
//!
//! let mut enc = Encoder::new(SampleRate::Hz48000, Channels::Mono, &Options::default());
//! let mut ogg = Vec::new();
//! for _ in 0..10 {
//!     let chunk: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
//!     ogg.extend_from_slice(enc.write_f32(&chunk)); // pages completed so far
//! }
//! ogg.extend_from_slice(&enc.finish());
//! assert_eq!(&ogg[..4], b"OggS");
//! ```
//!
//! # Parity contract
//!
//! The audio packets, granule positions and page layout are bit-identical
//! to libvorbis 1.3.7 (built without FMA contraction) fed the same samples
//! through `vorbis_analysis_buffer` in 64-frame `vorbis_analysis_wrote`
//! chunks. The header packets are identical given the same stream serial,
//! vendor string and comments, which are set through [`Options`].
//!
//! The default vendor string is [`DEFAULT_VENDOR`] (`"oxivorbis <version>"`)
//! and the default serial is [`DEFAULT_SERIAL`], so default output is fully
//! deterministic.
//!
//! # Cargo features
//!
//! - **`parallel`** (off by default): analyze blocks (MDCT, psychoacoustics,
//!   floor, residue) on the current Rayon thread pool. The API and the
//!   output bytes are unchanged; long inputs encode several times faster.
//!   Streaming output trails the input by up to about 16 s of audio.
//!   Encoding from inside a Rayon pool (e.g. one file per `par_iter` item)
//!   is safe. Leave it off for `wasm32` targets; it isn't needed when you
//!   already parallelize across files.
//!
//! The crate has no other public features; `oracle` and `__internal` exist
//! only for this crate's own test suite and are not part of the API.
//!
//! See the [README](https://github.com/opera-emoller/oxivorbis#readme) for
//! performance numbers, and `CONTRIBUTING.md` for how parity is tested.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

// Compile the README's code blocks as doctests so they cannot go stale.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

#[allow(dead_code)]
mod tables;

mod debug_dump;
pub(crate) use debug_dump::debug_flag;

#[allow(dead_code)]
mod codebook;

#[allow(dead_code)]
mod floor1;

#[allow(dead_code)]
mod residue;

// Internals exposed only to this crate's tests and tools (`__internal`
// feature; not part of the public API and not semver-covered).
macro_rules! internal_mod {
    ($name:ident) => {
        #[cfg(feature = "__internal")]
        #[doc(hidden)]
        #[allow(missing_docs)]
        pub mod $name;
        #[cfg(not(feature = "__internal"))]
        #[allow(dead_code)]
        mod $name;
    };
}
internal_mod!(bitpack);
internal_mod!(headers);
internal_mod!(psy);
internal_mod!(mdct);
internal_mod!(ogg_pages);

mod drft;
mod encode;
mod envelope;
mod lpc;
mod mapping0;
mod quality;
mod setup;
mod setup_gen;
mod setup_templates;
mod window;

use std::fmt;

/// Input sample rate.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SampleRate {
    /// 44 100 Hz.
    Hz44100,
    /// 48 000 Hz.
    Hz48000,
}

impl SampleRate {
    /// The rate in Hz.
    pub const fn hz(self) -> u32 {
        match self {
            SampleRate::Hz44100 => 44_100,
            SampleRate::Hz48000 => 48_000,
        }
    }
}

/// Error for a sample rate or channel count oxivorbis does not support.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Unsupported {
    what: &'static str,
    value: u32,
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unsupported {} {}", self.what, self.value)
    }
}

impl std::error::Error for Unsupported {}

impl TryFrom<u32> for SampleRate {
    type Error = Unsupported;
    /// `44100` or `48000`.
    fn try_from(hz: u32) -> Result<Self, Unsupported> {
        match hz {
            44_100 => Ok(SampleRate::Hz44100),
            48_000 => Ok(SampleRate::Hz48000),
            _ => Err(Unsupported {
                what: "sample rate",
                value: hz,
            }),
        }
    }
}

/// Channel layout of the interleaved input.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Channels {
    /// One channel.
    Mono,
    /// Two channels, interleaved left, right.
    Stereo,
}

impl Channels {
    /// Number of channels.
    pub const fn count(self) -> u16 {
        match self {
            Channels::Mono => 1,
            Channels::Stereo => 2,
        }
    }
}

impl TryFrom<u16> for Channels {
    type Error = Unsupported;
    /// `1` or `2`.
    fn try_from(n: u16) -> Result<Self, Unsupported> {
        match n {
            1 => Ok(Channels::Mono),
            2 => Ok(Channels::Stereo),
            _ => Err(Unsupported {
                what: "channel count",
                value: n as u32,
            }),
        }
    }
}

/// Encoding quality on oggenc's `-q` scale: -1.0 ..= 10.0, fractional
/// values allowed. The default is [`Quality::Q5`].
///
/// The value is converted to libvorbis's `base_quality` exactly as oggenc
/// 1.4.3 does (`(float)((double)q * 0.1)`), so `Quality::new(7.5)` picks
/// the same libvorbis setup (and ID-header nominal bitrate) as
/// `oggenc -q 7.5`. Qualities below 0 select libvorbis's q-1 template
/// (512/4096-sample blocks).
#[derive(Copy, Clone, Debug, PartialEq, PartialOrd)]
pub struct Quality {
    /// oggenc `-q` value.
    q: f32,
    /// libvorbis `base_quality` (-0.1..=1.0).
    base: f32,
}

impl Quality {
    /// Quality 5, oggenc's default.
    pub const Q5: Quality = Quality { q: 5.0, base: 0.5 };

    /// `q` on oggenc's `-q` scale. Returns `None` for NaN or anything
    /// outside -1.0..=10.0.
    pub fn new(q: f32) -> Option<Quality> {
        if (-1.0..=10.0).contains(&q) {
            Some(Quality {
                q,
                base: crate::encode::oggenc_base_quality(q),
            })
        } else {
            None
        }
    }

    /// The oggenc-scale value this quality was created from.
    pub fn get(self) -> f32 {
        self.q
    }

    /// Quality from an exact libvorbis `base_quality` (-0.1..=1.0), for
    /// parity tests that drive the oracle with that float.
    #[cfg(feature = "__internal")]
    #[doc(hidden)]
    pub fn from_base_quality(base: f32) -> Option<Quality> {
        if (crate::quality::MIN_BASE_QUALITY..=1.0).contains(&base) {
            Some(Quality {
                q: base * 10.0,
                base,
            })
        } else {
            None
        }
    }

    pub(crate) fn base(self) -> f32 {
        self.base
    }
}

impl Default for Quality {
    fn default() -> Self {
        Quality::Q5
    }
}

/// Stream serial used when [`Options::serial`] is `None`.
pub const DEFAULT_SERIAL: u32 = 0x6C65_7774; // "lewt"

/// Vendor string used when [`Options::vendor`] is `None`.
pub const DEFAULT_VENDOR: &str = concat!("oxivorbis ", env!("CARGO_PKG_VERSION"));

/// Encoder settings.
///
/// `Options::default()` is quality 5, serial [`DEFAULT_SERIAL`], vendor
/// [`DEFAULT_VENDOR`] and no comments, so output is deterministic. Build
/// it with the `with_*` setters or by assigning fields:
///
/// ```
/// use oxivorbis::{Options, Quality};
///
/// let opts = Options::default()
///     .with_quality(Quality::new(3.0).unwrap())
///     .with_serial(42)
///     .with_comment("ARTIST", "Someone");
///
/// let mut opts2 = Options::default();
/// opts2.quality = Quality::new(3.0).unwrap();
/// ```
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Options {
    /// Encoding quality.
    pub quality: Quality,
    /// Ogg stream serial number; `None` = [`DEFAULT_SERIAL`]. Streams
    /// that are chained or multiplexed together need distinct serials.
    pub serial: Option<u32>,
    /// Comment-header vendor string; `None` = [`DEFAULT_VENDOR`]. Written
    /// as given (any UTF-8); a vendor string or comment longer than
    /// `u32::MAX` bytes is truncated to that length.
    pub vendor: Option<String>,
    /// User comments (`("TITLE", "...")` etc.), written to the comment
    /// header in order as `KEY=value`.
    ///
    /// The Vorbis spec allows only ASCII 0x20..=0x7D other than `=` in
    /// keys, so any other byte is dropped from the key: `"A=B"` becomes
    /// `"AB"`, `"TÍTLE"` becomes `"TTLE"`. A comment whose key is left
    /// empty (including an empty key) is skipped. Values are written as
    /// given and may contain `=` and any UTF-8.
    pub comments: Vec<(String, String)>,
}

impl Options {
    /// Same as [`Options::default`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the quality.
    pub fn with_quality(mut self, quality: Quality) -> Self {
        self.quality = quality;
        self
    }

    /// Set the Ogg stream serial number.
    pub fn with_serial(mut self, serial: u32) -> Self {
        self.serial = Some(serial);
        self
    }

    /// Set the comment-header vendor string.
    pub fn with_vendor(mut self, vendor: impl Into<String>) -> Self {
        self.vendor = Some(vendor.into());
        self
    }

    /// Append a user comment `key=value`. Bytes the Vorbis spec doesn't
    /// allow in a key (anything outside ASCII 0x20..=0x7D, and `=`) are
    /// dropped from `key`, and the comment is skipped if no key is left;
    /// see [`Options::comments`].
    ///
    /// ```
    /// # use oxivorbis::Options;
    /// let opts = Options::default()
    ///     .with_comment("TITLE", "a=b")  // TITLE=a=b
    ///     .with_comment("A=B", "v")      // written as AB=v
    ///     .with_comment("", "skipped");  // not written
    /// ```
    pub fn with_comment(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.comments.push((key.into(), value.into()));
        self
    }

    fn core(&self, rate: SampleRate, channels: Channels) -> encode::EncoderCore {
        let vendor = self.vendor.as_deref().unwrap_or(DEFAULT_VENDOR);
        let comments: Vec<Vec<u8>> = self
            .comments
            .iter()
            .filter_map(|(k, v)| comment_bytes(k, v))
            .collect();
        encode::EncoderCore::new(
            rate,
            channels,
            self.quality.base(),
            self.serial.unwrap_or(DEFAULT_SERIAL),
            vendor.as_bytes(),
            &comments,
        )
    }
}

/// `KEY=value` bytes for one user comment, with the key reduced to what
/// the Vorbis comment spec allows (ASCII 0x20..=0x7D except `=`); `None`
/// if no key byte survives.
fn comment_bytes(key: &str, value: &str) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = key
        .bytes()
        .filter(|&b| (0x20..=0x7D).contains(&b) && b != b'=')
        .collect();
    if out.is_empty() {
        return None;
    }
    out.push(b'=');
    out.extend_from_slice(value.as_bytes());
    Some(out)
}

/// Streaming Ogg Vorbis encoder.
///
/// Feed interleaved PCM in chunks of any size with [`write`](Self::write)
/// (`i16`) or [`write_f32`](Self::write_f32) (`f32`), collect the Ogg pages
/// each call completes, and call [`finish`](Self::finish) for the rest of
/// the stream. Memory use is bounded (a few blocks of PCM) regardless of
/// input length.
///
/// The output does not depend on how the input is split across `write`
/// calls: any chunking, including chunks that split a stereo frame, gives
/// the same bytes as the one-shot [`encode`] / [`encode_f32`].
///
/// ```
/// use oxivorbis::{Channels, Encoder, Options, SampleRate};
///
/// let mut enc = Encoder::new(SampleRate::Hz44100, Channels::Stereo, &Options::default());
/// let mut ogg = Vec::new();
/// for chunk in vec![0i16; 44100 * 2].chunks(4097) {
///     ogg.extend_from_slice(enc.write(chunk));
/// }
/// ogg.extend_from_slice(&enc.finish());
/// let pcm = vec![0i16; 44100 * 2];
/// let one_shot = oxivorbis::encode(&pcm, SampleRate::Hz44100, Channels::Stereo, &Options::default());
/// assert_eq!(ogg, one_shot);
/// ```
pub struct Encoder {
    core: Box<encode::EncoderCore>,
}

// `Encoder` and `Options` are `Send + Sync` with and without `parallel`
// (features are additive, so this must not depend on them).
const _: () = {
    const fn send_sync<T: Send + Sync>() {}
    send_sync::<Encoder>();
    send_sync::<Options>();
};

impl Encoder {
    /// New encoder for `rate` / `channels` with the given options. The
    /// header pages are produced right away and returned by the first
    /// `write` (or by `finish`).
    pub fn new(rate: SampleRate, channels: Channels, options: &Options) -> Self {
        Self {
            core: Box::new(options.core(rate, channels)),
        }
    }

    /// Encode interleaved 16-bit samples. Returns the Ogg bytes (whole
    /// pages) completed by this call, which may be empty; the slice is
    /// valid until the next call.
    ///
    /// Samples are scaled by `1/32768`, as libvorbis-based tools do. The
    /// slice need not hold whole frames: a trailing unpaired stereo sample
    /// is carried over and paired with the first sample of the next call.
    pub fn write(&mut self, interleaved: &[i16]) -> &[u8] {
        self.core.begin_write();
        self.core.feed(interleaved, encode::i16_to_f32);
        self.core.take_output()
    }

    /// Encode interleaved `f32` samples, nominally in `-1.0..=1.0`. Returns
    /// the Ogg bytes completed by this call, like [`write`](Self::write),
    /// and carries a trailing unpaired stereo sample over the same way.
    ///
    /// Samples reach the encoder unscaled, exactly as if written to
    /// libvorbis's `vorbis_analysis_buffer`, and the output matches
    /// libvorbis for finite input, including values outside ±1.0,
    /// subnormals and `-0.0` (parity is tested up to ±10⁴; at absurd
    /// magnitudes libvorbis's own float-to-int conversions are undefined,
    /// but oxivorbis still produces a valid stream). Non-finite samples
    /// (NaN, ±infinity), which make libvorbis emit a corrupt stream, are
    /// encoded as `0.0`.
    ///
    /// `write` and `write_f32` may be mixed; both feed the same stream.
    pub fn write_f32(&mut self, interleaved: &[f32]) -> &[u8] {
        self.core.begin_write();
        self.core.feed(interleaved, encode::f32_sample);
        self.core.take_output()
    }

    /// Flush the remaining input, end the stream, and return the Ogg bytes
    /// not yet returned by `write`. A stereo sample still waiting for its
    /// pair is dropped.
    pub fn finish(self) -> Vec<u8> {
        self.core.finish()
    }

    /// Frames currently held in the analysis buffer. Diagnostic for the
    /// bounded-memory tests.
    #[cfg(feature = "__internal")]
    #[doc(hidden)]
    pub fn buffered_frames(&self) -> usize {
        self.core.buffered_frames()
    }

    /// Blocks analyzed per parallel batch (default 32 x Rayon threads,
    /// at most 256 long blocks of 2048 or 128 of 4096). Output does not
    /// depend on it; exposed for tests.
    #[doc(hidden)]
    #[cfg(all(feature = "parallel", feature = "__internal"))]
    pub fn set_parallel_batch(&mut self, blocks: usize) {
        self.core.set_batch(blocks);
    }
}

/// Encode interleaved `i16` PCM into a complete Ogg Vorbis stream.
///
/// Same bytes as streaming the input through an [`Encoder`]. Samples are
/// scaled by `1/32768`. In stereo input with an odd number of samples the
/// last, unpaired sample is ignored.
pub fn encode(pcm: &[i16], rate: SampleRate, channels: Channels, options: &Options) -> Vec<u8> {
    let mut core = options.core(rate, channels);
    core.feed(pcm, encode::i16_to_f32);
    core.finish()
}

/// Encode interleaved `f32` PCM (nominally `-1.0..=1.0`) into a complete
/// Ogg Vorbis stream. See [`Encoder::write_f32`] for how samples are
/// interpreted; in stereo input with an odd number of samples the last,
/// unpaired sample is ignored.
pub fn encode_f32(pcm: &[f32], rate: SampleRate, channels: Channels, options: &Options) -> Vec<u8> {
    let mut core = options.core(rate, channels);
    core.feed(pcm, encode::f32_sample);
    core.finish()
}

// ---------------------------------------------------------------------------
// Test entry points (`__internal` feature): exact libvorbis base_quality and
// raw comment-header strings, as the parity tests copy them from the oracle.
// ---------------------------------------------------------------------------

#[cfg(feature = "__internal")]
fn internal_core(
    rate: SampleRate,
    channels: Channels,
    base_quality: f32,
    serial: u32,
    vendor: Option<&[u8]>,
    encoder_tag: Option<&[u8]>,
) -> encode::EncoderCore {
    let comments: Vec<Vec<u8>> = encoder_tag
        .filter(|t| !t.is_empty())
        .map(|t| t.to_vec())
        .into_iter()
        .collect();
    encode::EncoderCore::new(
        rate,
        channels,
        base_quality,
        serial,
        vendor.unwrap_or(DEFAULT_VENDOR.as_bytes()),
        &comments,
    )
}

/// One-shot encode at libvorbis `base_quality` with the given serial,
/// vendor (`None` = default) and single raw comment (`None` or empty =
/// none).
#[cfg(feature = "__internal")]
#[doc(hidden)]
pub fn encode_with_base_quality_and_meta(
    samples: &[i16],
    rate: SampleRate,
    channels: Channels,
    base_quality: f32,
    serial: u32,
    vendor: Option<&[u8]>,
    encoder_tag: Option<&[u8]>,
) -> Vec<u8> {
    let mut core = internal_core(rate, channels, base_quality, serial, vendor, encoder_tag);
    core.feed(samples, encode::i16_to_f32);
    core.finish()
}

/// f32 counterpart of [`encode_with_base_quality_and_meta`].
#[cfg(feature = "__internal")]
#[doc(hidden)]
pub fn encode_f32_with_base_quality_and_meta(
    samples: &[f32],
    rate: SampleRate,
    channels: Channels,
    base_quality: f32,
    serial: u32,
    vendor: Option<&[u8]>,
    encoder_tag: Option<&[u8]>,
) -> Vec<u8> {
    let mut core = internal_core(rate, channels, base_quality, serial, vendor, encoder_tag);
    core.feed(samples, encode::f32_sample);
    core.finish()
}

/// Q5 [`encode_with_base_quality_and_meta`].
#[cfg(feature = "__internal")]
#[doc(hidden)]
pub fn encode_with_serial_and_meta(
    samples: &[i16],
    rate: SampleRate,
    channels: Channels,
    serial: u32,
    vendor: Option<&[u8]>,
    encoder_tag: Option<&[u8]>,
) -> Vec<u8> {
    encode_with_base_quality_and_meta(samples, rate, channels, 0.5, serial, vendor, encoder_tag)
}

/// Q5 encode with default vendor, no comments, and the given serial.
#[cfg(feature = "__internal")]
#[doc(hidden)]
pub fn encode_with_serial(
    samples: &[i16],
    rate: SampleRate,
    channels: Channels,
    serial: u32,
) -> Vec<u8> {
    encode_with_serial_and_meta(samples, rate, channels, serial, None, None)
}

/// Streaming counterpart of [`encode_with_base_quality_and_meta`].
#[cfg(feature = "__internal")]
#[doc(hidden)]
pub fn encoder_with_base_quality_and_meta(
    rate: SampleRate,
    channels: Channels,
    base_quality: f32,
    serial: u32,
    vendor: Option<&[u8]>,
    encoder_tag: Option<&[u8]>,
) -> Encoder {
    Encoder {
        core: Box::new(internal_core(
            rate,
            channels,
            base_quality,
            serial,
            vendor,
            encoder_tag,
        )),
    }
}

/// oggenc `-q` -> libvorbis `base_quality`, bit for bit as vorbis-tools
/// 1.4.3: `((q as f32) as f64 * 0.1) as f32`, with `q > 10` clamped to 10.
#[cfg(feature = "__internal")]
#[doc(hidden)]
pub fn oggenc_base_quality(quality: f32) -> f32 {
    encode::oggenc_base_quality(quality)
}
