//! Streaming `Encoder` behaviour: chunking invariance, incremental output,
//! bounded memory and parallel batching. No oracle needed: the one-shot
//! path is itself oracle-tested (tests/parity.rs, tests/parity_f32.rs) and
//! is a thin wrapper over the same core.

mod common;

use common::{ALL_SIGS, Rng, Sig, ch_count, rate_hz, signal_f32, signal_i16};
use lewtoff::{Channels, Encoder, Options, Quality, SampleRate};

const CONFIGS: [(SampleRate, Channels); 4] = [
    (SampleRate::Hz44100, Channels::Mono),
    (SampleRate::Hz44100, Channels::Stereo),
    (SampleRate::Hz48000, Channels::Mono),
    (SampleRate::Hz48000, Channels::Stereo),
];

fn opts(q: f32) -> Options {
    Options::default().with_quality(Quality::new(q).unwrap())
}

/// Long block size of the template `q` selects.
fn bs1(q: f32) -> usize {
    if q < 0.0 { 4096 } else { 2048 }
}

/// Pre-extrapolation training length: `(bs1/64 + 1) * 64` frames.
fn n_lpc(q: f32) -> usize {
    bs1(q) + 64
}

/// Chunk sizes (in samples, not frames) to split `len` samples into.
#[derive(Clone, Debug)]
enum Chunking {
    Whole,
    Ones,
    /// Seeded mix of 1-sample, odd, small and large chunks.
    Random(u64),
    /// `first` samples, then the rest.
    Split(usize),
}

impl Chunking {
    fn sizes(&self, len: usize) -> Vec<usize> {
        match *self {
            Chunking::Whole => vec![len],
            Chunking::Ones => vec![1; len],
            Chunking::Split(first) => vec![first.min(len), len - first.min(len)],
            Chunking::Random(seed) => {
                let mut rng = Rng(seed);
                let mut out = Vec::new();
                let mut left = len;
                while left > 0 {
                    let n = match rng.below(5) {
                        0 => 1,
                        1 => 1 + 2 * rng.below(70), // odd: splits stereo frames
                        2 => rng.below(300),        // includes empty writes
                        3 => rng.below(9000),
                        _ => rng.below(70_000),
                    }
                    .min(left);
                    out.push(n);
                    left -= n;
                }
                out
            }
        }
    }
}

fn chunkings(q: f32, ch: usize, frames: usize, seed: u64) -> Vec<Chunking> {
    let lpc = n_lpc(q) * ch;
    let mut v = vec![
        Chunking::Whole,
        Chunking::Random(seed),
        Chunking::Random(seed ^ 0xABCD),
        Chunking::Split(lpc - 1),
        Chunking::Split(lpc),
        Chunking::Split(lpc + 1),
        Chunking::Split(lpc + ch * 64 + 1),
    ];
    if frames <= 5000 {
        v.push(Chunking::Ones);
    }
    v
}

fn stream_i16(
    pcm: &[i16],
    rate: SampleRate,
    channels: Channels,
    o: &Options,
    c: &Chunking,
) -> Vec<u8> {
    let mut enc = Encoder::new(rate, channels, o);
    let mut out = Vec::new();
    let mut rest = pcm;
    for n in c.sizes(pcm.len()) {
        out.extend_from_slice(enc.write(&rest[..n]));
        rest = &rest[n..];
    }
    out.extend_from_slice(&enc.finish());
    out
}

fn stream_f32(
    pcm: &[f32],
    rate: SampleRate,
    channels: Channels,
    o: &Options,
    c: &Chunking,
) -> Vec<u8> {
    let mut enc = Encoder::new(rate, channels, o);
    let mut out = Vec::new();
    let mut rest = pcm;
    for n in c.sizes(pcm.len()) {
        out.extend_from_slice(enc.write_f32(&rest[..n]));
        rest = &rest[n..];
    }
    out.extend_from_slice(&enc.finish());
    out
}

fn lengths(q: f32) -> Vec<usize> {
    let b = bs1(q);
    let mut v = vec![0, 1, 63, 64, 65, 2047, 2048, 2049, 2111, 2112, 2113];
    v.extend([4159, 4160, 4161]); // n_lpc at q-1
    v.extend([3 * b - 1, 3 * b, 3 * b + 1]);
    v
}

fn check_invariance(q: f32) {
    let o = opts(q);
    let mut seed = 1u64;
    for (rate, channels) in CONFIGS {
        let ch = ch_count(channels);
        for sig in ALL_SIGS {
            for frames in lengths(q) {
                seed += 1;
                let pcm = signal_i16(sig, frames, ch, rate_hz(rate), seed);
                let want = lewtoff::encode(&pcm, rate, channels, &o);
                for c in chunkings(q, ch, frames, seed) {
                    let got = stream_i16(&pcm, rate, channels, &o, &c);
                    assert!(
                        got == want,
                        "i16 q{q} {rate:?} {channels:?} {sig:?} {frames} frames, {c:?}: \
                         differs at byte {}",
                        common::first_diff(&got, &want)
                    );
                }
                let pcm = signal_f32(sig, frames, ch, rate_hz(rate), seed);
                let want = lewtoff::encode_f32(&pcm, rate, channels, &o);
                for c in chunkings(q, ch, frames, seed) {
                    let got = stream_f32(&pcm, rate, channels, &o, &c);
                    assert!(
                        got == want,
                        "f32 q{q} {rate:?} {channels:?} {sig:?} {frames} frames, {c:?}: \
                         differs at byte {}",
                        common::first_diff(&got, &want)
                    );
                }
            }
        }
    }
}

#[test]
fn chunking_invariance_q5() {
    check_invariance(5.0);
}

#[test]
fn chunking_invariance_q2() {
    check_invariance(2.0);
}

#[test]
fn chunking_invariance_q_minus_1() {
    check_invariance(-1.0);
}

/// 10 s inputs: every signal, both channel layouts, three qualities.
#[test]
#[cfg_attr(debug_assertions, ignore = "slow in debug builds; run with --release")]
fn chunking_invariance_10s() {
    let mut seed = 100;
    for q in [5.0f32, 2.0, -1.0] {
        let o = opts(q);
        for (k, (rate, channels)) in CONFIGS.into_iter().enumerate() {
            let ch = ch_count(channels);
            let sig = ALL_SIGS[k];
            seed += 1;
            let frames = 10 * rate_hz(rate) as usize;
            let pcm = signal_i16(sig, frames, ch, rate_hz(rate), seed);
            let want = lewtoff::encode(&pcm, rate, channels, &o);
            for c in [Chunking::Random(seed), Chunking::Split(n_lpc(q) * ch + 1)] {
                assert!(
                    stream_i16(&pcm, rate, channels, &o, &c) == want,
                    "i16 q{q} {rate:?} {channels:?} {sig:?} 10 s, {c:?}"
                );
            }
            let pcm = signal_f32(sig, frames, ch, rate_hz(rate), seed);
            let want = lewtoff::encode_f32(&pcm, rate, channels, &o);
            assert!(
                stream_f32(&pcm, rate, channels, &o, &Chunking::Random(seed)) == want,
                "f32 q{q} {rate:?} {channels:?} {sig:?} 10 s"
            );
        }
    }
}

/// `write` and `write_f32` feed the same stream (i16 `x` == f32
/// `x / 32768`), even when a stereo frame is split between them.
#[test]
fn mixed_i16_and_f32_writes() {
    let pcm = signal_i16(Sig::Transients, 20_000, 2, 44100, 5);
    let floats: Vec<f32> = pcm.iter().map(|&x| x as f32 / 32768.0).collect();
    let o = Options::default();
    let want = lewtoff::encode(&pcm, SampleRate::Hz44100, Channels::Stereo, &o);
    let mut enc = Encoder::new(SampleRate::Hz44100, Channels::Stereo, &o);
    let mut out = Vec::new();
    let mut i = 0;
    let mut rng = Rng(77);
    while i < pcm.len() {
        let n = (1 + rng.below(3000)).min(pcm.len() - i);
        if rng.below(2) == 0 {
            out.extend_from_slice(enc.write(&pcm[i..i + n]));
        } else {
            out.extend_from_slice(enc.write_f32(&floats[i..i + n]));
        }
        i += n;
    }
    out.extend_from_slice(&enc.finish());
    assert!(out == want);
}

/// The first `write` returns the header pages; a dangling half frame at
/// `finish` is dropped, like a trailing odd sample in the one-shot path.
#[test]
fn headers_first_and_trailing_half_frame() {
    let o = Options::default();
    let mut enc = Encoder::new(SampleRate::Hz44100, Channels::Stereo, &o);
    let headers = enc.write(&[]).to_vec();
    assert_eq!(&headers[..4], b"OggS");
    assert!(enc.write(&[]).is_empty());
    let pcm = signal_i16(Sig::Sine, 3000, 2, 44100, 1);
    let mut out = headers;
    out.extend_from_slice(enc.write(&pcm));
    out.extend_from_slice(enc.write(&[123]));
    out.extend_from_slice(&enc.finish());
    assert!(out == lewtoff::encode(&pcm, SampleRate::Hz44100, Channels::Stereo, &o));
    let mut odd = pcm.clone();
    odd.push(123);
    assert!(out == lewtoff::encode(&odd, SampleRate::Hz44100, Channels::Stereo, &o));
}

/// A 60 s input produces almost all of its pages before `finish` (the
/// parallel build holds back at most one batch of blocks), and the
/// first audio pages appear long before the end of the input.
#[test]
#[cfg_attr(debug_assertions, ignore = "slow in debug builds; run with --release")]
fn streaming_emits_pages_before_finish() {
    let rate = SampleRate::Hz44100;
    let pcm = signal_i16(Sig::Noise, 60 * 44100, 2, 44100, 9);
    let o = Options::default();
    let mut enc = Encoder::new(rate, Channels::Stereo, &o);
    let mut out = Vec::new();
    let mut first_audio_page_at = None;
    let chunk = 4410 * 2; // 100 ms
    for (k, c) in pcm.chunks(chunk).enumerate() {
        let bytes = enc.write(c);
        out.extend_from_slice(bytes);
        // Header pages are ~4 KB; anything well beyond is audio.
        if first_audio_page_at.is_none() && out.len() > 16_000 {
            first_audio_page_at = Some(k);
        }
    }
    let before_finish = out.len();
    out.extend_from_slice(&enc.finish());
    assert!(out == lewtoff::encode(&pcm, rate, Channels::Stereo, &o));
    let at = first_audio_page_at.expect("no pages before finish");
    assert!(
        at < 20,
        "first audio page only after {} ms of input",
        at * 100
    );
    assert!(
        before_finish as f64 > 0.95 * out.len() as f64,
        "only {before_finish} of {} bytes before finish",
        out.len()
    );
}

/// The analysis buffer stays bounded independent of input length.
#[test]
#[cfg_attr(debug_assertions, ignore = "slow in debug builds; run with --release")]
fn buffer_is_bounded_over_10_minutes() {
    for (q, minutes, limit) in [(5.0f32, 10, 12_000), (-1.0, 2, 20_000)] {
        let mut enc = Encoder::new(SampleRate::Hz48000, Channels::Stereo, &opts(q));
        let mut max = 0;
        let second = signal_i16(Sig::Transients, 48_000, 2, 48_000, 3);
        for _ in 0..minutes * 60 {
            for c in second.chunks(3001) {
                enc.write(c);
                max = max.max(enc.buffered_frames());
            }
        }
        assert!(
            max <= limit,
            "q{q}: buffered up to {max} frames (limit {limit})"
        );
        enc.finish();
    }
}

/// Absurdly loud (but finite) f32 input encodes without panicking, in
/// debug builds too (integer overflow checks).
#[test]
fn extreme_f32_does_not_panic() {
    for g in [1e5f32, 1e10, 1e20, 1e30, f32::MAX] {
        for q in [5.0f32, -0.5, 10.0] {
            let s: Vec<f32> = (0..20_000)
                .map(|i| {
                    if (i / 3000) % 2 == 0 {
                        (i as f32 * 0.37).sin() * g
                    } else if i % 2 == 0 {
                        g
                    } else {
                        -g
                    }
                })
                .collect();
            let ogg = lewtoff::encode_f32(&s, SampleRate::Hz44100, Channels::Stereo, &opts(q));
            assert_eq!(&ogg[..4], b"OggS");
        }
    }
}

/// Parallel batch size changes scheduling only, never bytes.
#[cfg(feature = "parallel")]
#[test]
fn parallel_batch_sizes_give_identical_bytes() {
    for (q, sig) in [
        (5.0f32, Sig::Transients),
        (-1.0, Sig::Noise),
        (2.0, Sig::Sine),
    ] {
        let pcm = signal_i16(sig, 5 * 44100, 2, 44100, 4);
        let o = opts(q);
        let want = lewtoff::encode(&pcm, SampleRate::Hz44100, Channels::Stereo, &o);
        for batch in [1usize, 3, 256] {
            let mut enc = Encoder::new(SampleRate::Hz44100, Channels::Stereo, &o);
            enc.set_parallel_batch(batch);
            let mut out = Vec::new();
            for c in pcm.chunks(7777) {
                out.extend_from_slice(enc.write(c));
            }
            out.extend_from_slice(&enc.finish());
            assert!(out == want, "q{q} batch {batch}");
        }
    }
}
