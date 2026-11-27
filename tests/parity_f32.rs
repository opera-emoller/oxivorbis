//! f32-input parity: `Encoder::write_f32` / `encode_f32_with_quality`
//! against libvorbis fed the same floats through `vorbis_analysis_buffer`
//! (`tools/oracle-encoder/oracle-encoder <rate> <ch> <q> f32`).
//!
//! Run with `cargo nextest run --features oracle parity_f32`.

#![cfg(feature = "oracle")]

mod common;

use common::{Rng, Sig, ch_count, rate_hz, signal_f32};
use oxivorbis::{Channels, SampleRate};

fn f32_bytes(samples: &[f32]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

fn oracle_f32(samples: &[f32], rate: SampleRate, channels: Channels, q: f32) -> Vec<u8> {
    common::oracle_encode_raw(
        &f32_bytes(samples),
        rate_hz(rate),
        ch_count(channels) as u16,
        q,
        "f32",
    )
}

/// oxivorbis over `samples`, fed through `write_f32` in the given chunk
/// sizes (cycled; empty = one-shot), with the oracle's serial and comment
/// strings.
fn oxivorbis_f32(
    samples: &[f32],
    rate: SampleRate,
    channels: Channels,
    q: f32,
    oracle: &[u8],
    chunks: &[usize],
) -> Vec<u8> {
    let serial = common::extract_serial(oracle);
    let (vendor, tag) = common::extract_comment_strings(oracle);
    if chunks.is_empty() {
        return oxivorbis::encode_f32_with_base_quality_and_meta(
            samples,
            rate,
            channels,
            q,
            serial,
            Some(&vendor),
            Some(&tag),
        );
    }
    let mut enc = oxivorbis::encoder_with_base_quality_and_meta(
        rate,
        channels,
        q,
        serial,
        Some(&vendor),
        Some(&tag),
    );
    let mut out = Vec::new();
    let mut rest = samples;
    for &n in chunks.iter().cycle() {
        if rest.is_empty() {
            break;
        }
        let n = n.min(rest.len());
        out.extend_from_slice(enc.write_f32(&rest[..n]));
        rest = &rest[n..];
    }
    out.extend_from_slice(&enc.finish());
    out
}

fn assert_same(what: &str, lw: &[u8], oracle: &[u8]) {
    if lw != oracle {
        let d = common::first_diff(lw, oracle);
        panic!(
            "{what}: f32 parity diverged at byte {d} (oxivorbis {} bytes, oracle {} bytes)",
            lw.len(),
            oracle.len()
        );
    }
}

/// Oracle parity for `samples` (one-shot and odd-sized chunked writes).
/// `oracle_input` is what libvorbis is fed (differs from `samples` only
/// for the non-finite cases).
fn check(
    what: &str,
    samples: &[f32],
    oracle_input: &[f32],
    rate: SampleRate,
    channels: Channels,
    q: f32,
) {
    let oracle = oracle_f32(oracle_input, rate, channels, q);
    assert_same(
        what,
        &oxivorbis_f32(samples, rate, channels, q, &oracle, &[]),
        &oracle,
    );
    assert_same(
        &format!("{what} (chunked 1/63/1001/7)"),
        &oxivorbis_f32(samples, rate, channels, q, &oracle, &[1, 63, 1001, 7]),
        &oracle,
    );
}

const CONFIGS: [(SampleRate, Channels); 4] = [
    (SampleRate::Hz44100, Channels::Mono),
    (SampleRate::Hz44100, Channels::Stereo),
    (SampleRate::Hz48000, Channels::Mono),
    (SampleRate::Hz48000, Channels::Stereo),
];

fn signals_at(q: f32) {
    for (rate, channels) in CONFIGS {
        for sig in [Sig::Sine, Sig::Transients, Sig::Noise] {
            let s = signal_f32(sig, 30_011, ch_count(channels), rate_hz(rate), 11);
            check(
                &format!("{sig:?} {rate:?} {channels:?} q{q}"),
                &s,
                &s,
                rate,
                channels,
                q,
            );
        }
    }
}

#[test]
fn parity_f32_signals_q5() {
    signals_at(0.5);
}

#[test]
fn parity_f32_signals_q02() {
    signals_at(0.2);
}

#[test]
fn parity_f32_signals_q_m005() {
    signals_at(-0.05);
}

#[test]
fn parity_f32_signals_q10() {
    signals_at(1.0);
}

/// Values beyond ±1.0 are not clipped: libvorbis encodes them as-is.
#[test]
fn parity_f32_out_of_range() {
    for (gain, q) in [(1.5f32, 0.5f32), (4.0, 0.2), (100.0, 0.5), (1.0e4, -0.05)] {
        for channels in [Channels::Mono, Channels::Stereo] {
            let s: Vec<f32> = signal_f32(Sig::Transients, 20_000, ch_count(channels), 44100, 5)
                .into_iter()
                .map(|v| v * gain)
                .collect();
            check(
                &format!("gain {gain} {channels:?} q{q}"),
                &s,
                &s,
                SampleRate::Hz44100,
                channels,
                q,
            );
        }
    }
}

/// Subnormals and signed zeros go through untouched.
#[test]
fn parity_f32_subnormals_and_signed_zero() {
    let mut rng = Rng(9);
    let tiny = f32::from_bits(1); // smallest subnormal
    let cases: Vec<(&str, Vec<f32>)> = vec![
        ("all -0.0", vec![-0.0f32; 9000 * 2]),
        (
            "alternating +-0.0",
            (0..9000 * 2)
                .map(|i| if i % 3 == 0 { -0.0 } else { 0.0 })
                .collect(),
        ),
        (
            "subnormal noise",
            (0..9000 * 2)
                .map(|_| f32::from_bits(rng.below(0x007f_ffff) as u32 + 1) * rng.unit().signum())
                .collect(),
        ),
        (
            "sine with subnormal tail",
            signal_f32(Sig::Sine, 9000, 2, 44100, 1)
                .into_iter()
                .enumerate()
                .map(|(i, v)| if i > 9000 { tiny * (i % 5) as f32 } else { v })
                .collect(),
        ),
        (
            "sine with -0.0 gaps",
            signal_f32(Sig::Sine, 9000, 2, 44100, 1)
                .into_iter()
                .enumerate()
                .map(|(i, v)| if (i / 700) % 2 == 1 { -0.0 } else { v })
                .collect(),
        ),
    ];
    for (what, s) in &cases {
        for q in [0.5f32, -0.1] {
            check(
                &format!("{what} q{q}"),
                s,
                s,
                SampleRate::Hz44100,
                Channels::Stereo,
                q,
            );
        }
    }
}

/// NaN and ±infinity are encoded as 0.0 (libvorbis itself produces a
/// corrupt stream for them): oxivorbis on the raw input must equal the
/// oracle on the input with those samples zeroed.
#[test]
fn parity_f32_non_finite_encoded_as_zero() {
    let base = signal_f32(Sig::Transients, 20_000, 2, 44100, 3);
    let specials = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -f32::NAN];
    let mut raw = base.clone();
    let mut zeroed = base.clone();
    let mut rng = Rng(4);
    for k in 0..200 {
        let i = rng.below(raw.len());
        raw[i] = specials[k % specials.len()];
        zeroed[i] = 0.0;
    }
    // A run of NaN at the very start (pre-extrapolation input) and end
    // (post-extrapolation input).
    for i in (0..300).chain(raw.len() - 300..raw.len()) {
        raw[i] = f32::NAN;
        zeroed[i] = 0.0;
    }
    for q in [0.5f32, -0.05] {
        check(
            &format!("non-finite q{q}"),
            &raw,
            &zeroed,
            SampleRate::Hz44100,
            Channels::Stereo,
            q,
        );
    }
}

/// Odd sample counts split stereo frames across `write_f32` calls.
#[test]
fn parity_f32_odd_chunking() {
    let s = signal_f32(Sig::Noise, 12_345, 2, 48000, 8);
    let oracle = oracle_f32(&s, SampleRate::Hz48000, Channels::Stereo, 0.5);
    for chunks in [&[1usize][..], &[3], &[127, 2113], &[4095, 1]] {
        assert_same(
            &format!("chunks {chunks:?}"),
            &oxivorbis_f32(
                &s,
                SampleRate::Hz48000,
                Channels::Stereo,
                0.5,
                &oracle,
                chunks,
            ),
            &oracle,
        );
    }
}

/// `x / 32768.0` through `write_f32` equals `x` through `write`, and both
/// equal the oracle's s16 path.
#[test]
fn parity_f32_matches_i16_scaling() {
    let pcm = common::signal_i16(Sig::Transients, 25_000, 2, 44100, 2);
    let floats: Vec<f32> = pcm.iter().map(|&x| x as f32 / 32768.0).collect();
    let raw: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
    let oracle_s16 = common::oracle_encode_raw(&raw, 44100, 2, 0.5, "s16");
    let oracle_f = oracle_f32(&floats, SampleRate::Hz44100, Channels::Stereo, 0.5);
    assert_eq!(oracle_s16, oracle_f, "oracle s16 vs f32 input");
    let lw = oxivorbis_f32(
        &floats,
        SampleRate::Hz44100,
        Channels::Stereo,
        0.5,
        &oracle_f,
        &[],
    );
    assert_same("f32 of i16", &lw, &oracle_f);
}
