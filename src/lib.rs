//! lewtoff: pure-Rust Ogg Vorbis encoder, byte-identical to libvorbis 1.3.7
//! (VBR, oggenc qualities -1..10).
//!
//! See `README.md` for scope, design, and constraints. The crate intentionally
//! has a tiny public surface — two functions and two enums — because the
//! supported input space is closed by construction (no `Result` needed).

#![forbid(unsafe_code)]

#[allow(dead_code)]
mod tables;

mod debug_dump;

#[allow(dead_code)]
mod setup_blob;

#[allow(dead_code)]
mod codebook;

#[allow(dead_code)]
mod floor1;

#[allow(dead_code)]
mod residue;

#[doc(hidden)]
pub mod bitpack;

#[doc(hidden)]
pub mod headers;

#[doc(hidden)]
pub mod psy;

#[doc(hidden)]
pub mod mdct;

#[doc(hidden)]
pub mod ogg_pages;

mod drft;
#[doc(hidden)]
pub mod encode;
#[cfg(test)]
mod encode_legacy;
mod envelope;
mod lpc;
mod mapping0;
mod quality;
mod setup;
mod setup_gen;
mod setup_templates;
mod window;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SampleRate {
    Hz44100,
    Hz48000,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Channels {
    Mono,
    Stereo,
}

/// Streaming Ogg Vorbis encoder.
///
/// Feed interleaved PCM in chunks of any size with [`write`](Self::write)
/// (`i16`) or [`write_f32`](Self::write_f32) (`f32`), collect the Ogg pages
/// each call completes, and call [`finish`](Self::finish) for the rest of
/// the stream. Memory use is bounded (a few blocks of PCM) regardless of
/// input length.
///
/// The output is byte-for-byte what libvorbis 1.3.7 produces for the same
/// samples, and it does not depend on how the input is split across
/// `write` calls: any chunking (including chunks that split a stereo frame)
/// gives the same bytes as the one-shot [`encode_with_quality`].
///
/// ```
/// use lewtoff::{Channels, Encoder, SampleRate};
///
/// let mut enc = Encoder::new(SampleRate::Hz44100, Channels::Stereo, 5.0);
/// let mut ogg = Vec::new();
/// for chunk in vec![0i16; 44100 * 2].chunks(4096) {
///     ogg.extend_from_slice(enc.write(chunk));
/// }
/// ogg.extend_from_slice(&enc.finish());
/// assert_eq!(&ogg[..4], b"OggS");
/// ```
pub struct Encoder {
    core: Box<crate::encode::EncoderCore>,
}

impl Encoder {
    /// New encoder at `quality` on oggenc's `-q` scale (-1..10, fractional
    /// allowed), with a randomly chosen stream serial number.
    ///
    /// # Panics
    ///
    /// Panics if `quality < -1` (see [`encode_with_quality`]).
    pub fn new(rate: SampleRate, channels: Channels, quality: f32) -> Self {
        Self::with_serial(rate, channels, quality, crate::encode::random_serial())
    }

    /// Like [`new`](Self::new) but with an explicit Ogg stream serial
    /// number, for reproducible output or when several logical streams
    /// must have distinct serials.
    ///
    /// # Panics
    ///
    /// Panics if `quality < -1` (see [`encode_with_quality`]).
    pub fn with_serial(rate: SampleRate, channels: Channels, quality: f32, serial: u32) -> Self {
        Self::with_base_quality_and_meta(
            rate,
            channels,
            oggenc_base_quality(quality),
            serial,
            None,
            None,
        )
    }

    /// Fully explicit constructor: libvorbis `base_quality` (-0.1..=1.0),
    /// serial, and comment-header vendor / encoder tag (`None` = defaults).
    /// Exposed for parity tests.
    #[doc(hidden)]
    pub fn with_base_quality_and_meta(
        rate: SampleRate,
        channels: Channels,
        base_quality: f32,
        serial: u32,
        vendor: Option<&[u8]>,
        encoder_tag: Option<&[u8]>,
    ) -> Self {
        Self {
            core: Box::new(crate::encode::EncoderCore::new(
                rate,
                channels,
                base_quality,
                serial,
                vendor,
                encoder_tag,
            )),
        }
    }

    /// Encode interleaved 16-bit samples. Returns the Ogg bytes (whole
    /// pages) completed by this call, which may be empty. The first call
    /// also returns the three header pages.
    ///
    /// Samples are scaled by `1/32768`, as libvorbis-based tools do. The
    /// slice need not hold whole frames: an unpaired stereo sample is kept
    /// until the next call.
    pub fn write(&mut self, interleaved: &[i16]) -> &[u8] {
        self.core.begin_write();
        self.core.feed(interleaved, crate::encode::i16_to_f32);
        self.core.take_output()
    }

    /// Encode interleaved `f32` samples, nominally in `-1.0..=1.0`. Returns
    /// the Ogg bytes completed by this call, like [`write`](Self::write).
    ///
    /// The samples go to the encoder unscaled, exactly as if passed to
    /// libvorbis's `vorbis_analysis_buffer`, and the output matches
    /// libvorbis for any finite input (including values outside ±1.0,
    /// subnormals and `-0.0`). Non-finite samples (NaN, ±infinity), which
    /// libvorbis turns into a corrupt stream, are encoded as `0.0`.
    ///
    /// `write` and `write_f32` may be mixed; both feed the same stream.
    pub fn write_f32(&mut self, interleaved: &[f32]) -> &[u8] {
        self.core.begin_write();
        self.core.feed(interleaved, crate::encode::f32_sample);
        self.core.take_output()
    }

    /// Flush the remaining input, end the stream, and return the remaining
    /// Ogg bytes. An unpaired trailing stereo sample is dropped.
    pub fn finish(self) -> Vec<u8> {
        self.core.finish()
    }

    /// Frames currently held in the analysis buffer. Diagnostic for the
    /// bounded-memory tests.
    #[doc(hidden)]
    pub fn buffered_frames(&self) -> usize {
        self.core.buffered_frames()
    }

    /// Blocks analyzed per parallel batch (default 8 x Rayon threads).
    /// Output does not depend on it; exposed for tests.
    #[doc(hidden)]
    #[cfg(feature = "parallel")]
    pub fn set_parallel_batch(&mut self, blocks: usize) {
        self.core.set_batch(blocks);
    }
}

/// Encode interleaved `i16` PCM into an Ogg Vorbis bitstream at quality Q5.
///
/// Output is byte-for-byte identical to libvorbis 1.3.7 at `-q 5` (built without
/// FMA contraction) for the supported input space (see crate docs / `README.md`).
pub fn encode(samples: &[i16], rate: SampleRate, channels: Channels) -> Vec<u8> {
    encode_with_serial(samples, rate, channels, crate::encode::random_serial())
}

/// Encode interleaved `i16` PCM into an Ogg Vorbis bitstream at the given
/// quality, on oggenc's `-q` scale (fractional values allowed).
///
/// Output is byte-for-byte identical to libvorbis 1.3.7 VBR mode at the
/// same quality — i.e. what `oggenc -q <quality>` produces with an
/// FMA-free libvorbis build. `quality` is converted to libvorbis's
/// `base_quality` float exactly as oggenc does (`(float)(q * 0.1)`, see
/// [`oggenc_base_quality`]); values above 10 are clamped to 10.
///
/// # Panics
///
/// Supported range is `-1.0..=10.0` (above 10 clamps to 10). Panics below
/// -1, where libvorbis itself rejects the quality. `-1 <= quality < 0`
/// selects libvorbis's q-1 template (512/4096-sample blocks).
pub fn encode_with_quality(
    samples: &[i16],
    rate: SampleRate,
    channels: Channels,
    quality: f32,
) -> Vec<u8> {
    crate::encode::encode_with_base_quality_and_meta(
        samples,
        rate,
        channels,
        oggenc_base_quality(quality),
        crate::encode::random_serial(),
        None,
        None,
    )
}

/// Encode interleaved `f32` PCM (nominally `-1.0..=1.0`) at the given
/// quality on oggenc's `-q` scale. See [`Encoder::write_f32`] for how
/// samples are interpreted and [`encode_with_quality`] for `quality`.
///
/// # Panics
///
/// Panics if `quality < -1`.
pub fn encode_f32_with_quality(
    samples: &[f32],
    rate: SampleRate,
    channels: Channels,
    quality: f32,
) -> Vec<u8> {
    let mut enc = Encoder::new(rate, channels, quality);
    enc.core.feed(samples, crate::encode::f32_sample);
    enc.finish()
}

/// Map an oggenc `-q` value to the libvorbis `base_quality` float, bit for
/// bit as vorbis-tools 1.4.3 does it: `((q as f32) as f64 * 0.1) as f32`,
/// with `q > 10` clamped to 10. See the source comment for provenance.
#[doc(hidden)]
pub fn oggenc_base_quality(quality: f32) -> f32 {
    crate::encode::oggenc_base_quality(quality)
}

/// Like [`encode`] but uses the given stream serial number.
/// Exposed for parity tests so the test can match the serial the reference encoder chose.
#[doc(hidden)]
pub fn encode_with_serial(
    samples: &[i16],
    rate: SampleRate,
    channels: Channels,
    serial: u32,
) -> Vec<u8> {
    encode_with_serial_and_meta(samples, rate, channels, serial, None, None)
}

/// Like [`encode_with_serial`] but also accepts vendor and encoder_tag strings
/// to embed in the comment header.  Pass `None` to use compile-time defaults.
/// Exposed for parity tests so the test can mirror the reference encoder's comment header.
#[doc(hidden)]
pub fn encode_with_serial_and_meta(
    samples: &[i16],
    rate: SampleRate,
    channels: Channels,
    serial: u32,
    vendor: Option<&[u8]>,
    encoder_tag: Option<&[u8]>,
) -> Vec<u8> {
    crate::encode::encode_with_base_quality_and_meta(
        samples,
        rate,
        channels,
        crate::encode::Q5_BASE_QUALITY,
        serial,
        vendor,
        encoder_tag,
    )
}

/// Fully explicit encode: libvorbis `base_quality` (-0.1..=1.0 scale, the
/// value passed to `vorbis_encode_init_vbr`), stream serial, and comment
/// header strings. Exposed for parity tests that drive the oracle with an
/// exact base_quality float.
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
    crate::encode::encode_with_base_quality_and_meta(
        samples,
        rate,
        channels,
        base_quality,
        serial,
        vendor,
        encoder_tag,
    )
}

/// f32 counterpart of [`encode_with_base_quality_and_meta`], for parity
/// tests.
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
    let mut enc = Encoder::with_base_quality_and_meta(
        rate,
        channels,
        base_quality,
        serial,
        vendor,
        encoder_tag,
    );
    enc.core.feed(samples, crate::encode::f32_sample);
    enc.finish()
}
