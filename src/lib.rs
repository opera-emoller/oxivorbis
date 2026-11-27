//! oxivorbis: a pure-Rust Ogg Vorbis encoder whose audio packets are
//! bit-identical to libvorbis 1.3.7: VBR at oggenc qualities -1..10, and
//! libvorbis's bitrate management (average bitrate, hard limits, quality
//! with limits; see [`BitrateMode`]).
//!
//! Input is interleaved `i16` or `f32` PCM at any rate from 1 Hz to
//! 200 kHz, with 1 to 255 channels in Vorbis channel order (see
//! [`Channels`]; 5.1 is coupled the way libvorbis does it). Encode a whole
//! buffer at once with [`encode`] / [`encode_f32`],
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
//! let ogg = oxivorbis::encode(&pcm, SampleRate::HZ_44100, Channels::STEREO, &opts);
//! assert_eq!(&ogg[..4], b"OggS");
//! ```
//!
//! # Streaming
//!
//! ```
//! use oxivorbis::{Channels, Encoder, Options, SampleRate};
//!
//! let mut enc = Encoder::new(SampleRate::HZ_48000, Channels::MONO, &Options::default());
//! let mut ogg = Vec::new();
//! for _ in 0..10 {
//!     let chunk: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
//!     ogg.extend_from_slice(enc.write_f32(&chunk)); // pages completed so far
//! }
//! ogg.extend_from_slice(&enc.finish());
//! assert_eq!(&ogg[..4], b"OggS");
//! ```
//!
//! # Bitrate modes
//!
//! [`Options::with_bitrate`] replaces VBR with one of the setups oggenc's
//! `-b`, `-m`, `-M` and `--managed` give libvorbis. It checks the request
//! against libvorbis's templates for the input's rate and channel count,
//! so encoding stays infallible:
//!
//! ```
//! use oxivorbis::{BitrateMode, Channels, Options, SampleRate};
//!
//! // oggenc --managed -b 96 (average bitrate)
//! let opts = Options::default()
//!     .with_bitrate(SampleRate::HZ_48000, Channels::STEREO, BitrateMode::average(96))
//!     .expect("96 kb/s is in libvorbis's range for 48 kHz stereo");
//! let ogg = oxivorbis::encode(&vec![0i16; 48_000 * 2], SampleRate::HZ_48000, Channels::STEREO, &opts);
//! assert_eq!(&ogg[..4], b"OggS");
//! ```
//!
//! # Writing to files and streams
//!
//! [`OggWriter`] owns an [`Encoder`] and any [`std::io::Write`] sink and
//! writes pages to it as they complete; [`S16LeWriter`] does the same for
//! raw s16le PCM bytes, through `std::io::Write`. Planar input (one slice
//! per channel) goes through [`Encoder::write_planar_f32`] /
//! [`Encoder::write_planar_i16`] or the `OggWriter` equivalents.
//!
//! ```
//! use oxivorbis::{Channels, OggWriter, Options, SampleRate};
//!
//! let (left, right) = (vec![0.0f32; 48_000], vec![0.0f32; 48_000]);
//! let mut w = OggWriter::new(Vec::new(), SampleRate::HZ_48000, Channels::STEREO, &Options::default())?;
//! w.write_planar_f32(&[&left, &right])?;
//! let ogg: Vec<u8> = w.finish()?; // or a File, socket, ...
//! assert_eq!(&ogg[..4], b"OggS");
//! # Ok::<(), std::io::Error>(())
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

mod bitrate;
mod drft;
mod encode;
mod envelope;
mod lpc;
mod mapping0;
mod mapping0_managed;
mod quality;
mod ratecontrol;
mod setup;
mod setup_gen;
mod setup_templates;
mod window;

mod planar;
mod writer;
pub use writer::{OggWriter, S16LeWriter};

pub use ratecontrol::{BitrateError, BitrateMode, BitrateTuning, EncoderTuning};

use std::fmt;

/// Input sample rate: any rate from 1 Hz to 200 kHz.
///
/// libvorbis 1.3.7 has an encoder template for every rate in
/// `1..=200_000` Hz, for every channel count, at every quality from -1 to 10, so
/// every [`SampleRate`] encodes with every [`Quality`] and [`Channels`].
/// The rate selects the libvorbis template (`modes/setup_*.h`) exactly as
/// libvorbis does, and with it the block sizes:
///
/// | Rate (Hz)       | Template | Block sizes (short/long)                  |
/// |-----------------|----------|-------------------------------------------|
/// | 1 – 7999        | `XX`     | 512/512                                   |
/// | 8000 – 8999     | `8`      | 512/512                                   |
/// | 9000 – 14999    | `11`     | 512/512                                   |
/// | 15000 – 18999   | `16`     | 1024/1024 below q0.5, 512/1024 from q0.5  |
/// | 19000 – 25999   | `22`     | as `16`                                   |
/// | 26000 – 39999   | `32`     | 512/4096 below q0, 256/2048 from q0       |
/// | 40000 – 50000   | `44`     | as `32`                                   |
/// | 50001 – 200000  | `X`      | as `32`                                   |
///
/// Rates above 200 kHz have no libvorbis template (`vorbis_encode_init_vbr`
/// fails with `OV_EIMPL`), and 0 Hz is rejected (`OV_EINVAL`).
///
/// ```
/// use oxivorbis::SampleRate;
///
/// assert_eq!(SampleRate::HZ_44100.hz(), 44_100);
/// assert_eq!(SampleRate::new(22_050), Some(SampleRate::HZ_22050));
/// assert!(SampleRate::new(37_800).is_some());
/// assert!(SampleRate::new(0).is_none());
/// assert!(SampleRate::new(200_001).is_none());
/// assert!(SampleRate::try_from(384_000u32).is_err());
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SampleRate(u32);

impl SampleRate {
    /// Lowest supported rate, 1 Hz.
    pub const MIN: SampleRate = SampleRate(crate::quality::MIN_RATE);
    /// Highest supported rate, 200 kHz (libvorbis has no template above).
    pub const MAX: SampleRate = SampleRate(crate::quality::MAX_RATE);

    /// 8 000 Hz.
    pub const HZ_8000: SampleRate = SampleRate(8_000);
    /// 11 025 Hz.
    pub const HZ_11025: SampleRate = SampleRate(11_025);
    /// 16 000 Hz.
    pub const HZ_16000: SampleRate = SampleRate(16_000);
    /// 22 050 Hz.
    pub const HZ_22050: SampleRate = SampleRate(22_050);
    /// 32 000 Hz.
    pub const HZ_32000: SampleRate = SampleRate(32_000);
    /// 44 100 Hz.
    pub const HZ_44100: SampleRate = SampleRate(44_100);
    /// 48 000 Hz.
    pub const HZ_48000: SampleRate = SampleRate(48_000);
    /// 88 200 Hz.
    pub const HZ_88200: SampleRate = SampleRate(88_200);
    /// 96 000 Hz.
    pub const HZ_96000: SampleRate = SampleRate(96_000);
    /// 192 000 Hz.
    pub const HZ_192000: SampleRate = SampleRate(192_000);

    /// `hz` if it is in `1..=200_000`, else `None`.
    pub const fn new(hz: u32) -> Option<SampleRate> {
        if hz >= crate::quality::MIN_RATE && hz <= crate::quality::MAX_RATE {
            Some(SampleRate(hz))
        } else {
            None
        }
    }

    /// The rate in Hz.
    pub const fn hz(self) -> u32 {
        self.0
    }
}

impl fmt::Display for SampleRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} Hz", self.0)
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
    /// Any rate in `1..=200_000` Hz (see [`SampleRate::new`]).
    fn try_from(hz: u32) -> Result<Self, Unsupported> {
        SampleRate::new(hz).ok_or(Unsupported {
            what: "sample rate",
            value: hz,
        })
    }
}

impl From<SampleRate> for u32 {
    fn from(rate: SampleRate) -> u32 {
        rate.hz()
    }
}

/// Number of interleaved input channels: any count from 1 to 255.
///
/// libvorbis 1.3.7 encodes every channel count in `1..=255` at every
/// [`SampleRate`] and [`Quality`], so every `Channels` value encodes. The
/// count selects the libvorbis mode template together with the rate:
///
/// | Channels | Rate (Hz)       | libvorbis template                         |
/// |----------|-----------------|--------------------------------------------|
/// | 2        | any             | the rate's `*_stereo` template (coupled)   |
/// | 6        | 40000 – 70000   | `44_51`: 5.1 surround, coupled, LFE coded separately |
/// | 6        | any other rate  | the rate's `*_uncoupled` template          |
/// | 1, 3–5, 7–255 | any        | the rate's `*_uncoupled` template          |
///
/// Uncoupled channels are encoded independently, each with its own floor
/// and residue (the residue books are shared). More than 255 channels
/// fails in libvorbis (`OV_EINVAL`; the Vorbis ID header stores the count
/// in 8 bits) and is rejected here.
///
/// # Channel order
///
/// Input is interleaved in **Vorbis channel order** (Vorbis I
/// specification, section 4.3.9) and is encoded in the order given;
/// oxivorbis does no reordering. For 1–8 channels the order is:
///
/// | Channels | Order |
/// |----------|-------|
/// | 1 | mono |
/// | 2 | left, right |
/// | 3 | left, center, right |
/// | 4 | front left, front right, rear left, rear right |
/// | 5 | front left, center, front right, rear left, rear right |
/// | 6 | front left, center, front right, rear left, rear right, LFE |
/// | 7 | front left, center, front right, side left, side right, rear center, LFE |
/// | 8 | front left, center, front right, side left, side right, rear left, rear right, LFE |
///
/// The order of 9 or more channels is application defined. WAV files and
/// most decoders use the WAV/SMPTE order instead (5.1: L, R, C, LFE, BL,
/// BR); [`Channels::wav_to_vorbis`] gives the permutation and
/// [`reorder_wav_to_vorbis`] applies it.
///
/// ```
/// use oxivorbis::Channels;
///
/// assert_eq!(Channels::STEREO.count(), 2);
/// assert_eq!(Channels::new(6), Some(Channels::SURROUND_5_1));
/// assert!(Channels::new(255).is_some());
/// assert!(Channels::new(0).is_none());
/// assert!(Channels::new(256).is_none());
/// assert!(Channels::try_from(300u16).is_err());
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Channels(u8);

impl Channels {
    /// One channel.
    pub const MONO: Channels = Channels(1);
    /// Two channels: left, right (coupled).
    pub const STEREO: Channels = Channels(2);
    /// Six channels, 5.1 surround: front left, center, front right, rear
    /// left, rear right, LFE (Vorbis order). Coupled with libvorbis's 5.1
    /// template at 40–70 kHz, uncoupled at other rates.
    pub const SURROUND_5_1: Channels = Channels(6);
    /// The most channels libvorbis encodes, 255.
    pub const MAX: Channels = Channels(u8::MAX);

    /// `n` channels if `n` is in `1..=255`, else `None`.
    pub const fn new(n: u16) -> Option<Channels> {
        if n >= 1 && n <= u8::MAX as u16 {
            Some(Channels(n as u8))
        } else {
            None
        }
    }

    /// Number of channels.
    pub const fn count(self) -> u16 {
        self.0 as u16
    }

    /// For each Vorbis-order output channel, the index of the same speaker
    /// in WAV/SMPTE order (`WAVE_FORMAT_EXTENSIBLE`'s default channel
    /// masks, which most decoders also use), for 1 to 8 channels; `None`
    /// for more, where neither order is defined. The same table as
    /// oggenc's WAV reader:
    ///
    /// | Channels | WAV order | Map |
    /// |---|---|---|
    /// | 3 | L R C | `[0, 2, 1]` |
    /// | 4 | FL FR BL BR | identity |
    /// | 5 | FL FR C BL BR | `[0, 2, 1, 3, 4]` |
    /// | 6 | FL FR C LFE BL BR | `[0, 2, 1, 4, 5, 3]` |
    /// | 7 | FL FR C LFE BC SL SR | `[0, 2, 1, 5, 6, 4, 3]` |
    /// | 8 | FL FR C LFE BL BR SL SR | `[0, 2, 1, 6, 7, 4, 5, 3]` |
    ///
    /// ```
    /// use oxivorbis::Channels;
    ///
    /// // WAV 5.1 is L R C LFE BL BR; Vorbis 5.1 is L C R BL BR LFE.
    /// assert_eq!(Channels::SURROUND_5_1.wav_to_vorbis(), Some(&[0, 2, 1, 4, 5, 3][..]));
    /// assert_eq!(Channels::STEREO.wav_to_vorbis(), Some(&[0, 1][..]));
    /// assert_eq!(Channels::new(9).unwrap().wav_to_vorbis(), None);
    /// ```
    pub const fn wav_to_vorbis(self) -> Option<&'static [usize]> {
        match self.0 {
            1 => Some(&[0]),
            2 => Some(&[0, 1]),
            3 => Some(&[0, 2, 1]),
            4 => Some(&[0, 1, 2, 3]),
            5 => Some(&[0, 2, 1, 3, 4]),
            6 => Some(&[0, 2, 1, 4, 5, 3]),
            7 => Some(&[0, 2, 1, 5, 6, 4, 3]),
            8 => Some(&[0, 2, 1, 6, 7, 4, 5, 3]),
            _ => None,
        }
    }
}

impl fmt::Display for Channels {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            1 => f.write_str("1 channel"),
            n => write!(f, "{n} channels"),
        }
    }
}

impl TryFrom<u16> for Channels {
    type Error = Unsupported;
    /// Any count in `1..=255` (see [`Channels::new`]).
    fn try_from(n: u16) -> Result<Self, Unsupported> {
        Channels::new(n).ok_or(Unsupported {
            what: "channel count",
            value: n as u32,
        })
    }
}

impl From<Channels> for u16 {
    fn from(channels: Channels) -> u16 {
        channels.count()
    }
}

/// Reorder interleaved frames in place from WAV/SMPTE channel order to
/// Vorbis order (see [`Channels::wav_to_vorbis`]), for input from a WAV
/// file or a decoder that uses WAV order. Returns `false`, leaving the
/// samples untouched, for more than 8 channels, where there is no
/// standard mapping. A trailing partial frame is left as is.
///
/// Optional: oxivorbis encodes channels in the order given and never
/// reorders on its own.
///
/// ```
/// use oxivorbis::{Channels, reorder_wav_to_vorbis};
///
/// // One 5.1 frame in WAV order: L R C LFE BL BR.
/// let mut frame = ["L", "R", "C", "LFE", "BL", "BR"];
/// assert!(reorder_wav_to_vorbis(&mut frame, Channels::SURROUND_5_1));
/// assert_eq!(frame, ["L", "C", "R", "BL", "BR", "LFE"]);
/// ```
pub fn reorder_wav_to_vorbis<T: Copy>(interleaved: &mut [T], channels: Channels) -> bool {
    let Some(map) = channels.wav_to_vorbis() else {
        return false;
    };
    if map.iter().enumerate().all(|(i, &m)| i == m) {
        return true;
    }
    let ch = map.len();
    for frame in interleaved.chunks_exact_mut(ch) {
        let mut tmp = [frame[0]; 8];
        tmp[..ch].copy_from_slice(frame);
        for (o, &m) in frame.iter_mut().zip(map) {
            *o = tmp[m];
        }
    }
    true
}

/// Encoding quality on oggenc's `-q` scale: -1.0 ..= 10.0, fractional
/// values allowed. The default is [`Quality::Q5`].
///
/// The value is converted to libvorbis's `base_quality` exactly as oggenc
/// 1.4.3 does (`(float)((double)q * 0.1)`), so `Quality::new(7.5)` picks
/// the same libvorbis setup (and ID-header nominal bitrate) as
/// `oggenc -q 7.5`. Every quality works at every [`SampleRate`]; how the
/// rate's template maps it onto block sizes is listed there (e.g. qualities
/// below 0 use 512/4096-sample blocks at 26 kHz and up).
///
/// One quality differs from libvorbis: below 26 kHz, `base_quality`
/// `0x3f7ffffd` (oggenc `-q 9.999998`) makes libvorbis 1.3.7 read past
/// the end of its mode tables, so its output there is undefined.
/// oxivorbis encodes that request as libvorbis's own top setting (q10).
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
    /// Bitrate-driven mode, set by [`Options::with_bitrate`]; `None` is
    /// VBR at `quality`.
    bitrate: Option<ratecontrol::BitrateRequest>,
    /// libvorbis's lowpass / impulse / coupling controls.
    encoder_tuning: EncoderTuning,
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

    /// Encode with a bitrate-driven [`BitrateMode`] (oggenc's `-b`, `-m`,
    /// `-M` and `--managed`) instead of plain VBR, for input at `rate` and
    /// `channels`.
    ///
    /// Returns an error if libvorbis 1.3.7 can't encode the request: it has
    /// no setup for the bitrate at this rate and channel count (bitrate
    /// modes only exist from 8 kHz to 50 kHz; see
    /// [`BitrateMode::supported_kbps`]), or the request is inconsistent
    /// (such as a minimum above the maximum). Encoding then never fails.
    ///
    /// The options remember `rate` and `channels`. Encoding other input
    /// with them re-checks the request for that input's rate and channel
    /// count, and panics if libvorbis can't encode it there.
    ///
    /// [`BitrateMode::QualityLimits`] uses [`Options::quality`] at the time
    /// of encoding.
    ///
    /// ```
    /// use oxivorbis::{BitrateError, BitrateMode, Channels, Options, SampleRate};
    ///
    /// let opts = Options::default()
    ///     .with_bitrate(SampleRate::HZ_44100, Channels::STEREO, BitrateMode::average(160))?;
    /// let pcm = vec![0i16; 44_100 * 2];
    /// let ogg = oxivorbis::encode(&pcm, SampleRate::HZ_44100, Channels::STEREO, &opts);
    /// assert_eq!(&ogg[..4], b"OggS");
    ///
    /// // 8 kb/s is below libvorbis's range for 44.1 kHz stereo.
    /// let low = Options::default()
    ///     .with_bitrate(SampleRate::HZ_44100, Channels::STEREO, BitrateMode::average(8));
    /// assert!(matches!(low, Err(BitrateError::OutOfRange { .. })));
    /// # Ok::<(), BitrateError>(())
    /// ```
    pub fn with_bitrate(
        self,
        rate: SampleRate,
        channels: Channels,
        mode: BitrateMode,
    ) -> Result<Self, BitrateError> {
        self.with_bitrate_tuning(rate, channels, mode, BitrateTuning::default())
    }

    /// [`Options::with_bitrate`] with the bit reservoir tuned as oggenc's
    /// `--advanced-encode-option`s do (see [`BitrateTuning`]).
    ///
    /// ```
    /// use oxivorbis::{BitrateMode, BitrateTuning, Channels, Options, SampleRate};
    ///
    /// // oggenc --managed -b 96 --advanced-encode-option bit_reservoir_bias=0.5
    /// let tuning = BitrateTuning::default().with_reservoir_bias(0.5);
    /// let opts = Options::default()
    ///     .with_bitrate_tuning(SampleRate::HZ_22050, Channels::MONO, BitrateMode::average(48), tuning)
    ///     .unwrap();
    /// # let _ = opts;
    /// ```
    pub fn with_bitrate_tuning(
        mut self,
        rate: SampleRate,
        channels: Channels,
        mode: BitrateMode,
        tuning: BitrateTuning,
    ) -> Result<Self, BitrateError> {
        ratecontrol::plan(&mode, &tuning, self.quality, rate, channels)?;
        self.bitrate = Some(ratecontrol::BitrateRequest {
            mode,
            tuning,
            rate,
            channels,
        });
        Ok(self)
    }

    /// Set libvorbis's lowpass, impulse noise tuning and coupling controls
    /// (oggenc's `lowpass_frequency`, `impulse_noisetune` and
    /// `disable_coupling` advanced options); see [`EncoderTuning`].
    ///
    /// ```
    /// use oxivorbis::{EncoderTuning, Options};
    ///
    /// // oggenc -q 4 --advanced-encode-option lowpass_frequency=15
    /// let opts = Options::default()
    ///     .with_quality(oxivorbis::Quality::new(4.0).unwrap())
    ///     .with_encoder_tuning(EncoderTuning::default().with_lowpass_khz(15.0));
    /// # let _ = opts;
    /// ```
    pub fn with_encoder_tuning(mut self, tuning: EncoderTuning) -> Self {
        self.encoder_tuning = tuning;
        self
    }

    /// The controls set by [`Options::with_encoder_tuning`].
    pub fn encoder_tuning(&self) -> EncoderTuning {
        self.encoder_tuning
    }

    /// Back to VBR at [`Options::quality`].
    pub fn without_bitrate(mut self) -> Self {
        self.bitrate = None;
        self
    }

    /// The bitrate mode set by [`Options::with_bitrate`], if any.
    pub fn bitrate(&self) -> Option<BitrateMode> {
        self.bitrate.as_ref().map(|b| b.mode)
    }

    /// The bitrate tuning set by [`Options::with_bitrate_tuning`] (the
    /// default for plain [`Options::with_bitrate`]), if a bitrate mode is set.
    pub fn bitrate_tuning(&self) -> Option<BitrateTuning> {
        self.bitrate.as_ref().map(|b| b.tuning)
    }

    fn core(&self, rate: SampleRate, channels: Channels) -> encode::EncoderCore {
        let vendor = self.vendor.as_deref().unwrap_or(DEFAULT_VENDOR);
        let comments: Vec<Vec<u8>> = self
            .comments
            .iter()
            .filter_map(|(k, v)| comment_bytes(k, v))
            .collect();
        let serial = self.serial.unwrap_or(DEFAULT_SERIAL);
        let (hz, ch) = (rate.hz(), channels.count() as usize);
        let Some(req) = &self.bitrate else {
            if self.encoder_tuning == EncoderTuning::default() {
                return encode::EncoderCore::new(
                    rate,
                    channels,
                    self.quality.base(),
                    serial,
                    vendor.as_bytes(),
                    &comments,
                );
            }
            let q = self.quality.base();
            let plan =
                ratecontrol::RatePlan::vbr(q, hz, ch).tune(&self.encoder_tuning, Some(q), hz, ch);
            return encode::EncoderCore::with_plan(
                rate,
                channels,
                &plan,
                serial,
                vendor.as_bytes(),
                &comments,
            );
        };
        let plan = ratecontrol::plan(&req.mode, &req.tuning, self.quality, rate, channels)
            .map(|p| p.tune(&self.encoder_tuning, None, hz, ch))
            .unwrap_or_else(|e| {
                // A request validated for this rate and channel count can't
                // fail here (QualityLimits only depends on the quality's
                // template family, i.e. on the rate).
                panic!(
                    "Options::with_bitrate was checked for {} {:?}, but the input is {rate} {channels:?}: {e}",
                    req.rate, req.channels
                )
            });
        encode::EncoderCore::with_plan(rate, channels, &plan, serial, vendor.as_bytes(), &comments)
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
/// calls: any chunking, including chunks that split a frame, gives
/// the same bytes as the one-shot [`encode`] / [`encode_f32`].
///
/// ```
/// use oxivorbis::{Channels, Encoder, Options, SampleRate};
///
/// let mut enc = Encoder::new(SampleRate::HZ_44100, Channels::STEREO, &Options::default());
/// let mut ogg = Vec::new();
/// for chunk in vec![0i16; 44100 * 2].chunks(4097) {
///     ogg.extend_from_slice(enc.write(chunk));
/// }
/// ogg.extend_from_slice(&enc.finish());
/// let pcm = vec![0i16; 44100 * 2];
/// let one_shot = oxivorbis::encode(&pcm, SampleRate::HZ_44100, Channels::STEREO, &Options::default());
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
    ///
    /// # Panics
    ///
    /// Only if `options` carry a bitrate mode ([`Options::with_bitrate`])
    /// checked for a different rate or channel count that libvorbis can't
    /// encode at `rate` / `channels`. The same holds for every other way
    /// of encoding with such options.
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
    /// slice need not hold whole frames: the samples of a trailing partial
    /// frame are carried over and completed by the next call.
    pub fn write(&mut self, interleaved: &[i16]) -> &[u8] {
        self.core.begin_write();
        self.core.feed(interleaved, encode::i16_to_f32);
        self.core.take_output()
    }

    /// Encode interleaved `f32` samples, nominally in `-1.0..=1.0`. Returns
    /// the Ogg bytes completed by this call, like [`write`](Self::write),
    /// and carries a trailing partial frame over the same way.
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
    /// not yet returned by `write`. A partial frame still waiting for the
    /// rest of its samples is dropped.
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
/// scaled by `1/32768`. If the sample count is not a multiple of the
/// channel count, the trailing partial frame is ignored.
pub fn encode(pcm: &[i16], rate: SampleRate, channels: Channels, options: &Options) -> Vec<u8> {
    let mut core = options.core(rate, channels);
    core.feed(pcm, encode::i16_to_f32);
    core.finish()
}

/// Encode interleaved `f32` PCM (nominally `-1.0..=1.0`) into a complete
/// Ogg Vorbis stream. See [`Encoder::write_f32`] for how samples are
/// interpreted; if the sample count is not a multiple of the channel
/// count, the trailing partial frame is ignored.
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

/// One-shot encode in a bitrate mode, with the quality given as an exact
/// libvorbis `base_quality` (only [`BitrateMode::QualityLimits`] reads
/// it). `Err` where [`Options::with_bitrate`] would fail.
#[cfg(feature = "__internal")]
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn encode_bitrate_with_meta(
    samples: &[i16],
    rate: SampleRate,
    channels: Channels,
    base_quality: f32,
    mode: BitrateMode,
    tuning: BitrateTuning,
    serial: u32,
    vendor: Option<&[u8]>,
) -> Result<Vec<u8>, BitrateError> {
    encode_tuned_with_meta(
        samples,
        rate,
        channels,
        base_quality,
        Some((mode, tuning)),
        EncoderTuning::default(),
        serial,
        vendor,
    )
}

/// [`encode_bitrate_with_meta`] with optional bitrate mode (`None` = VBR)
/// and [`EncoderTuning`].
#[cfg(feature = "__internal")]
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn encode_tuned_with_meta(
    samples: &[i16],
    rate: SampleRate,
    channels: Channels,
    base_quality: f32,
    bitrate: Option<(BitrateMode, BitrateTuning)>,
    encoder_tuning: EncoderTuning,
    serial: u32,
    vendor: Option<&[u8]>,
) -> Result<Vec<u8>, BitrateError> {
    let quality = Quality::from_base_quality(base_quality).expect("base_quality in range");
    let (hz, ch) = (rate.hz(), channels.count() as usize);
    let plan =
        match bitrate {
            Some((mode, tuning)) => ratecontrol::plan(&mode, &tuning, quality, rate, channels)?
                .tune(&encoder_tuning, None, hz, ch),
            None => ratecontrol::RatePlan::vbr(base_quality, hz, ch).tune(
                &encoder_tuning,
                Some(base_quality),
                hz,
                ch,
            ),
        };
    let mut core = encode::EncoderCore::with_plan(
        rate,
        channels,
        &plan,
        serial,
        vendor.unwrap_or(DEFAULT_VENDOR.as_bytes()),
        &[],
    );
    core.feed(samples, encode::i16_to_f32);
    Ok(core.finish())
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
