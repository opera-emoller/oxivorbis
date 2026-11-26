//! lewtoff: pure-Rust Ogg Vorbis encoder, byte-identical to libvorbis 1.3.7
//! (VBR, oggenc qualities 0..10).
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
mod envelope;
mod lpc;
mod mapping0;
mod quality;
mod setup;
mod setup_gen;
mod setup_templates;
// The Q5-era BLOCK_SIZE / HALF_BLOCK aliases in window.rs are unused now
// that block sizes come from QualityConfig; window.rs is being made
// block-size generic separately, so silence that here rather than edit it.
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

/// Encode interleaved `i16` PCM into an Ogg Vorbis bitstream at quality Q5.
///
/// Output is byte-for-byte identical to libvorbis 1.3.7 at `-q 5` (built without
/// FMA contraction) for the supported input space (see crate docs / `README.md`).
pub fn encode(samples: &[i16], rate: SampleRate, channels: Channels) -> Vec<u8> {
    crate::encode::encode_impl(samples, rate, channels)
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
/// Supported range is `0.0..=10.0`. Panics below 0: oggenc's `-1..0`
/// range (libvorbis q-1, 512/4096-sample blocks) is not supported yet.
pub fn encode_with_quality(
    samples: &[i16],
    rate: SampleRate,
    channels: Channels,
    quality: f32,
) -> Vec<u8> {
    assert!(
        quality >= 0.0,
        "q-1 not yet supported (quality {quality}; supported range is 0..=10)"
    );
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
    crate::encode::encode_with_serial(samples, rate, channels, serial)
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
    crate::encode::encode_with_serial_and_meta(samples, rate, channels, serial, vendor, encoder_tag)
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
