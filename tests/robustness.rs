//! Robustness tests: encodes driven from inside Rayon pools (the
//! `parallel` feature must not deadlock there), adversarial f32 input,
//! comment sanitizing, and the concurrency of the setup caches.
//!
//! Everything here runs in both the serial and the `parallel` build.

use std::sync::mpsc;
use std::time::Duration;

use oxivorbis::{Channels, Encoder, Options, Quality, SampleRate};
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
    oxivorbis::encode(
        pcm,
        SampleRate::HZ_44100,
        Channels::Stereo,
        &Options::default(),
    )
}

/// Streaming encode with a small parallel batch so a short input spans
/// many batches.
fn stream_music(pcm: &[i16], chunk: usize) -> Vec<u8> {
    let mut enc = Encoder::new(SampleRate::HZ_44100, Channels::Stereo, &Options::default());
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
        let mut enc = Encoder::new(SampleRate::HZ_44100, Channels::Stereo, &Options::default());
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

/// `Encoder` is `Send + Sync` in every feature combination (checked at
/// compile time; this file is built with and without `parallel`).
#[test]
fn encoder_is_send_sync() {
    const fn send_sync<T: Send + Sync>() {}
    const _: () = send_sync::<Encoder>();
    const _: () = send_sync::<Options>();
    // Runtime calls too, so older rustc (1.85) doesn't flag the helper as
    // dead code under -D warnings.
    send_sync::<Encoder>();
    send_sync::<Options>();
    // And shareable by reference across threads.
    let enc = Encoder::new(SampleRate::HZ_44100, Channels::Mono, &Options::default());
    std::thread::scope(|s| {
        s.spawn(|| {
            let _: &Encoder = &enc;
        });
    });
}

/// Granule position of the last page in `pages` (whole Ogg pages), if
/// any page carries one.
fn last_granule(pages: &[u8]) -> Option<u64> {
    let mut p = 0;
    let mut last = None;
    while p + 27 <= pages.len() {
        assert_eq!(&pages[p..p + 4], b"OggS");
        let gp = u64::from_le_bytes(pages[p + 6..p + 14].try_into().unwrap());
        if gp != u64::MAX {
            last = Some(gp);
        }
        let nseg = pages[p + 26] as usize;
        let body: usize = pages[p + 27..p + 27 + nseg]
            .iter()
            .map(|&b| b as usize)
            .sum();
        p += 27 + nseg + body;
    }
    assert_eq!(p, pages.len(), "write returned a partial page");
    last
}

/// The per-(rate, channels, quality) setup cache (codebooks, psy lookups)
/// under contention: 32 threads released together, each encoding at
/// settings from a pool of 24 keys (three times the cache capacity, so
/// entries are evicted and rebuilt concurrently), must match encodes
/// made one at a time.
#[test]
fn setup_cache_is_thread_safe() {
    let mut keys = Vec::new();
    for (rate, channels) in CONFIGS {
        for q in [-1.0f32, 0.0, 2.5, 5.0, 7.7, 10.0] {
            keys.push((rate, channels, q));
        }
    }
    let pcm = music(11_025, 6); // 0.25 s stereo; mono uses its first half
    let enc = |&(rate, channels, q): &(SampleRate, Channels, f32)| {
        let frames = pcm.len() / 2;
        let input = if channels == Channels::Mono {
            &pcm[..frames]
        } else {
            &pcm[..]
        };
        let opts = Options::default().with_quality(Quality::new(q).unwrap());
        oxivorbis::encode(input, rate, channels, &opts)
    };
    let want: Vec<Vec<u8>> = keys.iter().map(enc).collect();
    let threads = 32;
    let barrier = std::sync::Barrier::new(threads);
    std::thread::scope(|s| {
        for t in 0..threads {
            let (keys, want, enc, barrier) = (&keys, &want, &enc, &barrier);
            s.spawn(move || {
                barrier.wait();
                for i in 0..keys.len() {
                    let k = (t * 7 + i * 5) % keys.len();
                    assert!(
                        enc(&keys[k]) == want[k],
                        "thread {t}: key {:?} differs",
                        keys[k]
                    );
                }
            });
        }
    });
}

/// With `parallel`, how far `write`'s output trails the input must not
/// grow with the thread count: an encoder sized for a 64-thread pool
/// still returns pages within ~16 s of audio (the batch cap; 3 batches of
/// at most 256 long blocks in flight), at q5 and q-1 (4096-sample blocks).
/// The serial build trails by well under a second.
#[test]
#[cfg_attr(debug_assertions, ignore = "45 s of audio; run with --release")]
fn streaming_lag_is_bounded() {
    let pcm = music(45 * 44_100, 5);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(64)
        .build()
        .unwrap();
    for q in [5.0f32, -1.0] {
        let opts = Options::default().with_quality(Quality::new(q).unwrap());
        // Sized inside the pool (64 threads), driven from this thread.
        let mut enc = pool.install(|| Encoder::new(SampleRate::HZ_44100, Channels::Stereo, &opts));
        let mut fed = 0usize;
        let mut emitted = 0u64;
        let mut max_lag = 0usize;
        for chunk in pcm.chunks(4096 * 2) {
            fed += chunk.len() / 2;
            if let Some(gp) = last_granule(enc.write(chunk)) {
                emitted = gp;
            }
            max_lag = max_lag.max(fed - emitted as usize);
        }
        enc.finish();
        let lag_s = max_lag as f64 / 44_100.0;
        let bound = if cfg!(feature = "parallel") {
            17.0
        } else {
            1.0
        };
        assert!(
            lag_s < bound,
            "q{q}: output lagged {lag_s:.1} s > {bound} s"
        );
    }
}

// ---------------------------------------------------------------------------
// Adversarial f32 input
// ---------------------------------------------------------------------------

const CONFIGS: [(SampleRate, Channels); 4] = [
    (SampleRate::HZ_44100, Channels::Stereo),
    (SampleRate::HZ_44100, Channels::Mono),
    (SampleRate::HZ_48000, Channels::Stereo),
    (SampleRate::HZ_48000, Channels::Mono),
];

/// Decode `ogg` with lewton; panics if it isn't a valid stream. Returns
/// the stream length in frames (the final granule position; lewton itself
/// doesn't trim the last packet to it).
fn decode(ogg: &[u8]) -> usize {
    let mut rdr = lewton::inside_ogg::OggStreamReader::new(std::io::Cursor::new(ogg))
        .expect("lewton rejects the headers");
    let mut decoded = 0;
    while let Some(pcm) = rdr.read_dec_packet().expect("lewton rejects a packet") {
        decoded += pcm[0].len();
    }
    let len = rdr.get_last_absgp().unwrap_or(0) as usize;
    assert!(decoded >= len, "decoded {decoded} frames < granule {len}");
    len
}

/// Named adversarial signals, `frames` frames of `ch` channels each.
fn adversarial(frames: usize, ch: usize) -> Vec<(&'static str, Vec<f32>)> {
    let n = frames * ch;
    let mut rng = Rng(0xADD);
    let sign = |i: usize| if i % 2 == 0 { 1.0f32 } else { -1.0 };
    let mut v = vec![
        ("const +1e30", vec![1e30; n]),
        ("const -f32::MAX", vec![-f32::MAX; n]),
        (
            "alternating ±f32::MAX per frame",
            (0..n).map(|i| sign(i / ch) * f32::MAX).collect(),
        ),
        (
            "alternating ±1e30 per sample",
            (0..n).map(|i| sign(i) * 1e30).collect(),
        ),
        (
            "impulses ±f32::MAX in silence",
            (0..n)
                .map(|i| {
                    if (i / ch) % 1500 == 0 {
                        sign(i / 1500) * f32::MAX
                    } else {
                        0.0
                    }
                })
                .collect(),
        ),
        (
            "sweep 1 to f32::MAX",
            (0..n)
                .map(|i| sign(i / 3) * 2f32.powf(128.0 * (i / ch) as f32 / frames as f32))
                .map(|x| x.clamp(f32::MIN, f32::MAX))
                .collect(),
        ),
    ];
    v.push((
        "impulses ±3.3e7 in quiet noise",
        (0..n)
            .map(|i| {
                if (i / ch) % 777 == 0 {
                    sign(i / 777) * 3.3e7
                } else {
                    rng.unit() * 1e-3
                }
            })
            .collect(),
    ));
    v.push((
        "random sign and exponent",
        (0..n)
            .map(|_| {
                let bits = rng.next();
                let exp = (bits % 254) as u32 + 1; // any normal exponent
                f32::from_bits(((bits >> 32) as u32 & 0x8000_0000) | (exp << 23))
            })
            .collect(),
    ));
    v.push((
        "random magnitude up to 1e38",
        (0..n)
            .map(|_| rng.unit() * 10f32.powf(rng.unit().abs() * 38.0))
            .collect(),
    ));
    v
}

/// Extreme finite f32 at q-1/0/5/10: no panic (debug builds check
/// integer overflow) and a stream lewton decodes to the right length.
fn adversarial_f32_decodes(rate: SampleRate, channels: Channels) {
    // Shorter in debug builds, which are slow.
    let frames = if cfg!(debug_assertions) {
        9_000
    } else {
        30_000
    };
    let ch = channels.count() as usize;
    for (name, pcm) in adversarial(frames, ch) {
        for q in [-1.0f32, 0.0, 5.0, 10.0] {
            let opts = Options::default().with_quality(Quality::new(q).unwrap());
            let ogg = oxivorbis::encode_f32(&pcm, rate, channels, &opts);
            let got = decode(&ogg);
            assert_eq!(
                got, frames,
                "{name} q{q} {rate:?} {channels:?}: decoded {got} frames"
            );
        }
    }
}

/// A single huge sample, alone and in tiny inputs.
fn adversarial_f32_tiny_inputs(rate: SampleRate, channels: Channels) {
    let ch = channels.count() as usize;
    for v in [
        3.3e7f32,
        -3.3e7,
        1e30,
        f32::MAX,
        f32::MIN,
        f32::MIN_POSITIVE,
    ] {
        for frames in [1usize, 2, 64, 300] {
            let pcm = vec![v; frames * ch];
            for q in [-1.0f32, 0.0, 5.0, 10.0] {
                let opts = Options::default().with_quality(Quality::new(q).unwrap());
                let ogg = oxivorbis::encode_f32(&pcm, rate, channels, &opts);
                let got = decode(&ogg);
                assert_eq!(got, frames, "{v} x{frames} q{q} {rate:?} {channels:?}");
            }
        }
    }
}

/// One test per config, so the test runner can spread them over cores.
macro_rules! adversarial_tests {
    ($($config:ident: $rate:ident, $ch:ident;)*) => {
        mod adversarial_f32 {
            use super::*;
            $(
                mod $config {
                    use super::*;
                    #[test]
                    fn decodes() {
                        adversarial_f32_decodes(SampleRate::$rate, Channels::$ch);
                    }
                    #[test]
                    fn tiny_inputs() {
                        adversarial_f32_tiny_inputs(SampleRate::$rate, Channels::$ch);
                    }
                }
            )*
        }
    };
}
adversarial_tests! {
    hz44100_stereo: Hz44100, Stereo;
    hz44100_mono: Hz44100, Mono;
    hz48000_stereo: Hz48000, Stereo;
    hz48000_mono: Hz48000, Mono;
}

// ---------------------------------------------------------------------------
// Comment header
// ---------------------------------------------------------------------------

/// Encode a short clip with `opts` and return lewton's view of the
/// comment header: (vendor, [(key, value)]).
fn decoded_comments(opts: &Options) -> (String, Vec<(String, String)>) {
    let ogg = oxivorbis::encode(&[0i16; 4410], SampleRate::HZ_44100, Channels::Mono, opts);
    let rdr = lewton::inside_ogg::OggStreamReader::new(std::io::Cursor::new(&ogg))
        .expect("lewton rejects the headers");
    (rdr.comment_hdr.vendor, rdr.comment_hdr.comment_list)
}

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Keys are reduced to the bytes the Vorbis comment spec allows (ASCII
/// 0x20..=0x7D except '='); comments left without a key are skipped;
/// values and the vendor string are written as given.
#[test]
fn comment_keys_are_sanitized() {
    let opts = Options::default()
        .with_vendor("vendör = 1.0")
        .with_comment("TITLE", "Example")
        .with_comment("A=B", "v")
        .with_comment("", "empty key")
        .with_comment("==", "only equals")
        .with_comment("TÍTLE", "non-ASCII key")
        .with_comment("ÄÖ", "nothing left")
        .with_comment("\tKEY\n\u{7f}~", "controls, DEL and ~ dropped")
        .with_comment("SP ACE}", "space and } kept")
        .with_comment("K", "a=b=c ü\u{0}");
    let (vendor, comments) = decoded_comments(&opts);
    assert_eq!(vendor, "vendör = 1.0");
    assert_eq!(
        comments,
        pairs(&[
            ("TITLE", "Example"),
            ("AB", "v"),
            ("TTLE", "non-ASCII key"),
            ("KEY", "controls, DEL and ~ dropped"),
            ("SP ACE}", "space and } kept"),
            ("K", "a=b=c ü\u{0}"),
        ])
    );
}

/// Valid keys are untouched, and setting no comments writes none.
#[test]
fn comment_defaults_and_valid_keys() {
    let (vendor, comments) = decoded_comments(&Options::default());
    assert_eq!(vendor, oxivorbis::DEFAULT_VENDOR);
    assert!(comments.is_empty());

    let printable: String = (0x20u8..=0x7D)
        .filter(|&b| b != b'=')
        .map(char::from)
        .collect();
    let opts = Options::default()
        .with_comment(printable.clone(), "all allowed bytes")
        .with_comment("artist", "lowercase kept");
    let (_, comments) = decoded_comments(&opts);
    assert_eq!(
        comments,
        vec![
            (printable, "all allowed bytes".to_string()),
            ("artist".to_string(), "lowercase kept".to_string()),
        ]
    );
}
