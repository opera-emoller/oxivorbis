//! Golden oracle vectors: oxivorbis output vs committed libvorbis output.
//!
//! Each case renders a short signal with integer-only arithmetic (no libm,
//! so the input is bit-identical on every target) and compares oxivorbis's
//! encode with `tests/vectors/golden/<name>.ogg`, which was produced by the
//! in-tree oracle-encoder (pristine libvorbis 1.3.7).
//!
//! Unlike tests/parity.rs this needs no oracle at run time, so it also runs
//! where the oracle can't: on wasm32-wasip1 under wasmtime (`just
//! wasm-test`, CI's wasm job), whose libm and codegen differ from the native
//! hosts. With `--features oracle` the vectors are additionally checked
//! against a fresh oracle run (and rewritten with `OXIVORBIS_BLESS=1`).

mod common;

use oxivorbis::{BitrateMode, BitrateTuning, Channels, SampleRate};

/// xorshift32.
struct Xs(u32);

impl Xs {
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
    /// Uniform in [-2^15, 2^15).
    fn sample(&mut self) -> i32 {
        (self.next() >> 16) as i32 - 32768
    }
}

/// Integer triangle wave in [-2^15, 2^15) for a phase accumulator.
fn tri(phase: u32) -> i32 {
    let p = (phase >> 15) as i32; // 0..2^17
    if p < 1 << 16 {
        p - (1 << 15)
    } else {
        (3 << 15) - p
    }
}

#[derive(Clone, Copy)]
enum Gen {
    /// Two triangles + noise with a stepped level envelope.
    ToneNoise,
    /// Quiet triangle bed with decaying noise bursts every ~0.1..0.35 s.
    Transients,
}

/// Interleaved integer signal (nominal full scale = 2^15).
fn render(g: Gen, frames: usize, rate: u32, ch: usize, seed: u32) -> Vec<i32> {
    let mut rng = Xs(seed);
    let mut out = Vec::with_capacity(frames * ch);
    let step = |hz: u64| ((hz << 32) / rate as u64) as u32;
    let mut ph = [0u32; 4];
    let mut env: i64 = 0; // Q16
    let mut next_burst = rate as usize / 10;
    for i in 0..frames {
        if i == next_burst {
            env = 1 << 16;
            next_burst += rate as usize / 10 + (rng.next() as usize % (rate as usize / 4));
        }
        env = (env * 65_000) >> 16; // ~ -0.04 dB per sample
        let level = [1i32, 3, 8, 2][(i * 4 / rate as usize) % 4]; // /8
        for c in 0..ch {
            let v = match g {
                Gen::ToneNoise => {
                    ph[c] = ph[c].wrapping_add(step(330 + 110 * c as u64));
                    ph[c + 2] = ph[c + 2].wrapping_add(step(2900 + 400 * c as u64));
                    (tri(ph[c]) / 3 + tri(ph[c + 2]) / 6 + rng.sample() / 5) * level / 8
                }
                Gen::Transients => {
                    ph[c] = ph[c].wrapping_add(step(220 + 110 * c as u64));
                    tri(ph[c]) / 40 + ((rng.sample() as i64 * env) >> 16) as i32 * 7 / 8
                }
            };
            out.push(v);
        }
    }
    out
}

struct Case {
    name: &'static str,
    signal: Gen,
    frames: usize,
    rate: SampleRate,
    channels: Channels,
    quality: f32,
    /// Feed f32 (value / 8192, so up to ~±4) instead of i16.
    f32_input: bool,
    /// A bitrate mode instead of VBR (i16 input only).
    bitrate: Option<BitrateMode>,
    golden: &'static [u8],
}

macro_rules! golden {
    ($name:literal) => {
        include_bytes!(concat!("vectors/golden/", $name, ".ogg"))
    };
}

const CASES: &[Case] = &[
    Case {
        name: "tone_noise_mono44_qm1",
        signal: Gen::ToneNoise,
        frames: 44100 * 2,
        rate: SampleRate::HZ_44100,
        channels: Channels::Mono,
        quality: -0.1,
        f32_input: false,
        bitrate: None,
        golden: golden!("tone_noise_mono44_qm1"),
    },
    Case {
        name: "transients_stereo44_q5",
        signal: Gen::Transients,
        frames: 44100 * 2,
        rate: SampleRate::HZ_44100,
        channels: Channels::Stereo,
        quality: 0.5,
        f32_input: false,
        bitrate: None,
        golden: golden!("transients_stereo44_q5"),
    },
    Case {
        name: "tone_noise_stereo48_q10",
        signal: Gen::ToneNoise,
        frames: 48000 * 3 / 2,
        rate: SampleRate::HZ_48000,
        channels: Channels::Stereo,
        quality: 1.0,
        f32_input: false,
        bitrate: None,
        golden: golden!("tone_noise_stereo48_q10"),
    },
    Case {
        name: "transients_mono48_q2",
        signal: Gen::Transients,
        frames: 48000 * 2,
        rate: SampleRate::HZ_48000,
        channels: Channels::Mono,
        quality: 0.2,
        f32_input: false,
        bitrate: None,
        golden: golden!("transients_mono48_q2"),
    },
    Case {
        name: "short_stereo44_qm1",
        signal: Gen::ToneNoise,
        frames: 3000,
        rate: SampleRate::HZ_44100,
        channels: Channels::Stereo,
        quality: -0.1,
        f32_input: false,
        bitrate: None,
        golden: golden!("short_stereo44_qm1"),
    },
    Case {
        name: "f32_tone_noise_stereo48_q5",
        signal: Gen::ToneNoise,
        frames: 48000 * 2,
        rate: SampleRate::HZ_48000,
        channels: Channels::Stereo,
        quality: 0.5,
        f32_input: true,
        bitrate: None,
        golden: golden!("f32_tone_noise_stereo48_q5"),
    },
    // Other libvorbis template families (8, 11, 16, 22, 32 kHz, X, XX).
    Case {
        name: "transients_mono8_q5",
        signal: Gen::Transients,
        frames: 8000 * 2,
        rate: SampleRate::HZ_8000,
        channels: Channels::Mono,
        quality: 0.5,
        f32_input: false,
        bitrate: None,
        golden: golden!("transients_mono8_q5"),
    },
    Case {
        name: "f32_tone_noise_stereo11_q0",
        signal: Gen::ToneNoise,
        frames: 11025 * 2,
        rate: SampleRate::HZ_11025,
        channels: Channels::Stereo,
        quality: 0.0,
        f32_input: true,
        bitrate: None,
        golden: golden!("f32_tone_noise_stereo11_q0"),
    },
    Case {
        name: "tone_noise_stereo16_qm1",
        signal: Gen::ToneNoise,
        frames: 16000 * 2,
        rate: SampleRate::HZ_16000,
        channels: Channels::Stereo,
        quality: -0.1,
        f32_input: false,
        bitrate: None,
        golden: golden!("tone_noise_stereo16_qm1"),
    },
    Case {
        name: "transients_stereo22_q10",
        signal: Gen::Transients,
        frames: 22050 * 2,
        rate: SampleRate::HZ_22050,
        channels: Channels::Stereo,
        quality: 1.0,
        f32_input: false,
        bitrate: None,
        golden: golden!("transients_stereo22_q10"),
    },
    Case {
        name: "transients_mono32_qm1",
        signal: Gen::Transients,
        frames: 32000 * 2,
        rate: SampleRate::HZ_32000,
        channels: Channels::Mono,
        quality: -0.1,
        f32_input: false,
        bitrate: None,
        golden: golden!("transients_mono32_qm1"),
    },
    Case {
        name: "tone_noise_stereo96_q5",
        signal: Gen::ToneNoise,
        frames: 96000,
        rate: SampleRate::HZ_96000,
        channels: Channels::Stereo,
        quality: 0.5,
        f32_input: false,
        bitrate: None,
        golden: golden!("tone_noise_stereo96_q5"),
    },
    Case {
        name: "short_mono4_q3",
        signal: Gen::ToneNoise,
        frames: 3000,
        rate: SampleRate::new(4000).unwrap(),
        channels: Channels::Mono,
        quality: 0.3,
        f32_input: false,
        bitrate: None,
        golden: golden!("short_mono4_q3"),
    },
    // Bitrate management: every packetblob, the reservoir's choice, padding
    // and truncation.
    Case {
        name: "br_cbr128_tone_noise_stereo44",
        signal: Gen::ToneNoise,
        frames: 44100 * 3,
        rate: SampleRate::HZ_44100,
        channels: Channels::Stereo,
        quality: 0.5,
        f32_input: false,
        bitrate: Some(BitrateMode::Average {
            kbps: 128,
            min_kbps: Some(128),
            max_kbps: Some(128),
        }),
        golden: golden!("br_cbr128_tone_noise_stereo44"),
    },
    Case {
        name: "br_max32_transients_mono22",
        signal: Gen::Transients,
        frames: 22050 * 3,
        rate: SampleRate::HZ_22050,
        channels: Channels::Mono,
        quality: 0.5,
        f32_input: false,
        bitrate: Some(BitrateMode::Limits {
            min_kbps: None,
            max_kbps: Some(32),
        }),
        golden: golden!("br_max32_transients_mono22"),
    },
    Case {
        name: "br_q8_max160_tone_noise_stereo48",
        signal: Gen::ToneNoise,
        frames: 48000 * 3,
        rate: SampleRate::HZ_48000,
        channels: Channels::Stereo,
        quality: 0.8,
        f32_input: false,
        bitrate: Some(BitrateMode::QualityLimits {
            min_kbps: Some(64),
            max_kbps: Some(160),
        }),
        golden: golden!("br_q8_max160_tone_noise_stereo48"),
    },
];

/// The oracle-encoder's fixed stream serial and comment strings (it adds no
/// user comments).
const ORACLE_SERIAL: u32 = 0xC0FF_EE42;
const ORACLE_VENDOR: &[u8] = b"Xiph.Org libVorbis I 20200704 (Reducing Environment)";

enum Input {
    I16(Vec<i16>),
    F32(Vec<f32>),
}

fn input(c: &Case) -> Input {
    let ch = c.channels.count() as usize;
    let seed = 0x5EED_0000 ^ c.frames as u32;
    let x = render(c.signal, c.frames, c.rate.hz(), ch, seed);
    if c.f32_input {
        // Exact in f32 (|v| < 2^17), deliberately beyond ±1.0.
        Input::F32(x.iter().map(|&v| v as f32 / 8192.0).collect())
    } else {
        Input::I16(x.iter().map(|&v| v.clamp(-32768, 32767) as i16).collect())
    }
}

fn oxivorbis_encode(c: &Case) -> Vec<u8> {
    if let (Some(mode), Input::I16(s)) = (c.bitrate, input(c)) {
        return oxivorbis::encode_bitrate_with_meta(
            &s,
            c.rate,
            c.channels,
            c.quality,
            mode,
            BitrateTuning::default(),
            ORACLE_SERIAL,
            Some(ORACLE_VENDOR),
        )
        .expect("supported bitrate");
    }
    match input(c) {
        Input::I16(s) => oxivorbis::encode_with_base_quality_and_meta(
            &s,
            c.rate,
            c.channels,
            c.quality,
            ORACLE_SERIAL,
            Some(ORACLE_VENDOR),
            Some(b""),
        ),
        Input::F32(s) => oxivorbis::encode_f32_with_base_quality_and_meta(
            &s,
            c.rate,
            c.channels,
            c.quality,
            ORACLE_SERIAL,
            Some(ORACLE_VENDOR),
            Some(b""),
        ),
    }
}

#[test]
fn golden_vectors_match() {
    let mut failures = Vec::new();
    for c in CASES {
        let got = oxivorbis_encode(c);
        if got != c.golden {
            failures.push(format!(
                "{}: diverged at byte {} (oxivorbis {} bytes, golden {} bytes)",
                c.name,
                common::first_diff(&got, c.golden),
                got.len(),
                c.golden.len()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The committed vectors are what the current oracle produces.
#[cfg(feature = "oracle")]
#[test]
fn golden_vectors_are_fresh() {
    let bless = std::env::var_os("OXIVORBIS_BLESS").is_some();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/golden");
    let mut stale = Vec::new();
    for c in CASES {
        let ch = c.channels.count();
        let (raw, fmt): (Vec<u8>, &str) = match input(c) {
            Input::I16(s) => (s.iter().flat_map(|v| v.to_le_bytes()).collect(), "s16"),
            Input::F32(s) => (s.iter().flat_map(|v| v.to_le_bytes()).collect(), "f32"),
        };
        let flags = c
            .bitrate
            .map(|m| common::bitrate_oracle_args(m, &BitrateTuning::default()))
            .unwrap_or_default();
        let fresh = common::oracle_encode_raw_with(&raw, c.rate.hz(), ch, c.quality, fmt, &flags);
        assert_eq!(common::extract_serial(&fresh), ORACLE_SERIAL);
        assert_eq!(
            common::extract_comment_strings(&fresh),
            (ORACLE_VENDOR.to_vec(), Vec::new())
        );
        if fresh != c.golden {
            if bless {
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join(format!("{}.ogg", c.name)), &fresh).unwrap();
            }
            stale.push(c.name);
        }
    }
    assert!(
        stale.is_empty() || bless,
        "golden vectors differ from the oracle: {stale:?} (OXIVORBIS_BLESS=1 rewrites them)"
    );
}
