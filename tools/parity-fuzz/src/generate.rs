//! Seeded, reproducible signal generators.
//!
//! A [`Case`] is a list of [`Segment`]s concatenated in time. Each segment
//! owns its own seed, so rendering is a pure function of the case
//! description; the minimizer relies on that to re-render simplified
//! variants.
//!
//! All transcendental math goes through [`crate::det`] (no host libm), so
//! a seed renders the same PCM on every platform.

use crate::det;
use std::f64::consts::PI;

/// SplitMix64. Small, fast, and good enough for test-signal generation.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, 1).
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.f64()
    }
    /// Uniform in [0, n). `n` must be > 0.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
    pub fn chance(&mut self, p: f64) -> bool {
        self.f64() < p
    }
    pub fn gauss(&mut self) -> f64 {
        let u1 = self.f64().max(1e-300);
        let u2 = self.f64();
        (-2.0 * det::ln(u1)).sqrt() * det::cos(2.0 * PI * u2)
    }
    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
    /// Log-uniform amplitude in [10^(lo_db/20), 10^(hi_db/20)].
    pub fn amp_db(&mut self, lo_db: f64, hi_db: f64) -> f64 {
        det::powf(10.0, self.range(lo_db, hi_db) / 20.0)
    }
}

pub fn mix_seed(a: u64, b: u64) -> u64 {
    let mut r = Rng::new(a ^ b.wrapping_mul(0xD6E8_FEB8_6659_FD93));
    r.next_u64()
}

/// Amplitudes are in units of full scale (1.0 == 32768).
#[derive(Clone, Debug)]
pub enum Gen {
    Silence,
    White {
        amp: f64,
        gaussian: bool,
    },
    Pink {
        amp: f64,
    },
    Brown {
        amp: f64,
    },
    /// (freq Hz, amp, phase rad)
    Sines {
        partials: Vec<(f64, f64, f64)>,
    },
    Chirp {
        f0: f64,
        f1: f64,
        amp: f64,
        log: bool,
    },
    /// Naive (aliasing) square.
    Square {
        freq: f64,
        amp: f64,
    },
    /// Naive (aliasing) sawtooth.
    Saw {
        freq: f64,
        amp: f64,
    },
    /// Periodic clicks; `alternate` flips polarity each click.
    ImpulseTrain {
        period: usize,
        amp: f64,
        width: usize,
        alternate: bool,
    },
    /// Silence with `count` randomly placed single-sample clicks.
    SparseClicks {
        count: usize,
        amp: f64,
    },
    /// Random ±lsb integer dither (near-silence).
    Dither {
        lsb: u8,
    },
    /// Amplitude jumps every `step` frames, cycling through `levels`.
    LevelSteps {
        inner: Box<Gen>,
        step: usize,
        levels: Vec<f64>,
    },
    /// Noise burst on top of a tone: forces transient detection mid-tone.
    ToneWithBursts {
        freq: f64,
        amp: f64,
        burst_every: usize,
        burst_len: usize,
        burst_amp: f64,
    },
}

#[derive(Clone, Debug)]
pub enum Stereo {
    Identical,
    Inverted,
    /// Same generator, independent seed for the right channel.
    Uncorrelated,
    LeftOnly,
    RightOnly,
    /// Right = left * k.
    Scaled(f64),
    /// Right channel uses a different generator.
    Different(Gen),
}

#[derive(Clone, Debug)]
pub struct Segment {
    pub generator: Gen,
    pub frames: usize,
    /// Linear gain applied after generation; >1 drives into clipping.
    pub gain: f64,
    /// DC offset in full-scale units, added after gain.
    pub dc: f64,
    pub stereo: Stereo,
    pub seed: u64,
}

#[derive(Clone, Debug)]
pub struct Case {
    pub rate: u32,
    pub channels: u16,
    pub segments: Vec<Segment>,
    /// Free-form label of how the case was constructed.
    pub kind: &'static str,
    /// f32 mode only: how the rendered signal becomes `write_f32` input.
    pub f32: F32Shape,
}

/// How [`Case::render_f32`] maps the rendered signal (full scale = 1.0) to
/// f32 samples. Values stay finite and within the documented parity range
/// of +-1e4.
#[derive(Clone, Debug)]
pub struct F32Shape {
    /// Multiplier applied before the cast (1.0 = nominal range).
    pub scale: f64,
    /// Number of samples overwritten with special finite values
    /// (subnormals, -0.0, +-1e4, ...), at positions drawn from `seed`.
    pub specials: usize,
    pub seed: u64,
}

impl Default for F32Shape {
    fn default() -> Self {
        F32Shape {
            scale: 1.0,
            specials: 0,
            seed: 0,
        }
    }
}

/// The documented parity range for `write_f32` input.
pub const F32_LIMIT: f64 = 1.0e4;

const F32_SPECIALS: &[f32] = &[
    -0.0,
    0.0,
    1.0e-45, // smallest subnormal
    -1.0e-45,
    1.0e-40,
    -3.0e-39,
    f32::MIN_POSITIVE,
    -f32::MIN_POSITIVE,
    1.0,
    -1.0,
    1.000_000_1,
    32767.0 / 32768.0,
    1.0e4,
    -1.0e4,
    9999.999,
    -0.5e4,
];

impl Case {
    pub fn frames(&self) -> usize {
        self.segments.iter().map(|s| s.frames).sum()
    }

    /// Render to interleaved i16.
    pub fn render(&self) -> Vec<i16> {
        let ch = self.channels as usize;
        let mut out = Vec::with_capacity(self.frames() * ch);
        for seg in &self.segments {
            let mut rng = Rng::new(seg.seed);
            let l = render_gen(&seg.generator, seg.frames, self.rate, &mut rng);
            if ch == 1 {
                out.extend(l.iter().map(|&x| quant(x * seg.gain + seg.dc)));
                continue;
            }
            let r = right_channel(seg, &l, self.rate);
            let extra = extra_channels(seg, &l, ch, self.rate);
            for i in 0..seg.frames {
                let (mut a, mut b) = (l[i] * seg.gain + seg.dc, r[i] * seg.gain + seg.dc);
                match seg.stereo {
                    Stereo::LeftOnly => b = 0.0,
                    Stereo::RightOnly => a = 0.0,
                    _ => {}
                }
                out.push(quant(a));
                out.push(quant(b));
                for e in &extra {
                    out.push(quant(e[i] * seg.gain + seg.dc));
                }
            }
        }
        out
    }
}

/// Channels 2.. of a segment with more than two channels. Each is drawn
/// from its own RNG stream (seeded by the segment seed and the channel
/// index), so mono and stereo cases render exactly as before. The mix
/// covers what coupling is sensitive to: copies and scaled copies of the
/// left channel (coupled pairs with zero or tiny side signal), silence
/// (one zero and one nonzero channel in a coupling step, e.g. an LFE-only
/// or center-only 5.1 stream), independent noise and other generators.
fn extra_channels(seg: &Segment, l: &[f64], ch: usize, rate: u32) -> Vec<Vec<f64>> {
    (2..ch)
        .map(|c| {
            let mut rng = Rng::new(seg.seed ^ (c as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
            match rng.below(8) {
                0 => l.to_vec(),
                1 => l.iter().map(|x| -x).collect(),
                2 => {
                    let k = rng.range(-1.0, 1.0);
                    l.iter().map(|x| x * k).collect()
                }
                3 => vec![0.0; l.len()],
                4 | 5 => {
                    let mut r2 = Rng::new(rng.next_u64());
                    render_gen(&seg.generator, seg.frames, rate, &mut r2)
                }
                _ => {
                    let g = random_gen(&mut rng, rate);
                    let mut r2 = Rng::new(rng.next_u64());
                    render_gen(&g, seg.frames, rate, &mut r2)
                }
            }
        })
        .collect()
}

impl Case {
    /// Render to interleaved f32 (see [`F32Shape`]).
    pub fn render_f32(&self) -> Vec<f32> {
        let ch = self.channels as usize;
        let mut out = Vec::with_capacity(self.frames() * ch);
        for seg in &self.segments {
            let mut rng = Rng::new(seg.seed);
            let l = render_gen(&seg.generator, seg.frames, self.rate, &mut rng);
            if ch == 1 {
                out.extend(l.iter().map(|&x| self.f32_quant(x * seg.gain + seg.dc)));
                continue;
            }
            let r = right_channel(seg, &l, self.rate);
            let extra = extra_channels(seg, &l, ch, self.rate);
            for i in 0..seg.frames {
                let (mut a, mut b) = (l[i] * seg.gain + seg.dc, r[i] * seg.gain + seg.dc);
                match seg.stereo {
                    Stereo::LeftOnly => b = 0.0,
                    Stereo::RightOnly => a = 0.0,
                    _ => {}
                }
                out.push(self.f32_quant(a));
                out.push(self.f32_quant(b));
                for e in &extra {
                    out.push(self.f32_quant(e[i] * seg.gain + seg.dc));
                }
            }
        }
        if !out.is_empty() {
            let mut rng = Rng::new(self.f32.seed);
            for _ in 0..self.f32.specials {
                let p = rng.below(out.len());
                out[p] = *rng.pick(F32_SPECIALS);
            }
        }
        out
    }

    fn f32_quant(&self, x: f64) -> f32 {
        (x * self.f32.scale).clamp(-F32_LIMIT, F32_LIMIT) as f32
    }
}

fn right_channel(seg: &Segment, l: &[f64], rate: u32) -> Vec<f64> {
    match &seg.stereo {
        Stereo::Identical | Stereo::LeftOnly | Stereo::RightOnly => l.to_vec(),
        Stereo::Inverted => l.iter().map(|x| -x).collect(),
        Stereo::Scaled(k) => l.iter().map(|x| x * k).collect(),
        Stereo::Uncorrelated => {
            let mut r2 = Rng::new(seg.seed ^ 0xA5A5_A5A5_5A5A_5A5A);
            render_gen(&seg.generator, seg.frames, rate, &mut r2)
        }
        Stereo::Different(g) => {
            let mut r2 = Rng::new(seg.seed ^ 0x1234_5678_9ABC_DEF0);
            render_gen(g, seg.frames, rate, &mut r2)
        }
    }
}

fn quant(x: f64) -> i16 {
    (x * 32768.0).round().clamp(-32768.0, 32767.0) as i16
}

fn render_gen(g: &Gen, n: usize, rate: u32, rng: &mut Rng) -> Vec<f64> {
    let fs = rate as f64;
    match g {
        Gen::Silence => vec![0.0; n],
        Gen::White { amp, gaussian } => (0..n)
            .map(|_| {
                if *gaussian {
                    amp * rng.gauss() * 0.33
                } else {
                    amp * rng.range(-1.0, 1.0)
                }
            })
            .collect(),
        Gen::Pink { amp } => {
            // Paul Kellet's economy pink filter.
            let (mut b0, mut b1, mut b2) = (0.0, 0.0, 0.0);
            (0..n)
                .map(|_| {
                    let w = rng.range(-1.0, 1.0);
                    b0 = 0.99765 * b0 + w * 0.0990460;
                    b1 = 0.96300 * b1 + w * 0.2965164;
                    b2 = 0.57000 * b2 + w * 1.0526913;
                    amp * (b0 + b1 + b2 + w * 0.1848) * 0.25
                })
                .collect()
        }
        Gen::Brown { amp } => {
            let mut y = 0.0;
            (0..n)
                .map(|_| {
                    y = 0.998 * y + rng.range(-1.0, 1.0) * 0.06;
                    amp * y.clamp(-1.0, 1.0)
                })
                .collect()
        }
        Gen::Sines { partials } => (0..n)
            .map(|i| {
                let t = i as f64 / fs;
                partials
                    .iter()
                    .map(|(f, a, p)| a * det::sin(2.0 * PI * f * t + p))
                    .sum()
            })
            .collect(),
        Gen::Chirp { f0, f1, amp, log } => {
            let dur = (n.max(1)) as f64 / fs;
            let mut phase = 0.0;
            (0..n)
                .map(|i| {
                    let u = i as f64 / n.max(1) as f64;
                    let f = if *log {
                        f0 * det::powf(f1 / f0, u)
                    } else {
                        f0 + (f1 - f0) * u
                    };
                    let _ = dur;
                    phase += 2.0 * PI * f / fs;
                    amp * det::sin(phase)
                })
                .collect()
        }
        Gen::Square { freq, amp } => (0..n)
            .map(|i| {
                let ph = (i as f64 * freq / fs).fract();
                if ph < 0.5 { *amp } else { -amp }
            })
            .collect(),
        Gen::Saw { freq, amp } => (0..n)
            .map(|i| amp * (2.0 * (i as f64 * freq / fs).fract() - 1.0))
            .collect(),
        Gen::ImpulseTrain {
            period,
            amp,
            width,
            alternate,
        } => {
            let period = (*period).max(1);
            (0..n)
                .map(|i| {
                    let k = i / period;
                    if i % period < *width {
                        if *alternate && k % 2 == 1 { -amp } else { *amp }
                    } else {
                        0.0
                    }
                })
                .collect()
        }
        Gen::SparseClicks { count, amp } => {
            let mut v = vec![0.0; n];
            if n > 0 {
                for _ in 0..*count {
                    let p = rng.below(n);
                    v[p] = if rng.chance(0.5) { *amp } else { -amp };
                }
            }
            v
        }
        Gen::Dither { lsb } => {
            let l = *lsb as i64;
            (0..n)
                .map(|_| {
                    let k = rng.below((2 * l + 1) as usize) as i64 - l;
                    k as f64 / 32768.0
                })
                .collect()
        }
        Gen::LevelSteps {
            inner,
            step,
            levels,
        } => {
            let base = render_gen(inner, n, rate, rng);
            let step = (*step).max(1);
            base.iter()
                .enumerate()
                .map(|(i, x)| x * levels[(i / step) % levels.len()])
                .collect()
        }
        Gen::ToneWithBursts {
            freq,
            amp,
            burst_every,
            burst_len,
            burst_amp,
        } => {
            let every = (*burst_every).max(1);
            (0..n)
                .map(|i| {
                    let t = i as f64 / fs;
                    let mut x = amp * det::sin(2.0 * PI * freq * t);
                    if i % every < *burst_len {
                        x += burst_amp * rng.range(-1.0, 1.0);
                    }
                    x
                })
                .collect()
        }
    }
}

/// Lengths (in frames) that sit on or around block / chunk / LPC boundaries.
pub const EDGE_LENGTHS: &[usize] = &[
    0, 1, 2, 3, 31, 32, 33, 63, 64, 65, 127, 128, 129, 255, 256, 257, 383, 384, 385, 511, 512, 513,
    767, 768, 769, 1023, 1024, 1025, 1535, 1536, 1537, 2047, 2048, 2049, 2111, 2112, 2113, 2175,
    2176, 2177, 3071, 3072, 3073, 4095, 4096, 4097, 6143, 6144, 6145, 8191, 8192, 8193,
];

fn random_gen(rng: &mut Rng, rate: u32) -> Gen {
    let nyq = rate as f64 / 2.0;
    match rng.below(16) {
        0 => Gen::Silence,
        1 => Gen::White {
            amp: rng.amp_db(-90.0, 0.0),
            gaussian: rng.chance(0.5),
        },
        2 => Gen::Pink {
            amp: rng.amp_db(-80.0, 3.0),
        },
        3 => Gen::Brown {
            amp: rng.amp_db(-80.0, 0.0),
        },
        4 | 5 => {
            let k = 1 + rng.below(6);
            let partials = (0..k)
                .map(|_| {
                    let f = match rng.below(5) {
                        0 => rng.range(0.1, 20.0),            // sub-audio
                        1 => nyq - rng.range(0.0, 300.0),     // near-Nyquist
                        2 => rng.range(20.0, 200.0),          // bass
                        3 => rng.amp_db(26.0, 86.0).min(nyq), // log-uniform 20..20k
                        _ => nyq * rng.range(0.9, 1.0),       // top band
                    };
                    (
                        f,
                        rng.amp_db(-70.0, -3.0) / k as f64,
                        rng.range(0.0, 2.0 * PI),
                    )
                })
                .collect();
            Gen::Sines { partials }
        }
        6 => Gen::Chirp {
            f0: rng.range(5.0, 2000.0),
            f1: rng.range(20.0, nyq),
            amp: rng.amp_db(-40.0, 0.0),
            log: rng.chance(0.5),
        },
        7 => Gen::Square {
            freq: rng.amp_db(20.0, 80.0),
            amp: rng.amp_db(-40.0, 0.0),
        },
        8 => Gen::Saw {
            freq: rng.amp_db(20.0, 80.0),
            amp: rng.amp_db(-40.0, 0.0),
        },
        9 => Gen::ImpulseTrain {
            period: *rng.pick(&[64, 128, 256, 300, 512, 777, 1024, 2048, 4410, 11025]),
            amp: rng.amp_db(-30.0, 0.0),
            width: 1 + rng.below(4),
            alternate: rng.chance(0.5),
        },
        10 => Gen::SparseClicks {
            count: 1 + rng.below(8),
            amp: rng.amp_db(-40.0, 0.0),
        },
        11 => Gen::Dither {
            lsb: 1 + (rng.chance(0.2) as u8),
        },
        12 => Gen::LevelSteps {
            inner: Box::new(if rng.chance(0.5) {
                Gen::White {
                    amp: 0.5,
                    gaussian: false,
                }
            } else {
                Gen::Sines {
                    partials: vec![(rng.range(50.0, 5000.0), 0.7, 0.0)],
                }
            }),
            step: *rng.pick(&[256, 1000, 2048, 4410, 8000]),
            levels: vec![1.0, rng.amp_db(-80.0, -10.0), 0.0, rng.amp_db(-20.0, 0.0)],
        },
        13 => Gen::ToneWithBursts {
            freq: rng.range(50.0, 4000.0),
            amp: rng.amp_db(-30.0, -3.0),
            burst_every: *rng.pick(&[2048, 4096, 5000, 11025]),
            burst_len: 1 + rng.below(200),
            burst_amp: rng.amp_db(-20.0, 0.0),
        },
        14 => Gen::Square {
            // Full-scale (clipping) square hitting both rails.
            freq: rng.amp_db(30.0, 70.0),
            amp: 1.01,
        },
        _ => Gen::White {
            amp: rng.amp_db(-120.0, -80.0),
            gaussian: true,
        },
    }
}

fn random_stereo(rng: &mut Rng, rate: u32) -> Stereo {
    match rng.below(8) {
        0 | 1 => Stereo::Identical,
        2 => Stereo::Inverted,
        3 | 4 => Stereo::Uncorrelated,
        5 => {
            if rng.chance(0.5) {
                Stereo::LeftOnly
            } else {
                Stereo::RightOnly
            }
        }
        6 => Stereo::Scaled(rng.range(-1.0, 1.0)),
        _ => Stereo::Different(random_gen(rng, rate)),
    }
}

fn random_segment(rng: &mut Rng, rate: u32, frames: usize) -> Segment {
    let generator = random_gen(rng, rate);
    let gain = if rng.chance(0.1) {
        rng.range(1.0, 8.0) // drive into clipping
    } else {
        1.0
    };
    let dc = if rng.chance(0.12) {
        *rng.pick(&[1.0 / 32768.0, -1.0 / 32768.0, 0.01, -0.25, 0.5, 0.999])
    } else {
        0.0
    };
    Segment {
        generator,
        frames,
        gain,
        dc,
        stereo: random_stereo(rng, rate),
        seed: rng.next_u64(),
    }
}

fn random_length(rng: &mut Rng, rate: u32, max_secs: f64) -> usize {
    let max = (rate as f64 * max_secs) as usize;
    let n = match rng.below(10) {
        0..=2 => rng.below((rate as usize / 2).max(1)), // < 0.5 s
        3..=6 => rng.below(max.max(1)),
        7 => *rng.pick(EDGE_LENGTHS),
        // Near a multiple of the short/long block hop.
        8 => (1 + rng.below(64)) * 256 + rng.below(5) - 2,
        _ => (1 + rng.below(32)) * 1024 + rng.below(3) - 1,
    };
    n.min(max)
}

/// Build the case for one fuzz iteration. The f32 shape comes from a
/// separate RNG stream, so it doesn't perturb the s16 case for a seed.
pub fn make_case(case_seed: u64, rate: u32, channels: u16, max_secs: f64) -> Case {
    let mut case = make_case_s16(case_seed, rate, channels, max_secs);
    let mut rng = Rng::new(case_seed ^ 0x0F32_0F32_0F32_0F32);
    case.f32 = F32Shape {
        scale: match rng.below(8) {
            0..=3 => 1.0,
            4 => rng.range(1.0, 2.0),
            5 => rng.amp_db(0.0, 40.0),  // up to 100x
            6 => rng.amp_db(40.0, 80.0), // up to 1e4x, clamped to +-1e4
            _ => rng.amp_db(-60.0, 0.0), // quiet
        },
        specials: if rng.chance(0.3) {
            1 + rng.below(64)
        } else {
            0
        },
        seed: rng.next_u64(),
    };
    case
}

fn make_case_s16(case_seed: u64, rate: u32, channels: u16, max_secs: f64) -> Case {
    let mut rng = Rng::new(case_seed);
    match rng.below(10) {
        // Exact edge length, single segment.
        0 | 1 => {
            let n = *rng.pick(EDGE_LENGTHS);
            let seg = random_segment(&mut rng, rate, n);
            Case {
                rate,
                channels,
                segments: vec![seg],
                kind: "edge-length",
                f32: F32Shape::default(),
            }
        }
        // Single segment, random length.
        2..=4 => {
            let n = random_length(&mut rng, rate, max_secs);
            let seg = random_segment(&mut rng, rate, n);
            Case {
                rate,
                channels,
                segments: vec![seg],
                kind: "single",
                f32: F32Shape::default(),
            }
        }
        // Random concatenation of segments (abrupt level / content changes).
        _ => {
            let k = 2 + rng.below(6);
            let total_max = (rate as f64 * max_secs) as usize;
            let mut segs = Vec::new();
            let mut total = 0usize;
            for _ in 0..k {
                let n = match rng.below(4) {
                    0 => rng.below(2048) + 1,
                    1 => *rng.pick(EDGE_LENGTHS),
                    _ => rng.below((rate as usize).max(1)) + 1,
                };
                let n = n.min(total_max.saturating_sub(total));
                if n == 0 {
                    break;
                }
                total += n;
                segs.push(random_segment(&mut rng, rate, n));
            }
            Case {
                rate,
                channels,
                segments: segs,
                kind: "concat",
                f32: F32Shape::default(),
            }
        }
    }
}
