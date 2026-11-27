//! The only module that touches the `oxivorbis` library API.
//!
//! Everything else in the CLI deals in plain `u32` rates, `u16` channel
//! counts, `f32` qualities and byte slices, so an API change in the
//! library (rate newtype, fallible construction, writer adapters) only
//! needs edits here.

use crate::args::{Advanced, RateArgs};
use oxivorbis::{
    BitrateMode, BitrateTuning, Channels, Encoder, EncoderTuning, Options, Quality, SampleRate,
};

/// Encoder settings in CLI terms.
pub struct Settings {
    /// oggenc `-q` scale; already validated with [`quality_ok`].
    pub quality: f32,
    pub rate_args: RateArgs,
    pub serial: Option<u32>,
    pub comments: Vec<(String, String)>,
}

/// oggenc 1.4.3's reading of `-q`, `-b`, `-m`, `-M` and `--managed`
/// (`oggenc/encode.c`): quality mode when `-q` is given or no bitrate is,
/// with hard limits if `-m`/`-M` are given; otherwise `-b` alone is VBR
/// tuned for that bitrate, `-b` with management is ABR, and limits
/// without `-b` are limits only. `None` is plain VBR.
pub fn mode(r: &RateArgs) -> Option<(BitrateMode, BitrateTuning)> {
    let quality_set = r.quality_set || (r.bitrate.is_none() && r.min.is_none() && r.max.is_none());
    let mode = if quality_set {
        if r.min.is_none() && r.max.is_none() {
            return None;
        }
        BitrateMode::QualityLimits {
            min_kbps: r.min,
            max_kbps: r.max,
        }
    } else {
        match r.bitrate {
            Some(kbps) if !r.managed => BitrateMode::Nominal { kbps },
            Some(kbps) => BitrateMode::Average {
                kbps,
                min_kbps: r.min,
                max_kbps: r.max,
            },
            None => BitrateMode::Limits {
                min_kbps: r.min,
                max_kbps: r.max,
            },
        }
    };
    let mut t = BitrateTuning::default();
    for a in &r.advanced {
        t = match *a {
            Advanced::ReservoirBits(b) => t.with_reservoir_bits(b),
            Advanced::ReservoirBias(b) => t.with_reservoir_bias(b),
            Advanced::AverageDamping(d) => t.with_average_damping(d),
            _ => t,
        };
    }
    Some((mode, t))
}

/// The lowpass / impulse-noisetune / coupling advanced options.
pub fn encoder_tuning(r: &RateArgs) -> EncoderTuning {
    let mut t = EncoderTuning::default();
    for a in &r.advanced {
        t = match *a {
            Advanced::LowpassKhz(k) => t.with_lowpass_khz(k),
            Advanced::ImpulseNoisetune(d) => t.with_impulse_noisetune(d),
            Advanced::DisableCoupling => t.with_coupling_disabled(),
            _ => t,
        };
    }
    t
}

/// The summary line's description of the setting, e.g. `q3` or
/// `managed 128 kbps (min 96)`.
pub fn describe(quality: f32, r: &RateArgs) -> String {
    let lim = |min: Option<u32>, max: Option<u32>| match (min, max) {
        (Some(a), Some(b)) => format!(" (min {a}, max {b})"),
        (Some(a), None) => format!(" (min {a})"),
        (None, Some(b)) => format!(" (max {b})"),
        (None, None) => String::new(),
    };
    match mode(r) {
        None => format!("q{quality}"),
        Some((m, _)) => match m {
            BitrateMode::Nominal { kbps } => format!("~{kbps} kbps VBR"),
            BitrateMode::Average {
                kbps,
                min_kbps,
                max_kbps,
            } => format!("managed {kbps} kbps{}", lim(min_kbps, max_kbps)),
            BitrateMode::Limits { min_kbps, max_kbps } => {
                format!("managed{}", lim(min_kbps, max_kbps))
            }
            BitrateMode::QualityLimits { min_kbps, max_kbps } => {
                format!("q{quality}{}", lim(min_kbps, max_kbps))
            }
            _ => "bitrate managed".into(),
        },
    }
}

/// Whether `q` is a quality the library accepts.
pub fn quality_ok(q: f32) -> bool {
    Quality::new(q).is_some()
}

/// Library name and version, e.g. `"oxivorbis 0.1.0"`.
pub fn library_version() -> &'static str {
    oxivorbis::DEFAULT_VENDOR
}

/// Whether WAV/SMPTE channel order differs from Vorbis order for this
/// many channels (3, 5, 6, 7 and 8 channels; 1, 2 and 4 are the same).
pub fn wav_order_differs(channels: u16) -> bool {
    Channels::new(channels)
        .and_then(|c| c.wav_to_vorbis())
        .is_some_and(|m| m.iter().enumerate().any(|(i, &v)| i != v))
}

/// A streaming encoder.
pub struct Enc(Encoder, Channels);

impl Enc {
    /// New encoder, or a user-facing reason the rate/channels/quality
    /// combination is not supported.
    pub fn new(rate_hz: u32, channels: u16, s: &Settings) -> Result<Enc, String> {
        let rate = SampleRate::try_from(rate_hz).map_err(|e| {
            format!("{e} Hz (resample first, e.g. `ffmpeg -i in.wav -ar 48000 out.wav`)")
        })?;
        let channels = Channels::try_from(channels).map_err(|e| e.to_string())?;
        let quality = Quality::new(s.quality)
            .ok_or_else(|| format!("unsupported quality {}; want -1..10", s.quality))?;
        let mut opts = Options::default()
            .with_quality(quality)
            .with_encoder_tuning(encoder_tuning(&s.rate_args));
        if let Some((mode, tuning)) = mode(&s.rate_args) {
            opts = opts
                .with_bitrate_tuning(rate, channels, mode, tuning)
                .map_err(|e| e.to_string())?;
        }
        if let Some(serial) = s.serial {
            opts = opts.with_serial(serial);
        }
        for (k, v) in &s.comments {
            opts = opts.with_comment(k.as_str(), v.as_str());
        }
        Ok(Enc(Encoder::new(rate, channels, &opts), channels))
    }

    /// Reorder whole interleaved frames in place from WAV/SMPTE to Vorbis
    /// channel order (a no-op beyond 8 channels).
    pub fn reorder_wav<T: Copy>(&self, interleaved: &mut [T]) {
        oxivorbis::reorder_wav_to_vorbis(interleaved, self.1);
    }

    /// Encode interleaved 16-bit samples (the library's exact i16 path);
    /// returns the Ogg bytes completed so far.
    pub fn write_i16(&mut self, interleaved: &[i16]) -> &[u8] {
        self.0.write(interleaved)
    }

    /// Encode interleaved `f32` samples, passed to the encoder unscaled.
    pub fn write_f32(&mut self, interleaved: &[f32]) -> &[u8] {
        self.0.write_f32(interleaved)
    }

    /// End the stream; returns the remaining Ogg bytes.
    pub fn finish(self) -> Vec<u8> {
        self.0.finish()
    }
}
