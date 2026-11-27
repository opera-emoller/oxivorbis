//! `--bitrate`: a random bitrate mode per case, drawn from the case seed,
//! and its oggenc-style oracle flags (`tools/oracle-encoder/main.c`). Some
//! cases are VBR with only libvorbis's lowpass / impulse / coupling ctls
//! (`EncoderTuning`), and some bitrate modes get those too.

use crate::generate::Rng;
use oxivorbis::{BitrateMode, BitrateTuning, Channels, EncoderTuning, SampleRate};

/// One case's rate control. `mode: None` is VBR.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RateCtl {
    pub mode: Option<BitrateMode>,
    pub tuning: BitrateTuning,
    pub enc: EncoderTuning,
}

/// A mode at `rate` / `ch`: every kind, average bitrates anywhere in (and
/// occasionally just outside) the supported range, random limits, and now
/// and then reservoir tuning.
pub fn pick(seed: u64, rate: u32, ch: u16) -> RateCtl {
    let mut r = Rng::new(seed ^ 0xB17_7A7E);
    let channels = if ch == 1 {
        Channels::MONO
    } else {
        Channels::STEREO
    };
    let range = SampleRate::new(rate)
        .and_then(|sr| BitrateMode::supported_kbps(sr, channels))
        .unwrap_or(16..=128);
    let (lo, hi) = (*range.start(), *range.end());
    let kbps = |r: &mut Rng| match r.below(8) {
        0 => lo,
        1 => hi,
        2 => lo.saturating_sub(1).max(1),
        3 => hi + 1,
        _ => lo + r.below((hi - lo + 1) as usize) as u32,
    };
    // A limit around `k`: below, above, or far off.
    let around = |r: &mut Rng, k: u32, up: bool| {
        let f = [1.0, 1.0, 1.05, 1.25, 1.5, 2.0, 4.0][r.below(7)];
        let v = if up { k as f64 * f } else { k as f64 / f };
        (v as u32).max(1)
    };
    let opt = |r: &mut Rng, v: u32| (r.below(2) == 0).then_some(v);
    let vbr = r.below(8) == 0;
    let mode = match r.below(7) {
        0 => BitrateMode::Nominal { kbps: kbps(&mut r) },
        1 => BitrateMode::average(kbps(&mut r)),
        2 => {
            let k = kbps(&mut r);
            let (min, max) = (around(&mut r, k, false), around(&mut r, k, true));
            BitrateMode::Average {
                kbps: k,
                min_kbps: opt(&mut r, min),
                max_kbps: opt(&mut r, max),
            }
        }
        3 => {
            let k = kbps(&mut r);
            BitrateMode::Average {
                kbps: k,
                min_kbps: Some(k),
                max_kbps: Some(k),
            }
        }
        4 => {
            let k = kbps(&mut r);
            let (min, max) = (around(&mut r, k, false), around(&mut r, k, true));
            match r.below(3) {
                0 => BitrateMode::Limits {
                    min_kbps: Some(k),
                    max_kbps: None,
                },
                1 => BitrateMode::Limits {
                    min_kbps: None,
                    max_kbps: Some(k),
                },
                _ => BitrateMode::Limits {
                    min_kbps: Some(min),
                    max_kbps: Some(max),
                },
            }
        }
        _ => {
            let k = lo + r.below((hi - lo + 1) as usize) as u32;
            let (min, max) = (around(&mut r, k, false), around(&mut r, k, true));
            match r.below(3) {
                0 => BitrateMode::QualityLimits {
                    min_kbps: Some(min),
                    max_kbps: None,
                },
                1 => BitrateMode::QualityLimits {
                    min_kbps: None,
                    max_kbps: Some(max),
                },
                _ => BitrateMode::QualityLimits {
                    min_kbps: Some(min),
                    max_kbps: Some(max),
                },
            }
        }
    };
    let mut tuning = BitrateTuning::default();
    if r.below(5) == 0 {
        if r.below(2) == 0 {
            tuning = tuning.with_reservoir_bits([0, 1000, 8000, 64_000, 1_000_000][r.below(5)]);
        }
        if r.below(2) == 0 {
            tuning = tuning.with_reservoir_bias([0.0, 0.05, 0.5, 1.0][r.below(4)]);
        }
        if r.below(2) == 0 {
            tuning = tuning.with_average_damping([0.1, 1.0, 7.5][r.below(3)]);
        }
    }
    let mut enc = EncoderTuning::default();
    if vbr || r.below(6) == 0 {
        if r.below(2) == 0 {
            enc = enc.with_lowpass_khz([1.0, 2.0, 4.5, 11.0, 16.0, 20.5, 60.0][r.below(7)]);
        }
        if r.below(2) == 0 {
            enc = enc.with_impulse_noisetune([2.0, 0.0, -1.5, -6.0, -15.0, -30.0][r.below(6)]);
        }
        if r.below(2) == 0 {
            enc = enc.with_coupling_disabled();
        }
    }
    if vbr {
        return RateCtl {
            mode: None,
            tuning: BitrateTuning::default(),
            enc,
        };
    }
    RateCtl {
        mode: Some(mode),
        tuning,
        enc,
    }
}

/// The oracle flags oggenc would get.
pub fn oracle_args(c: &RateCtl) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let lim = |a: &mut Vec<String>, min: Option<u32>, max: Option<u32>| {
        if let Some(m) = min {
            a.extend(["-m".into(), m.to_string()]);
        }
        if let Some(m) = max {
            a.extend(["-M".into(), m.to_string()]);
        }
    };
    match c.mode {
        None => {}
        Some(BitrateMode::Nominal { kbps }) => a.extend(["-b".into(), kbps.to_string()]),
        Some(BitrateMode::Average {
            kbps,
            min_kbps,
            max_kbps,
        }) => {
            a.extend(["--managed".into(), "-b".into(), kbps.to_string()]);
            lim(&mut a, min_kbps, max_kbps);
        }
        Some(BitrateMode::Limits { min_kbps, max_kbps }) => lim(&mut a, min_kbps, max_kbps),
        Some(BitrateMode::QualityLimits { min_kbps, max_kbps }) => {
            a.push("-q".into());
            lim(&mut a, min_kbps, max_kbps);
        }
        Some(_) => unreachable!("unknown BitrateMode"),
    }
    let t = &c.tuning;
    if let Some(b) = t.reservoir_bits {
        a.extend(["-A".into(), format!("bit_reservoir_bits={b}")]);
    }
    if let Some(b) = t.reservoir_bias {
        a.extend(["-A".into(), format!("bit_reservoir_bias={b}")]);
    }
    if let Some(d) = t.average_damping {
        a.extend(["-A".into(), format!("bitrate_average_damping={d}")]);
    }
    let e = &c.enc;
    if let Some(k) = e.lowpass_khz {
        a.extend(["-A".into(), format!("lowpass_frequency={k}")]);
    }
    if let Some(d) = e.impulse_noisetune {
        a.extend(["-A".into(), format!("impulse_noisetune={d}")]);
    }
    if e.disable_coupling {
        a.extend(["-A".into(), "disable_coupling".into()]);
    }
    a
}

/// [`oracle_args`] back to a [`RateCtl`] (for `--mode` replays).
pub fn parse(spec: &str) -> Option<RateCtl> {
    let mut b = None;
    let (mut min, mut max) = (None, None);
    let (mut managed, mut q) = (false, false);
    let mut tuning = BitrateTuning::default();
    let mut enc = EncoderTuning::default();
    let mut it = spec.split_whitespace();
    while let Some(t) = it.next() {
        match t {
            "-b" => b = Some(it.next()?.parse().ok()?),
            "-m" => min = Some(it.next()?.parse().ok()?),
            "-M" => max = Some(it.next()?.parse().ok()?),
            "--managed" => managed = true,
            "-q" => q = true,
            "-A" => {
                let kv = it.next()?;
                if kv == "disable_coupling" {
                    enc.disable_coupling = true;
                    continue;
                }
                let (k, v) = kv.split_once('=')?;
                match k {
                    "lowpass_frequency" => enc.lowpass_khz = Some(v.parse().ok()?),
                    "impulse_noisetune" => enc.impulse_noisetune = Some(v.parse().ok()?),
                    "bit_reservoir_bits" => tuning.reservoir_bits = Some(v.parse().ok()?),
                    "bit_reservoir_bias" => tuning.reservoir_bias = Some(v.parse().ok()?),
                    "bitrate_average_damping" => tuning.average_damping = Some(v.parse().ok()?),
                    _ => return None,
                }
            }
            _ => return None,
        }
    }
    let any = b.is_some() || min.is_some() || max.is_some();
    if !any {
        // VBR (oggenc's quality mode; --managed alone changes nothing).
        return Some(RateCtl {
            mode: None,
            tuning,
            enc,
        });
    }
    let mode = match (q, b, managed || min.is_some() || max.is_some()) {
        (true, _, _) => BitrateMode::QualityLimits {
            min_kbps: min,
            max_kbps: max,
        },
        (false, Some(kbps), false) => BitrateMode::Nominal { kbps },
        (false, Some(kbps), true) => BitrateMode::Average {
            kbps,
            min_kbps: min,
            max_kbps: max,
        },
        (false, None, _) => BitrateMode::Limits {
            min_kbps: min,
            max_kbps: max,
        },
    };
    Some(RateCtl {
        mode: Some(mode),
        tuning,
        enc,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_round_trip() {
        for s in 0..2000 {
            let c = pick(s, [8000, 22050, 44100][s as usize % 3], 1 + (s % 2) as u16);
            assert_eq!(parse(&oracle_args(&c).join(" ")), Some(c), "{c:?}");
        }
    }
}
