//! Byte parity of the bitrate modes (oggenc `-b`, `-m`, `-M`, `--managed`
//! and `-q` with limits) against the in-tree oracle, which replays
//! oggenc 1.4.3's libvorbis setup calls (`tools/oracle-encoder/main.c`).
//!
//!   cargo nextest run --features oracle -E 'binary(parity_bitrate)'
//!
//! - `bitrate_range::<rate>::<ch>::{nominal,average}`: average bitrates
//!   across each rate's whole supported range, both ends included (at the
//!   top of the low-rate stereo ranges libvorbis switches to the uncoupled
//!   template), for every template family and the ends of each family's
//!   rate range.
//! - `bitrate_limits::<rate>::<ch>::<combo>`: hard minimum only, maximum
//!   only and both, with and without an average, and with quality mode;
//!   tight limits force libvorbis to pad and truncate packets.
//! - `bitrate_tuning::*`: the reservoir knobs.
//! - `bitrate_long::*`: 30-120 s inputs, so the reservoirs fill, drain and
//!   clamp many times over.
//! - `bitrate_rejects_like_libvorbis`: requests outside the templates fail
//!   on both sides.

#![cfg(feature = "oracle")]

mod common;

use common::{Sig, extract_comment_strings, extract_serial, first_diff, signal_i16};
use oxivorbis::{BitrateMode, BitrateTuning, Channels, SampleRate};
use std::io::Write;
use std::process::{Command, Stdio};

/// The oracle arguments oggenc would get for `mode` / `tuning`.
fn oracle_args(mode: BitrateMode, tuning: &BitrateTuning) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let lim = |a: &mut Vec<String>, min: Option<u32>, max: Option<u32>| {
        if let Some(m) = min {
            a.extend(["-m".into(), m.to_string()]);
        }
        if let Some(m) = max {
            a.extend(["-M".into(), m.to_string()]);
        }
    };
    match mode {
        BitrateMode::Nominal { kbps } => a.extend(["-b".into(), kbps.to_string()]),
        BitrateMode::Average {
            kbps,
            min_kbps,
            max_kbps,
        } => {
            a.extend(["--managed".into(), "-b".into(), kbps.to_string()]);
            lim(&mut a, min_kbps, max_kbps);
        }
        BitrateMode::Limits { min_kbps, max_kbps } => lim(&mut a, min_kbps, max_kbps),
        BitrateMode::QualityLimits { min_kbps, max_kbps } => {
            a.push("-q".into());
            lim(&mut a, min_kbps, max_kbps);
        }
        _ => unreachable!("new mode"),
    }
    if let Some(b) = tuning.reservoir_bits {
        a.extend(["-A".into(), format!("bit_reservoir_bits={b}")]);
    }
    if let Some(b) = tuning.reservoir_bias {
        a.extend(["-A".into(), format!("bit_reservoir_bias={b}")]);
    }
    if let Some(d) = tuning.average_damping {
        a.extend(["-A".into(), format!("bitrate_average_damping={d}")]);
    }
    a
}

/// The oracle's stream, or `None` if it (libvorbis) refused the setup.
fn oracle(
    pcm: &[i16],
    rate: u32,
    ch: u16,
    base_quality: f32,
    mode: BitrateMode,
    tuning: &BitrateTuning,
) -> Option<Vec<u8>> {
    let raw: Vec<u8> = pcm.iter().flat_map(|&s| s.to_le_bytes()).collect();
    let bin = std::env::current_dir()
        .unwrap()
        .join("tools/oracle-encoder/oracle-encoder");
    let mut child = Command::new(&bin)
        .args(oracle_args(mode, tuning))
        .arg(rate.to_string())
        .arg(ch.to_string())
        .arg(base_quality.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn oracle-encoder at {}: {e}", bin.display()));
    let mut stdin = child.stdin.take().unwrap();
    let feeder = std::thread::spawn(move || {
        // The oracle exits early (EPIPE) when it rejects the setup.
        let _ = stdin.write_all(&raw);
    });
    let out = child.wait_with_output().unwrap();
    feeder.join().unwrap();
    out.status.success().then_some(out.stdout)
}

/// Encode with both and compare bytes. Returns the stream length, or
/// `None` when both reject the request.
fn check(
    pcm: &[i16],
    rate: SampleRate,
    channels: Channels,
    base_quality: f32,
    mode: BitrateMode,
    tuning: BitrateTuning,
) -> Option<usize> {
    let ch = channels.count();
    let theirs = oracle(pcm, rate.hz(), ch, base_quality, mode, &tuning);
    let (serial, vendor) = match &theirs {
        Some(t) => (extract_serial(t), extract_comment_strings(t).0),
        None => (1, Vec::new()),
    };
    let ours = oxivorbis::encode_bitrate_with_meta(
        pcm,
        rate,
        channels,
        base_quality,
        mode,
        tuning,
        serial,
        Some(&vendor),
    );
    let (ours, theirs) = match (ours, theirs) {
        (Ok(o), Some(t)) => (o, t),
        (Err(_), None) => return None,
        (o, t) => panic!(
            "{rate} {channels:?} q{base_quality} {mode:?} {tuning:?}: oxivorbis {}, oracle {}",
            match o {
                Ok(_) => "ok".to_string(),
                Err(e) => format!("rejected ({e})"),
            },
            if t.is_some() { "ok" } else { "rejected" }
        ),
    };
    if ours != theirs {
        let d = first_diff(&ours, &theirs);
        panic!(
            "{rate} {channels:?} q{base_quality} {mode:?} {tuning:?}: diverged at byte {d} of {} (oracle {})",
            ours.len(),
            theirs.len()
        );
    }
    Some(ours.len())
}

/// Two-second stretches of tone, noise, transients and silence, so the
/// reservoirs see both cheap and expensive passages.
fn music(secs: f64, ch: usize, rate: u32, seed: u64) -> Vec<i16> {
    let frames = (secs * rate as f64) as usize;
    let seg = (rate as usize * 2).max(1);
    let kinds = [
        Sig::Sine,
        Sig::Noise,
        Sig::Transients,
        Sig::Silence,
        Sig::Noise,
        Sig::Transients,
    ];
    let mut out = Vec::with_capacity(frames * ch);
    let mut s = seed;
    let mut i = 0;
    while i < frames {
        let n = seg.min(frames - i);
        let k = kinds[(s % kinds.len() as u64) as usize];
        out.extend(signal_i16(k, n, ch, rate, s));
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407)
            >> 7;
        i += n;
    }
    out
}

fn rate(hz: u32) -> SampleRate {
    SampleRate::new(hz).unwrap()
}

fn supported(hz: u32, ch: Channels) -> (u32, u32) {
    let r = BitrateMode::supported_kbps(rate(hz), ch).expect("rate with bitrate setups");
    (*r.start(), *r.end())
}

/// Average bitrates spanning the supported range, ends included.
fn range_points(hz: u32, ch: Channels) -> Vec<u32> {
    let (lo, hi) = supported(hz, ch);
    let mut v = vec![
        lo,
        lo + (hi - lo) / 5,
        lo + (hi - lo) / 2,
        lo + (hi - lo) * 4 / 5,
        hi,
    ];
    v.dedup();
    v
}

fn run_range(hz: u32, ch: Channels, average: bool) {
    let pcm = music(5.0, ch.count() as usize, hz, hz as u64 ^ 0xB17);
    for k in range_points(hz, ch) {
        let mode = if average {
            BitrateMode::average(k)
        } else {
            BitrateMode::Nominal { kbps: k }
        };
        check(&pcm, rate(hz), ch, 0.5, mode, BitrateTuning::default())
            .unwrap_or_else(|| panic!("{hz} {ch:?} {k} kb/s rejected"));
    }
}

mod bitrate_range {
    use super::*;

    macro_rules! range {
        ($name:ident, $hz:expr) => {
            mod $name {
                use super::*;
                mod mono {
                    use super::*;
                    #[test]
                    fn nominal() {
                        run_range($hz, Channels::Mono, false)
                    }
                    #[test]
                    fn average() {
                        run_range($hz, Channels::Mono, true)
                    }
                }
                mod stereo {
                    use super::*;
                    #[test]
                    fn nominal() {
                        run_range($hz, Channels::Stereo, false)
                    }
                    #[test]
                    fn average() {
                        run_range($hz, Channels::Stereo, true)
                    }
                }
            }
        };
    }

    // One per template family, then the ends of each family's rate range.
    range!(r08000_8, 8000);
    range!(r11025_11, 11025);
    range!(r16000_16, 16000);
    range!(r22050_22, 22050);
    range!(r32000_32, 32000);
    range!(r44100_44, 44100);
    range!(r48000_44, 48000);
    range!(r08999, 8999);
    range!(r09000, 9000);
    range!(r14999, 14999);
    range!(r15000, 15000);
    range!(r18999, 18999);
    range!(r19000, 19000);
    range!(r25999, 25999);
    range!(r26000, 26000);
    range!(r39999, 39999);
    range!(r40000, 40000);
    range!(r50000, 50000);
}

/// Limit combinations relative to the supported range's midpoint `avg`.
#[derive(Clone, Copy, Debug)]
enum Combo {
    /// --managed -b avg -m (avg*3/4)
    AvgMin,
    /// --managed -b avg -M (avg*5/4)
    AvgMax,
    /// --managed -b avg -m (avg*3/4) -M (avg*5/4)
    AvgMinMax,
    /// --managed -b avg -m avg -M avg ("CBR")
    Cbr,
    /// -m avg (no -b): setup for the minimum
    Min,
    /// -M avg: setup for 87.5 % of the maximum
    Max,
    /// -m (avg/2) -M avg: setup for the midpoint
    MinMax,
    /// -q Q -m avg
    QMin(f32),
    /// -q Q -M avg
    QMax(f32),
    /// -q Q -m (avg/2) -M avg
    QMinMax(f32),
}

fn run_limits(hz: u32, ch: Channels, combo: Combo) {
    let (lo, hi) = supported(hz, ch);
    let avg = (lo + hi) / 2;
    // Quieter material than `run_range` uses would rarely hit a minimum.
    let pcm = music(8.0, ch.count() as usize, hz, 0x5EED ^ hz as u64);
    let (q, mode) = match combo {
        Combo::AvgMin => (
            0.5,
            BitrateMode::Average {
                kbps: avg,
                min_kbps: Some(avg * 3 / 4),
                max_kbps: None,
            },
        ),
        Combo::AvgMax => (
            0.5,
            BitrateMode::Average {
                kbps: avg,
                min_kbps: None,
                max_kbps: Some(avg * 5 / 4),
            },
        ),
        Combo::AvgMinMax => (
            0.5,
            BitrateMode::Average {
                kbps: avg,
                min_kbps: Some(avg * 3 / 4),
                max_kbps: Some(avg * 5 / 4),
            },
        ),
        Combo::Cbr => (
            0.5,
            BitrateMode::Average {
                kbps: avg,
                min_kbps: Some(avg),
                max_kbps: Some(avg),
            },
        ),
        Combo::Min => (
            0.5,
            BitrateMode::Limits {
                min_kbps: Some(avg),
                max_kbps: None,
            },
        ),
        Combo::Max => (
            0.5,
            BitrateMode::Limits {
                min_kbps: None,
                max_kbps: Some(avg),
            },
        ),
        Combo::MinMax => (
            0.5,
            BitrateMode::Limits {
                min_kbps: Some(avg / 2),
                max_kbps: Some(avg),
            },
        ),
        Combo::QMin(q) => (
            q,
            BitrateMode::QualityLimits {
                min_kbps: Some(avg),
                max_kbps: None,
            },
        ),
        Combo::QMax(q) => (
            q,
            BitrateMode::QualityLimits {
                min_kbps: None,
                max_kbps: Some(avg),
            },
        ),
        Combo::QMinMax(q) => (
            q,
            BitrateMode::QualityLimits {
                min_kbps: Some(avg / 2),
                max_kbps: Some(avg),
            },
        ),
    };
    check(&pcm, rate(hz), ch, q, mode, BitrateTuning::default())
        .unwrap_or_else(|| panic!("{hz} {ch:?} {combo:?} rejected"));
}

mod bitrate_limits {
    use super::*;

    macro_rules! combos {
        ($hz:expr, $ch:expr) => {
            #[test]
            fn avg_min() {
                run_limits($hz, $ch, Combo::AvgMin)
            }
            #[test]
            fn avg_max() {
                run_limits($hz, $ch, Combo::AvgMax)
            }
            #[test]
            fn avg_min_max() {
                run_limits($hz, $ch, Combo::AvgMinMax)
            }
            #[test]
            fn cbr() {
                run_limits($hz, $ch, Combo::Cbr)
            }
            #[test]
            fn min() {
                run_limits($hz, $ch, Combo::Min)
            }
            #[test]
            fn max() {
                run_limits($hz, $ch, Combo::Max)
            }
            #[test]
            fn min_max() {
                run_limits($hz, $ch, Combo::MinMax)
            }
            #[test]
            fn q_m1_max() {
                run_limits($hz, $ch, Combo::QMax(-0.1))
            }
            #[test]
            fn q3_min() {
                run_limits($hz, $ch, Combo::QMin(0.3))
            }
            #[test]
            fn q5_max() {
                run_limits($hz, $ch, Combo::QMax(0.5))
            }
            #[test]
            fn q5_min_max() {
                run_limits($hz, $ch, Combo::QMinMax(0.5))
            }
            #[test]
            fn q10_min_max() {
                run_limits($hz, $ch, Combo::QMinMax(1.0))
            }
        };
    }

    macro_rules! rates {
        ($name:ident, $hz:expr) => {
            mod $name {
                use super::*;
                mod mono {
                    use super::*;
                    combos!($hz, Channels::Mono);
                }
                mod stereo {
                    use super::*;
                    combos!($hz, Channels::Stereo);
                }
            }
        };
    }

    rates!(r08000, 8000);
    rates!(r11025, 11025);
    rates!(r16000, 16000);
    rates!(r22050, 22050);
    rates!(r32000, 32000);
    rates!(r44100, 44100);
    rates!(r48000, 48000);
}

mod bitrate_tuning {
    use super::*;

    fn run(hz: u32, ch: Channels, mode: BitrateMode, t: BitrateTuning) {
        let pcm = music(8.0, ch.count() as usize, hz, 77);
        check(&pcm, rate(hz), ch, 0.5, mode, t).expect("accepted");
    }

    const CBR128: BitrateMode = BitrateMode::Average {
        kbps: 128,
        min_kbps: Some(128),
        max_kbps: Some(128),
    };

    #[test]
    fn small_reservoir() {
        let t = BitrateTuning::default().with_reservoir_bits(4000);
        run(44100, Channels::Stereo, CBR128, t);
    }
    #[test]
    fn large_reservoir() {
        let t = BitrateTuning::default().with_reservoir_bits(4_000_000);
        run(44100, Channels::Stereo, CBR128, t);
    }
    #[test]
    fn zero_reservoir_keeps_managed_setup() {
        let t = BitrateTuning::default().with_reservoir_bits(0);
        run(44100, Channels::Stereo, BitrateMode::average(128), t);
    }
    #[test]
    fn bias_0_and_1() {
        for b in [0.0, 1.0] {
            let t = BitrateTuning::default().with_reservoir_bias(b);
            run(22050, Channels::Mono, BitrateMode::average(40), t);
        }
    }
    #[test]
    fn damping() {
        for d in [0.05, 0.75, 20.0] {
            let t = BitrateTuning::default().with_average_damping(d);
            run(48000, Channels::Stereo, BitrateMode::average(160), t);
        }
    }
    #[test]
    fn all_three_on_limits() {
        let t = BitrateTuning::default()
            .with_reservoir_bits(64_000)
            .with_reservoir_bias(0.6)
            .with_average_damping(3.0);
        let m = BitrateMode::Limits {
            min_kbps: Some(48),
            max_kbps: Some(96),
        };
        run(32000, Channels::Stereo, m, t);
    }
    #[test]
    fn on_nominal_and_quality_limits() {
        let t = BitrateTuning::default().with_reservoir_bias(0.3);
        run(16000, Channels::Mono, BitrateMode::Nominal { kbps: 40 }, t);
        let m = BitrateMode::QualityLimits {
            min_kbps: None,
            max_kbps: Some(40),
        };
        run(16000, Channels::Mono, m, t);
    }
}

mod bitrate_long {
    use super::*;

    fn run(secs: f64, hz: u32, ch: Channels, q: f32, mode: BitrateMode) {
        let pcm = music(secs, ch.count() as usize, hz, secs as u64 * 31 + hz as u64);
        check(&pcm, rate(hz), ch, q, mode, BitrateTuning::default()).expect("accepted");
    }

    #[test]
    fn cbr128_stereo44_90s() {
        let m = BitrateMode::Average {
            kbps: 128,
            min_kbps: Some(128),
            max_kbps: Some(128),
        };
        run(90.0, 44100, Channels::Stereo, 0.5, m);
    }
    #[test]
    fn abr192_max256_stereo48_60s() {
        let m = BitrateMode::Average {
            kbps: 192,
            min_kbps: None,
            max_kbps: Some(256),
        };
        run(60.0, 48000, Channels::Stereo, 0.5, m);
    }
    #[test]
    fn abr96_stereo44_120s() {
        run(
            120.0,
            44100,
            Channels::Stereo,
            0.5,
            BitrateMode::average(96),
        );
    }
    #[test]
    fn q5_max128_stereo44_60s() {
        let m = BitrateMode::QualityLimits {
            min_kbps: None,
            max_kbps: Some(128),
        };
        run(60.0, 44100, Channels::Stereo, 0.5, m);
    }
    #[test]
    fn max32_mono22_120s() {
        let m = BitrateMode::Limits {
            min_kbps: None,
            max_kbps: Some(32),
        };
        run(120.0, 22050, Channels::Mono, 0.5, m);
    }
    #[test]
    fn min64_mono44_45s() {
        let m = BitrateMode::Limits {
            min_kbps: Some(64),
            max_kbps: None,
        };
        run(45.0, 44100, Channels::Mono, 0.5, m);
    }
    #[test]
    fn abr16_mono8_120s() {
        run(120.0, 8000, Channels::Mono, 0.5, BitrateMode::average(16));
    }
    #[test]
    fn nominal80_stereo32_30s() {
        run(
            30.0,
            32000,
            Channels::Stereo,
            0.5,
            BitrateMode::Nominal { kbps: 80 },
        );
    }
    #[test]
    fn uncoupled_abr176_stereo22_60s() {
        // 88 kb/s per channel: only 22_uncoupled covers it.
        run(
            60.0,
            22050,
            Channels::Stereo,
            0.5,
            BitrateMode::average(176),
        );
    }
}

/// Requests libvorbis has no setup for fail on both sides.
#[test]
fn bitrate_rejects_like_libvorbis() {
    let t = BitrateTuning::default();
    for (hz, ch, mode) in [
        (44100, Channels::Stereo, BitrateMode::average(44)),
        (44100, Channels::Stereo, BitrateMode::average(501)),
        (44100, Channels::Mono, BitrateMode::Nominal { kbps: 300 }),
        (96000, Channels::Stereo, BitrateMode::average(128)),
        (7999, Channels::Mono, BitrateMode::average(16)),
        (
            50001,
            Channels::Stereo,
            BitrateMode::QualityLimits {
                min_kbps: None,
                max_kbps: Some(128),
            },
        ),
        (
            44100,
            Channels::Stereo,
            BitrateMode::Limits {
                min_kbps: None,
                max_kbps: Some(600),
            },
        ),
    ] {
        let ch_n = ch.count() as usize;
        let pcm = music(1.0, ch_n, hz, 3);
        assert_eq!(
            check(&pcm, rate(hz), ch, 0.5, mode, t),
            None,
            "{hz} {mode:?}"
        );
    }
}
