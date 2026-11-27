//! The only module that touches the `oxivorbis` library API.
//!
//! Everything else in the CLI deals in plain `u32` rates, `u16` channel
//! counts, `f32` qualities and byte slices, so an API change in the
//! library (rate newtype, fallible construction, writer adapters) only
//! needs edits here.

use oxivorbis::{Channels, Encoder, Options, Quality, SampleRate};

/// Encoder settings in CLI terms.
pub struct Settings {
    /// oggenc `-q` scale; already validated with [`quality_ok`].
    pub quality: f32,
    pub serial: Option<u32>,
    pub comments: Vec<(String, String)>,
}

/// Whether `q` is a quality the library accepts.
pub fn quality_ok(q: f32) -> bool {
    Quality::new(q).is_some()
}

/// Library name and version, e.g. `"oxivorbis 0.1.0"`.
pub fn library_version() -> &'static str {
    oxivorbis::DEFAULT_VENDOR
}

/// A streaming encoder.
pub struct Enc(Encoder);

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
        let mut opts = Options::default().with_quality(quality);
        if let Some(serial) = s.serial {
            opts = opts.with_serial(serial);
        }
        for (k, v) in &s.comments {
            opts = opts.with_comment(k.as_str(), v.as_str());
        }
        Ok(Enc(Encoder::new(rate, channels, &opts)))
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
