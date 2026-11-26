//! Helpers shared by the oracle-backed integration tests (parity.rs,
//! corpus_sweep.rs).

#![allow(dead_code)]

use lewtoff::{Channels, SampleRate};

/// libvorbis base_quality that lewtoff currently encodes (q5).
pub const LEWTOFF_NATIVE_QUALITY: f32 = 0.5;

/// Whether lewtoff can encode at `base_quality` yet. Flip this (and
/// `lewtoff_encode_q`) once quality is wired through the public API.
pub fn lewtoff_supports_quality(base_quality: f32) -> bool {
    base_quality == LEWTOFF_NATIVE_QUALITY
}

/// Quality-aware lewtoff entry point used by the parity tests.
///
/// PENDING QUALITY WIRING: lewtoff only encodes q5 today, so any other
/// quality panics. Tests exercising other qualities are `#[ignore]`d
/// until this calls a real quality-taking encode function.
pub fn lewtoff_encode_q(
    samples: &[i16],
    rate: SampleRate,
    channels: Channels,
    base_quality: f32,
    serial: u32,
    vendor: Option<&[u8]>,
    encoder_tag: Option<&[u8]>,
) -> Vec<u8> {
    assert!(
        lewtoff_supports_quality(base_quality),
        "pending quality wiring: lewtoff cannot encode base_quality {base_quality} yet"
    );
    lewtoff::encode_with_serial_and_meta(samples, rate, channels, serial, vendor, encoder_tag)
}
