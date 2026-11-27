//! Helpers shared by the oracle-backed integration tests (parity.rs,
//! corpus_sweep.rs).

#![allow(dead_code)]

use oxivorbis::{Channels, SampleRate};

/// Default libvorbis base_quality for the sweeps (q5).
pub const OXIVORBIS_NATIVE_QUALITY: f32 = 0.5;

/// Quality-aware oxivorbis entry point used by the parity tests.
pub fn oxivorbis_encode_q(
    samples: &[i16],
    rate: SampleRate,
    channels: Channels,
    base_quality: f32,
    serial: u32,
    vendor: Option<&[u8]>,
    encoder_tag: Option<&[u8]>,
) -> Vec<u8> {
    oxivorbis::encode_with_base_quality_and_meta(
        samples,
        rate,
        channels,
        base_quality,
        serial,
        vendor,
        encoder_tag,
    )
}

/// Run the in-tree oracle on raw interleaved little-endian PCM. `format` is
/// the oracle's 4th argument: `"s16"` (i16, scaled by 1/32768) or `"f32"`.
pub fn oracle_encode_raw(
    raw: &[u8],
    rate: u32,
    channels: u16,
    base_quality: f32,
    format: &str,
) -> Vec<u8> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let bin = std::env::current_dir()
        .unwrap()
        .join("tools/oracle-encoder/oracle-encoder");
    let mut child = Command::new(&bin)
        .arg(rate.to_string())
        .arg(channels.to_string())
        .arg(base_quality.to_string())
        .arg(format)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn oracle-encoder at {}: {e}", bin.display()));
    let mut stdin = child.stdin.take().unwrap();
    let raw = raw.to_vec();
    let feeder = std::thread::spawn(move || stdin.write_all(&raw).unwrap());
    let out = child.wait_with_output().unwrap();
    feeder.join().unwrap();
    assert!(out.status.success(), "oracle-encoder exited non-zero");
    out.stdout
}

/// Ogg stream serial of the first page.
pub fn extract_serial(ogg_bytes: &[u8]) -> u32 {
    u32::from_le_bytes(ogg_bytes[14..18].try_into().unwrap())
}

/// (vendor, first user comment) from the comment header (second page).
pub fn extract_comment_strings(ogg_bytes: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let page0_len = {
        let segs = ogg_bytes[26] as usize;
        let body: usize = ogg_bytes[27..27 + segs].iter().map(|&s| s as usize).sum();
        27 + segs + body
    };
    let page1 = &ogg_bytes[page0_len..];
    let segs = page1[26] as usize;
    let first_pkt_len = page1[27] as usize;
    let pkt = &page1[27 + segs..27 + segs + first_pkt_len];
    let mut off = 7usize;
    let vlen = u32::from_le_bytes(pkt[off..off + 4].try_into().unwrap()) as usize;
    off += 4;
    let vendor = pkt[off..off + vlen].to_vec();
    off += vlen;
    let count = u32::from_le_bytes(pkt[off..off + 4].try_into().unwrap()) as usize;
    off += 4;
    let tag = if count > 0 {
        let clen = u32::from_le_bytes(pkt[off..off + 4].try_into().unwrap()) as usize;
        off += 4;
        pkt[off..off + clen].to_vec()
    } else {
        Vec::new()
    };
    (vendor, tag)
}

pub fn first_diff(a: &[u8], b: &[u8]) -> usize {
    a.iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .unwrap_or(a.len().min(b.len()))
}

/// SplitMix64: reproducible signals and chunkings.
pub struct Rng(pub u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, n); `n > 0`.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
    /// Uniform in [-1, 1).
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 23) as f32 - 1.0
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Sig {
    Silence,
    Sine,
    /// Quiet tone bed with sharp noise bursts every ~0.1-0.3 s.
    Transients,
    Noise,
}

pub const ALL_SIGS: [Sig; 4] = [Sig::Silence, Sig::Sine, Sig::Transients, Sig::Noise];

/// Test signal as nominally ±1.0 floats, interleaved.
pub fn signal_f32(sig: Sig, frames: usize, ch: usize, rate: u32, seed: u64) -> Vec<f32> {
    let mut rng = Rng(seed);
    let tau = std::f32::consts::TAU;
    let sr = rate as f32;
    let mut out = Vec::with_capacity(frames * ch);
    let mut next_burst = (sr * 0.05) as usize;
    let mut since = usize::MAX;
    for i in 0..frames {
        let t = i as f32 / sr;
        if i == next_burst {
            since = 0;
            next_burst += (sr * 0.1) as usize + rng.below((sr * 0.2) as usize);
        }
        let env = if since == usize::MAX {
            0.0
        } else {
            (-(since as f32) / (sr * 0.02)).exp()
        };
        since = since.saturating_add(1);
        for c in 0..ch {
            let v = match sig {
                Sig::Silence => 0.0,
                Sig::Sine => (tau * (440.0 + 220.0 * c as f32) * t).sin() * 0.5,
                Sig::Transients => {
                    (tau * (220.0 + 110.0 * c as f32) * t).sin() * 0.03 + rng.unit() * 0.85 * env
                }
                Sig::Noise => rng.unit() * 0.4,
            };
            out.push(v);
        }
    }
    out
}

/// [`signal_f32`] quantized to i16.
pub fn signal_i16(sig: Sig, frames: usize, ch: usize, rate: u32, seed: u64) -> Vec<i16> {
    signal_f32(sig, frames, ch, rate, seed)
        .into_iter()
        .map(|v| (v * 32767.0).clamp(-32768.0, 32767.0) as i16)
        .collect()
}

pub fn rate_hz(rate: SampleRate) -> u32 {
    rate.hz()
}

pub fn ch_count(channels: Channels) -> usize {
    channels.count() as usize
}
