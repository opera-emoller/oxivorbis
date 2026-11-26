//! Helpers shared by the oracle-backed integration tests (parity.rs,
//! corpus_sweep.rs).

#![allow(dead_code)]

use lewtoff::{Channels, SampleRate};

/// Default libvorbis base_quality for the sweeps (q5).
pub const LEWTOFF_NATIVE_QUALITY: f32 = 0.5;

/// Quality-aware lewtoff entry point used by the parity tests.
pub fn lewtoff_encode_q(
    samples: &[i16],
    rate: SampleRate,
    channels: Channels,
    base_quality: f32,
    serial: u32,
    vendor: Option<&[u8]>,
    encoder_tag: Option<&[u8]>,
) -> Vec<u8> {
    lewtoff::encode_with_base_quality_and_meta(
        samples,
        rate,
        channels,
        base_quality,
        serial,
        vendor,
        encoder_tag,
    )
}
