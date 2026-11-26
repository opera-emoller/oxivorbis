//! Robustness tests: encodes driven from inside Rayon pools (the
//! `parallel` feature must not deadlock there), adversarial f32 input,
//! comment sanitizing, and the concurrency of the setup caches.
//!
//! Everything here runs in both the serial and the `parallel` build.

use std::sync::mpsc;
use std::time::Duration;

use lewtoff::{Channels, Encoder, Options, SampleRate};
use rayon::prelude::*;

/// Run `f` on its own thread and fail if it doesn't finish within
/// `secs`, so a deadlock fails the test instead of stalling CI. A hung
/// thread is leaked; the test process exits regardless.
fn watchdog<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)));
    });
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(Ok(v)) => v,
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(_) => panic!("timed out after {secs} s: encode deadlocked or stalled"),
    }
}

/// Deadlock watchdog budget. Generous, because debug builds are slow and
/// CI hosts are loaded; a real deadlock never finishes.
const WATCHDOG_SECS: u64 = 120;

/// SplitMix64.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [-1, 1).
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 23) as f32 - 1.0
    }
}

/// Tone plus noise with a burst every second, stereo-interleaved i16.
fn music(frames: usize, seed: u64) -> Vec<i16> {
    let mut rng = Rng(seed);
    let mut out = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let t = i as f32 / 44_100.0;
        let burst = if i % 44_100 < 512 { 0.6 } else { 0.0 };
        for c in 0..2 {
            let tone = (t * (440.0 + 110.0 * c as f32) * std::f32::consts::TAU).sin() * 0.3;
            let v = tone + rng.unit() * (0.05 + burst);
            out.push((v.clamp(-1.0, 1.0) * 32_000.0) as i16);
        }
    }
    out
}

fn encode_music(pcm: &[i16]) -> Vec<u8> {
    lewtoff::encode(
        pcm,
        SampleRate::Hz44100,
        Channels::Stereo,
        &Options::default(),
    )
}

/// Streaming encode with a small parallel batch so a short input spans
/// many batches.
fn stream_music(pcm: &[i16], chunk: usize) -> Vec<u8> {
    let mut enc = Encoder::new(SampleRate::Hz44100, Channels::Stereo, &Options::default());
    #[cfg(feature = "parallel")]
    enc.set_parallel_batch(3);
    let mut out = Vec::new();
    for c in pcm.chunks(chunk) {
        out.extend_from_slice(enc.write(c));
    }
    out.extend_from_slice(&enc.finish());
    out
}

/// More encodes than pool threads, each on a pool thread: the
/// `par_iter(...).map(encode)` pattern that used to hang with `parallel`.
#[test]
fn encodes_inside_par_iter() {
    let pcm = music(2 * 44_100, 1);
    let want = encode_music(&pcm);
    let got = watchdog(WATCHDOG_SECS, move || {
        let n = 2 * rayon::current_num_threads() + 3;
        (0..n)
            .into_par_iter()
            .map(|i| {
                if i % 2 == 0 {
                    encode_music(&pcm)
                } else {
                    stream_music(&pcm, 1000 + 37 * i)
                }
            })
            .collect::<Vec<_>>()
    });
    for (i, g) in got.iter().enumerate() {
        assert!(*g == want, "encode {i} inside par_iter differs from serial");
    }
}

/// An encode under `install` on a 1-thread pool: the only worker is the
/// caller, so anything it blocks on can never run.
#[test]
fn encode_in_single_thread_pool() {
    let pcm = music(3 * 44_100, 2);
    let want = encode_music(&pcm);
    let (one_shot, streamed) = watchdog(WATCHDOG_SECS, move || {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        pool.install(|| (encode_music(&pcm), stream_music(&pcm, 4096)))
    });
    assert!(one_shot == want, "1-thread pool one-shot differs");
    assert!(streamed == want, "1-thread pool streaming differs");
}

/// Encodes in a pool nested inside another pool's worker, plus a
/// par_iter of encodes inside the inner pool.
#[test]
fn encode_in_nested_pools() {
    let pcm = music(2 * 44_100, 3);
    let want = encode_music(&pcm);
    let got = watchdog(WATCHDOG_SECS, move || {
        let outer = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let inner = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        outer.install(|| {
            (0..4)
                .into_par_iter()
                .map(|i| {
                    inner.install(|| {
                        if i % 2 == 0 {
                            vec![encode_music(&pcm)]
                        } else {
                            (0..3)
                                .into_par_iter()
                                .map(|_| stream_music(&pcm, 7777))
                                .collect()
                        }
                    })
                })
                .flatten()
                .collect::<Vec<_>>()
        })
    });
    assert_eq!(got.len(), 2 + 2 * 3);
    for (i, g) in got.iter().enumerate() {
        assert!(*g == want, "nested-pool encode {i} differs from serial");
    }
}

/// An `Encoder` that starts outside any pool (batches in flight on the
/// global pool) and is then driven, and finished, from a 1-thread pool.
#[test]
fn encoder_moves_into_pool() {
    let pcm = music(3 * 44_100, 4);
    let want = encode_music(&pcm);
    let got = watchdog(WATCHDOG_SECS, move || {
        let mut enc = Encoder::new(SampleRate::Hz44100, Channels::Stereo, &Options::default());
        #[cfg(feature = "parallel")]
        enc.set_parallel_batch(3);
        let (a, b) = pcm.split_at(pcm.len() / 2);
        let mut out = enc.write(a).to_vec();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        pool.install(|| {
            out.extend_from_slice(enc.write(b));
            out.extend_from_slice(&enc.finish());
        });
        out
    });
    assert!(got == want, "encoder moved into a pool differs");
}
