//! Helpers shared by the oracle-backed integration tests (parity.rs,
//! corpus_sweep.rs).

#![allow(dead_code)]

use lewtoff::{Channels, SampleRate};

/// Default libvorbis base_quality for the sweeps (q5).
pub const LEWTOFF_NATIVE_QUALITY: f32 = 0.5;

/// Whether lewtoff can encode at `base_quality`: q0..q10 (0.0..=1.0).
/// q-1 (base_quality < 0, 512/4096-sample blocks) is pending.
pub fn lewtoff_supports_quality(base_quality: f32) -> bool {
    (0.0..=1.0).contains(&base_quality)
}

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
    assert!(
        lewtoff_supports_quality(base_quality),
        "pending 512/4096 block support: lewtoff cannot encode base_quality {base_quality} yet"
    );
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
