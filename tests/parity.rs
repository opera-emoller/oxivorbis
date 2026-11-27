//! Byte-parity tests: oxivorbis vs oggenc (libvorbis) -q 5.
//!
//! Gated behind the `oracle` feature so contributors without oggenc can still
//! run the rest of the suite. Run with:
//!   cargo nextest run --features oracle parity_
//!
//! Historically these tests used `ffmpeg -c:a libvorbis`, but Homebrew's
//! ffmpeg formula dropped --enable-libvorbis. oggenc from vorbis-tools is
//! the canonical libvorbis CLI and produces equivalent output.

#![cfg(feature = "oracle")]

mod common;

use oxivorbis::{Channels, SampleRate};
use std::io::Write;
use std::process::{Command, Stdio};

/// Encode using our own statically-linked libvorbis (tools/oracle-encoder).
/// Pinned compile flags (-O0 -ffp-contract=off) make this deterministic;
/// CI / dev hosts produce identical bytes given identical input.
fn oracle_encode_q5(samples: &[i16], rate: u32, channels: u16) -> Vec<u8> {
    oracle_encode(samples, rate, channels, None)
}

/// Like [`oracle_encode_q5`] but at an arbitrary libvorbis base_quality
/// (-0.1..1.0 == q-1..q10), passed as the oracle's 3rd argument.
fn oracle_encode_q(samples: &[i16], rate: u32, channels: u16, base_quality: f32) -> Vec<u8> {
    oracle_encode(samples, rate, channels, Some(base_quality))
}

fn oracle_encode(samples: &[i16], rate: u32, channels: u16, base_quality: Option<f32>) -> Vec<u8> {
    let raw: Vec<u8> = samples.iter().flat_map(|&s| s.to_le_bytes()).collect();
    let bin = std::env::current_dir()
        .unwrap()
        .join("tools/oracle-encoder/oracle-encoder");
    let mut cmd = Command::new(&bin);
    cmd.arg(rate.to_string()).arg(channels.to_string());
    if let Some(q) = base_quality {
        cmd.arg(q.to_string());
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn oracle-encoder at {}: {e}", bin.display()));
    child.stdin.take().unwrap().write_all(&raw).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "oracle-encoder exited non-zero");
    out.stdout
}

fn assert_parity_oracle(samples: &[i16], rate: SampleRate, channels: Channels) {
    let rate_hz: u32 = rate.hz();
    let ch: u16 = channels.count();
    let oracle_bytes = oracle_encode_q5(samples, rate_hz, ch);
    let serial = extract_serial(&oracle_bytes);
    let (vendor, encoder_tag) = extract_comment_strings(&oracle_bytes);
    let oxivorbis_bytes = oxivorbis::encode_with_serial_and_meta(
        samples,
        rate,
        channels,
        serial,
        Some(&vendor),
        Some(&encoder_tag),
    );
    if oxivorbis_bytes != oracle_bytes {
        let div = first_diff(&oxivorbis_bytes, &oracle_bytes);
        let lw_end = (div + 16).min(oxivorbis_bytes.len());
        let or_end = (div + 16).min(oracle_bytes.len());
        let start = div.saturating_sub(8);
        panic!(
            "ORACLE parity diverged at byte {div}\n  oxivorbis len: {}\n  oracle  len: {}\n  oxivorbis ctx: {:02x?}\n  oracle  ctx: {:02x?}",
            oxivorbis_bytes.len(),
            oracle_bytes.len(),
            &oxivorbis_bytes[start..lw_end],
            &oracle_bytes[start.min(oracle_bytes.len())..or_end],
        );
    }
}

fn oggenc_encode_q5(samples: &[i16], rate: u32, channels: u16) -> Vec<u8> {
    oggenc_encode(samples, rate, channels, "5")
}

/// System `oggenc -q <quality>` (quality passed verbatim, oggenc scale).
fn oggenc_encode(samples: &[i16], rate: u32, channels: u16, quality: &str) -> Vec<u8> {
    let raw: Vec<u8> = samples.iter().flat_map(|&s| s.to_le_bytes()).collect();

    let mut child = Command::new("oggenc")
        .args([
            "-Q",
            "-q",
            quality,
            "-R",
            &rate.to_string(),
            "-B",
            "16",
            "-C",
            &channels.to_string(),
            "--raw",
            "-",
            "-o",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn oggenc — is vorbis-tools installed?");

    child.stdin.take().unwrap().write_all(&raw).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "oggenc exited with non-zero status"
    );
    output.stdout
}

fn extract_serial(ogg_bytes: &[u8]) -> u32 {
    assert!(
        ogg_bytes.len() >= 18,
        "ogg output too short to contain serial"
    );
    u32::from_le_bytes(ogg_bytes[14..18].try_into().unwrap())
}

/// Extract the vendor string and first user comment (encoder tag) from a
/// Vorbis comment header packet embedded in the given OGG bitstream.
///
/// The comment header is the second packet in the stream (second OGG page,
/// first segment).  Returns (vendor, encoder_tag) as owned Vec<u8>.
fn extract_comment_strings(ogg_bytes: &[u8]) -> (Vec<u8>, Vec<u8>) {
    assert!(ogg_bytes.len() >= 58, "OGG output too short");
    let page0_len = {
        let page_segs = ogg_bytes[26] as usize;
        let seg_table = &ogg_bytes[27..27 + page_segs];
        let data_len: usize = seg_table.iter().map(|&s| s as usize).sum();
        27 + page_segs + data_len
    };
    let page1 = &ogg_bytes[page0_len..];
    assert!(page1.len() >= 27, "page1 too short");
    let page1_segs = page1[26] as usize;
    assert!(page1.len() >= 27 + page1_segs, "page1 seg table too short");
    let first_pkt_len = page1[27] as usize;
    let pkt = &page1[27 + page1_segs..27 + page1_segs + first_pkt_len];

    let mut off = 7usize;
    let vlen = u32::from_le_bytes(pkt[off..off + 4].try_into().unwrap()) as usize;
    off += 4;
    let vendor = pkt[off..off + vlen].to_vec();
    off += vlen;
    let count = u32::from_le_bytes(pkt[off..off + 4].try_into().unwrap()) as usize;
    off += 4;
    let encoder_tag = if count > 0 {
        let clen = u32::from_le_bytes(pkt[off..off + 4].try_into().unwrap()) as usize;
        off += 4;
        pkt[off..off + clen].to_vec()
    } else {
        Vec::new()
    };
    (vendor, encoder_tag)
}

fn first_diff(a: &[u8], b: &[u8]) -> usize {
    let common = a.len().min(b.len());
    for i in 0..common {
        if a[i] != b[i] {
            return i;
        }
    }
    common
}

fn assert_parity(samples: &[i16], rate: SampleRate, channels: Channels) {
    let rate_hz: u32 = rate.hz();
    let ch: u16 = channels.count();

    let oracle_bytes = oggenc_encode_q5(samples, rate_hz, ch);
    let serial = extract_serial(&oracle_bytes);
    let (vendor, encoder_tag) = extract_comment_strings(&oracle_bytes);
    let oxivorbis_bytes = oxivorbis::encode_with_serial_and_meta(
        samples,
        rate,
        channels,
        serial,
        Some(&vendor),
        Some(&encoder_tag),
    );

    if oxivorbis_bytes != oracle_bytes {
        let div = first_diff(&oxivorbis_bytes, &oracle_bytes);
        let lw_start = div.saturating_sub(8);
        let lw_end = (div + 16).min(oxivorbis_bytes.len());
        let or_end = (div + 16).min(oracle_bytes.len());
        let or_start = div.saturating_sub(8).min(oracle_bytes.len());
        panic!(
            "parity diverged at byte {div}\n  oxivorbis len: {}\n  oggenc  len: {}\n  oxivorbis ctx: {:02x?}\n  oggenc  ctx: {:02x?}",
            oxivorbis_bytes.len(),
            oracle_bytes.len(),
            &oxivorbis_bytes[lw_start..lw_end],
            &oracle_bytes[or_start..or_end],
        );
    }
}

/// `oxivorbis::oggenc_base_quality` must reproduce oggenc's `-q` float
/// conversion bit for bit. The listed values are ones where the candidate
/// conversions disagree on the f32 and that disagreement reaches the bytes
/// (the ID header's bitrate_nominal): 9.344/9.351 separate oggenc's
/// `(float)((double)(float)q * 0.1)` from both `(float)(atof(q) * 0.1)` and
/// `(float)q * 0.1f`; 9.303 separates it from the all-double form. "11"
/// covers oggenc's clamp to 10. Silence keeps the system libvorbis's FP
/// optimizations out of the picture.
#[test]
fn parity_oggenc_quality_conversion() {
    let samples = vec![0i16; 4410 * 2];
    for q in ["0", "2.5", "7.3", "9.303", "9.344", "9.351", "10", "11"] {
        let want = oggenc_encode(&samples, 44100, 2, q);
        let serial = extract_serial(&want);
        let (vendor, encoder_tag) = extract_comment_strings(&want);
        let qf: f32 = q.parse().unwrap();
        let got = oxivorbis::encode_with_base_quality_and_meta(
            &samples,
            SampleRate::Hz44100,
            Channels::Stereo,
            oxivorbis::oggenc_base_quality(qf),
            serial,
            Some(&vendor),
            Some(&encoder_tag),
        );
        assert!(
            got == want,
            "oggenc -q {q}: diverged at byte {}",
            first_diff(&got, &want)
        );
    }
    // The test values really do discriminate between the conversions.
    let naive = |q: f64| (q * 0.1) as f32;
    assert_ne!(oxivorbis::oggenc_base_quality(9.344), naive(9.344));
    assert_ne!(oxivorbis::oggenc_base_quality(9.303), naive(9.303));
    assert_ne!(oxivorbis::oggenc_base_quality(9.344), 9.344f32 * 0.1f32);
}

fn make_sine_mono(rate: u32, freq: f32, duration_secs: f32) -> Vec<i16> {
    let n = (rate as f32 * duration_secs) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / rate as f32;
            (f32::sin(2.0 * std::f32::consts::PI * freq * t) * 16384.0) as i16
        })
        .collect()
}

#[test]
fn parity_silence_mono44() {
    assert_parity(&vec![0i16; 44100], SampleRate::Hz44100, Channels::Mono);
}

#[test]
fn parity_silence_stereo44() {
    let stereo: Vec<i16> = vec![0i16; 44100 * 2];
    assert_parity(&stereo, SampleRate::Hz44100, Channels::Stereo);
}

#[test]
fn parity_silence_mono48() {
    assert_parity(&vec![0i16; 48000], SampleRate::Hz48000, Channels::Mono);
}

#[test]
fn parity_silence_stereo48() {
    let stereo: Vec<i16> = vec![0i16; 48000 * 2];
    assert_parity(&stereo, SampleRate::Hz48000, Channels::Stereo);
}

#[test]
#[ignore = "brew oggenc's libvorbis has FP optimization (FMA) that diverges from the in-tree -O0 oracle on tone-rich input. Use oracle_parity_sine_440_mono44 instead."]
fn parity_sine_440_mono44() {
    let samples = make_sine_mono(44100, 440.0, 1.0);
    assert_parity(&samples, SampleRate::Hz44100, Channels::Mono);
}

// --- Oracle-based parity (own libvorbis build, deterministic flags) ---

#[test]
fn oracle_parity_silence_mono44() {
    assert_parity_oracle(&vec![0i16; 44100], SampleRate::Hz44100, Channels::Mono);
}

#[test]
fn oracle_parity_silence_stereo44() {
    let stereo: Vec<i16> = vec![0i16; 44100 * 2];
    assert_parity_oracle(&stereo, SampleRate::Hz44100, Channels::Stereo);
}

#[test]
fn oracle_parity_sine_440_mono44() {
    let samples = make_sine_mono(44100, 440.0, 1.0);
    assert_parity_oracle(&samples, SampleRate::Hz44100, Channels::Mono);
}

#[test]
#[ignore = "oggenc/libvorbis bottle may differ from in-tree -O0 oracle (FMA, optimization). Use oracle_parity_ramp_stereo44 instead."]
fn parity_ramp_stereo44() {
    let n = 44100usize;
    let samples: Vec<i16> = (0..n * 2)
        .map(|i| ((i % 65536) as i32 - 32768) as i16)
        .collect();
    assert_parity(&samples, SampleRate::Hz44100, Channels::Stereo);
}

#[test]
fn oracle_parity_ramp_stereo44() {
    let n = 44100usize;
    let samples: Vec<i16> = (0..n * 2)
        .map(|i| ((i % 65536) as i32 - 32768) as i16)
        .collect();
    assert_parity_oracle(&samples, SampleRate::Hz44100, Channels::Stereo);
}

#[test]
#[ignore = "manual dump for parity-diff analysis — ramp stereo"]
fn parity_dump_ramp_files() {
    let n = 44100usize;
    let samples: Vec<i16> = (0..n * 2)
        .map(|i| ((i % 65536) as i32 - 32768) as i16)
        .collect();
    let oracle_bytes = oracle_encode_q5(&samples, 44100, 2);
    let serial = extract_serial(&oracle_bytes);
    let (vendor, encoder_tag) = extract_comment_strings(&oracle_bytes);
    let oxivorbis_bytes = oxivorbis::encode_with_serial_and_meta(
        &samples,
        SampleRate::Hz44100,
        Channels::Stereo,
        serial,
        Some(&vendor),
        Some(&encoder_tag),
    );
    std::fs::write("/tmp/oracle_ramp.ogg", &oracle_bytes).unwrap();
    std::fs::write("/tmp/lw_ramp.ogg", &oxivorbis_bytes).unwrap();
    eprintln!(
        "Wrote oracle_ramp.ogg ({} bytes) and lw_ramp.ogg ({} bytes)",
        oracle_bytes.len(),
        oxivorbis_bytes.len()
    );
}

#[test]
#[ignore = "manual dump for parity-diff analysis"]
fn parity_dump_files() {
    let samples = vec![0i16; 44100];
    let oracle_bytes = oggenc_encode_q5(&samples, 44100, 1);
    let serial = extract_serial(&oracle_bytes);
    let oxivorbis_bytes =
        oxivorbis::encode_with_serial(&samples, SampleRate::Hz44100, Channels::Mono, serial);
    std::fs::write("/tmp/oggenc_parity.ogg", &oracle_bytes).unwrap();
    std::fs::write("/tmp/lw_parity.ogg", &oxivorbis_bytes).unwrap();
    eprintln!(
        "Wrote oggenc_parity.ogg ({} bytes) and lw_parity.ogg ({} bytes)",
        oracle_bytes.len(),
        oxivorbis_bytes.len()
    );
}

#[test]
#[ignore = "manual dump for parity-diff analysis — sine wave"]
fn parity_dump_sine_files() {
    let samples = make_sine_mono(44100, 440.0, 1.0);
    let oracle_bytes = oggenc_encode_q5(&samples, 44100, 1);
    let serial = extract_serial(&oracle_bytes);
    let (vendor, encoder_tag) = extract_comment_strings(&oracle_bytes);
    let oxivorbis_bytes = oxivorbis::encode_with_serial_and_meta(
        &samples,
        SampleRate::Hz44100,
        Channels::Mono,
        serial,
        Some(&vendor),
        Some(&encoder_tag),
    );
    std::fs::write("/tmp/oggenc_sine.ogg", &oracle_bytes).unwrap();
    std::fs::write("/tmp/lw_sine.ogg", &oxivorbis_bytes).unwrap();
    eprintln!(
        "Wrote oggenc_sine.ogg ({} bytes) and lw_sine.ogg ({} bytes)",
        oracle_bytes.len(),
        oxivorbis_bytes.len()
    );
}

// Real-audio corpus parity is now in tests/corpus_sweep.rs — it walks
// <repo_root>/corpus/ recursively and runs every file through the same
// assert_parity_oracle path. That's gated by the `oracle` feature and
// `#[ignore]` (manual run) since it depends on an out-of-tree corpus.

// --- Quality-parameterized oracle parity ---

fn rate_hz(rate: SampleRate) -> u32 {
    rate.hz()
}

fn channel_count(channels: Channels) -> u16 {
    channels.count()
}

/// Oracle parity at an arbitrary libvorbis base_quality (-0.1..1.0).
///
/// The oracle gets `base_quality` as its 3rd argument; oxivorbis goes through
/// `common::oxivorbis_encode_q`.
fn assert_parity_oracle_q(
    samples: &[i16],
    rate: SampleRate,
    channels: Channels,
    base_quality: f32,
) {
    let oracle_bytes = oracle_encode_q(
        samples,
        rate_hz(rate),
        channel_count(channels),
        base_quality,
    );
    let serial = extract_serial(&oracle_bytes);
    let (vendor, encoder_tag) = extract_comment_strings(&oracle_bytes);
    let oxivorbis_bytes = common::oxivorbis_encode_q(
        samples,
        rate,
        channels,
        base_quality,
        serial,
        Some(&vendor),
        Some(&encoder_tag),
    );
    if oxivorbis_bytes != oracle_bytes {
        let div = first_diff(&oxivorbis_bytes, &oracle_bytes);
        let start = div.saturating_sub(8);
        let lw_end = (div + 16).min(oxivorbis_bytes.len());
        let or_end = (div + 16).min(oracle_bytes.len());
        panic!(
            "ORACLE parity (base_quality {base_quality}) diverged at byte {div}\n  oxivorbis len: {}\n  oracle  len: {}\n  oxivorbis ctx: {:02x?}\n  oracle  ctx: {:02x?}",
            oxivorbis_bytes.len(),
            oracle_bytes.len(),
            &oxivorbis_bytes[start.min(oxivorbis_bytes.len())..lw_end],
            &oracle_bytes[start.min(oracle_bytes.len())..or_end],
        );
    }
}

/// Deterministic xorshift32 so the noise signals are reproducible.
struct XorShift32(u32);

impl XorShift32 {
    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// Uniform in [-1, 1).
    fn next_f32(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / (1u32 << 23) as f32 - 1.0
    }
}

#[derive(Clone, Copy)]
enum Signal {
    /// 1 s digital silence.
    Silence,
    /// 1 s sine (440 Hz left/mono, 660 Hz right).
    Sine440,
    /// 1 s full-scale sawtooth ramp over interleaved samples.
    Ramp,
    /// 2 s seeded white noise mixed with two tones.
    NoiseTone,
    /// ~10 s quiet tone bed with sharp decaying noise bursts, so the
    /// envelope detector picks short blocks / impulse handling.
    Transients,
}

impl Signal {
    fn generate(self, rate: u32, channels: u16) -> Vec<i16> {
        let ch = channels as usize;
        let sr = rate as f32;
        let tau = 2.0 * std::f32::consts::PI;
        let frames = |secs: f32| (sr * secs) as usize;
        let clip = |v: f32| v.clamp(-32768.0, 32767.0) as i16;
        match self {
            Signal::Silence => vec![0; frames(1.0) * ch],
            Signal::Sine440 => (0..frames(1.0))
                .flat_map(|i| {
                    let t = i as f32 / sr;
                    (0..ch).map(move |c| {
                        let f = if c == 0 { 440.0 } else { 660.0 };
                        (f32::sin(tau * f * t) * 16384.0) as i16
                    })
                })
                .collect(),
            Signal::Ramp => (0..frames(1.0) * ch)
                .map(|i| ((i % 65536) as i32 - 32768) as i16)
                .collect(),
            Signal::NoiseTone => {
                let mut rng = XorShift32(0x1234_5678);
                let mut out = Vec::with_capacity(frames(2.0) * ch);
                for i in 0..frames(2.0) {
                    let t = i as f32 / sr;
                    for c in 0..ch {
                        let tone = f32::sin(tau * 330.0 * t) * 6000.0
                            + f32::sin(tau * (2500.0 + 500.0 * c as f32) * t) * 3000.0;
                        out.push(clip(tone + rng.next_f32() * 4000.0));
                    }
                }
                out
            }
            Signal::Transients => {
                let mut rng = XorShift32(0x9E37_79B9);
                let n = frames(10.0);
                // Burst onsets at irregular ~0.2..0.6 s spacing.
                let mut onsets = Vec::new();
                let mut pos = frames(0.25);
                while pos < n {
                    onsets.push(pos);
                    pos += frames(0.2) + (rng.next_u32() as usize % frames(0.4));
                }
                let decay = 1.0 / (sr * 0.03);
                let mut out = Vec::with_capacity(n * ch);
                let mut next = 0;
                let mut since = usize::MAX;
                for i in 0..n {
                    if next < onsets.len() && onsets[next] == i {
                        since = 0;
                        next += 1;
                    }
                    let t = i as f32 / sr;
                    let env = if since == usize::MAX {
                        0.0
                    } else {
                        f32::exp(-(since as f32) * decay)
                    };
                    since = since.saturating_add(1);
                    for c in 0..ch {
                        let bed = f32::sin(tau * (220.0 + 110.0 * c as f32) * t) * 800.0;
                        out.push(clip(bed + rng.next_f32() * 28000.0 * env));
                    }
                }
                out
            }
        }
    }
}

fn run_quality_case(base_quality: f32, rate: SampleRate, channels: Channels, signal: Signal) {
    let samples = signal.generate(rate_hz(rate), channel_count(channels));
    assert_parity_oracle_q(&samples, rate, channels, base_quality);
}

/// Test matrix: `oracle_parity_q::<quality>::<config>::<signal>`.
/// One representative base_quality per libvorbis template interval
/// (-0.1, 0.0, ..., 1.0 are the 12 anchor points; the midpoints exercise
/// interpolation between adjacent templates) plus the endpoints 0.0 and
/// 1.0 and q5 itself (0.5).
mod oracle_parity_q {
    use super::*;

    macro_rules! signal_tests {
        ($q:expr, $rate:expr, $ch:expr; $(#[$m:meta])*) => {
            #[test] $(#[$m])* fn silence() { run_quality_case($q, $rate, $ch, Signal::Silence) }
            #[test] $(#[$m])* fn sine440() { run_quality_case($q, $rate, $ch, Signal::Sine440) }
            #[test] $(#[$m])* fn ramp() { run_quality_case($q, $rate, $ch, Signal::Ramp) }
            #[test] $(#[$m])* fn noise_tone() { run_quality_case($q, $rate, $ch, Signal::NoiseTone) }
            #[test] $(#[$m])* fn transients() { run_quality_case($q, $rate, $ch, Signal::Transients) }
        };
    }

    macro_rules! quality {
        ($name:ident, $q:expr $(, #[$m:meta])*) => {
            mod $name {
                use super::*;
                mod mono44 { use super::*; signal_tests!($q, SampleRate::Hz44100, Channels::Mono; $(#[$m])*); }
                mod stereo44 { use super::*; signal_tests!($q, SampleRate::Hz44100, Channels::Stereo; $(#[$m])*); }
                mod mono48 { use super::*; signal_tests!($q, SampleRate::Hz48000, Channels::Mono; $(#[$m])*); }
                mod stereo48 { use super::*; signal_tests!($q, SampleRate::Hz48000, Channels::Stereo; $(#[$m])*); }
            }
        };
    }

    // q-1 (512/4096 blocks): the -0.1 anchor, interior points, and the
    // smallest-magnitude negative base_quality that still selects it.
    quality!(q_m010, -0.1);
    quality!(q_m008, -0.08);
    quality!(q_m005, -0.05);
    quality!(q_m001, -0.01);
    quality!(q_m0000001, -0.0000001);
    quality!(q_000, 0.0);
    quality!(q_005, 0.05);
    quality!(q_015, 0.15);
    quality!(q_025, 0.25);
    quality!(q_035, 0.35);
    quality!(q_045, 0.45);
    quality!(q_050, 0.5);
    quality!(q_055, 0.55);
    quality!(q_065, 0.65);
    quality!(q_075, 0.75);
    quality!(q_085, 0.85);
    quality!(q_095, 0.95);
    quality!(q_100, 1.0);
}

/// Reassemble Ogg packets from a stream (single logical stream).
fn ogg_packets(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut packets = Vec::new();
    let mut cur = Vec::new();
    let mut i = 0;
    while i + 27 <= bytes.len() {
        let n = bytes[i + 26] as usize;
        let lacing = &bytes[i + 27..i + 27 + n];
        let mut off = i + 27 + n;
        for &l in lacing {
            cur.extend_from_slice(&bytes[off..off + l as usize]);
            off += l as usize;
            if l < 255 {
                packets.push(std::mem::take(&mut cur));
            }
        }
        i = off;
    }
    packets
}

/// Sanity check on the matrix itself: the Transients signal must make
/// libvorbis pick short blocks (mode 0 in libvorbis's 2-mode setup; the
/// mode number is bit 1 of an audio packet's first byte).
#[test]
fn oracle_parity_transients_signal_uses_short_blocks() {
    for (rate, ch) in [(44100, 1), (48000, 2)] {
        let samples = Signal::Transients.generate(rate, ch);
        let out = oracle_encode_q(&samples, rate, ch, 0.5);
        let audio = &ogg_packets(&out)[3..];
        let short = audio
            .iter()
            .filter(|p| !p.is_empty() && p[0] & 0b10 == 0)
            .count();
        assert!(
            short >= 20,
            "{rate} Hz x{ch}: only {short}/{} short blocks",
            audio.len()
        );
    }
}

/// Regression: Q5 digital silence packs into 1-byte packets, so libogg
/// force-flushes pages at 255 segments (ogg_stream_flush_i `vals==255`).
/// oxivorbis's page accounting used to carry its byte/packet counters across
/// those forced flushes and cut a short page once ~4096 cumulative packets
/// (~95 s at 44.1 kHz) had gone by. 120 s of silence crosses that point.
#[test]
fn oracle_parity_long_silence_mono44() {
    assert_parity_oracle(
        &vec![0i16; 44100 * 120],
        SampleRate::Hz44100,
        Channels::Mono,
    );
}

/// Same condition on a stereo 48 kHz stream (~4096 packets after ~87 s).
#[test]
fn oracle_parity_long_silence_stereo48() {
    let stereo = vec![0i16; 48000 * 2 * 100];
    assert_parity_oracle(&stereo, SampleRate::Hz48000, Channels::Stereo);
}

// --- Long non-silent signals ---
//
// Everything above is at most 10 s of non-silent audio, so a bug that only
// shows up after many blocks (state carried across blocks, page/granule
// accounting, envelope history, bitrate tracking) could slip through. These
// run 30..120 s of busy, never-silent material end to end.

/// Long seeded signal generators (`secs` of audio, never silent).
#[derive(Clone, Copy)]
enum LongSignal {
    /// White noise + two tones, with a slow (0.1 Hz) level swell so the
    /// psychoacoustics see changing loudness.
    NoiseTone,
    /// Tone bed with noise bursts every ~0.15..0.5 s (short blocks
    /// throughout).
    Transients,
    /// 20 Hz .. 20 kHz logarithmic sweep, back down again, over a quiet
    /// noise floor; the right channel lags by a varying amount.
    Chirp,
}

fn long_signal(sig: LongSignal, secs: f32, rate: u32, channels: u16) -> Vec<i16> {
    let ch = channels as usize;
    let sr = rate as f64;
    let n = (sr * secs as f64) as usize;
    let tau = std::f64::consts::TAU;
    let clip = |v: f64| v.clamp(-32768.0, 32767.0) as i16;
    let mut rng = XorShift32(match sig {
        LongSignal::NoiseTone => 0x0BAD_5EED,
        LongSignal::Transients => 0x7A57_0001,
        LongSignal::Chirp => 0xC41B_0002,
    });
    let mut out = Vec::with_capacity(n * ch);
    match sig {
        LongSignal::NoiseTone => {
            for i in 0..n {
                let t = i as f64 / sr;
                let swell = 0.55 + 0.45 * (tau * 0.1 * t).sin();
                for c in 0..ch {
                    let tone = (tau * (330.0 + 7.0 * c as f64) * t).sin() * 7000.0
                        + (tau * (2500.0 + 500.0 * c as f64) * t).sin() * 2500.0;
                    let noise = rng.next_f32() as f64 * 5000.0;
                    out.push(clip((tone + noise) * swell + 60.0));
                }
            }
        }
        LongSignal::Transients => {
            let mut next_onset = (0.1 * sr) as usize;
            let mut since = usize::MAX;
            let decay = 1.0 / (sr * 0.025);
            for i in 0..n {
                if i == next_onset {
                    since = 0;
                    next_onset +=
                        (0.15 * sr) as usize + rng.next_u32() as usize % (0.35 * sr) as usize;
                }
                let env = if since == usize::MAX {
                    0.0
                } else {
                    (-(since as f64) * decay).exp()
                };
                since = since.saturating_add(1);
                let t = i as f64 / sr;
                for c in 0..ch {
                    let bed = (tau * (220.0 + 110.0 * c as f64) * t).sin() * 900.0;
                    out.push(clip(bed + rng.next_f32() as f64 * 30000.0 * env));
                }
            }
        }
        LongSignal::Chirp => {
            let (f0, f1) = (20.0f64, 20000.0f64.min(sr * 0.45));
            let half = n / 2;
            let mut phase = [0.0f64; 2];
            for i in 0..n {
                // Up for the first half, down for the second.
                let u = if i < half {
                    i as f64 / half as f64
                } else {
                    (n - i) as f64 / (n - half) as f64
                };
                for (c, ph) in phase.iter_mut().enumerate().take(ch) {
                    let lag = 0.002 * c as f64 * (1.0 + (i as f64 / sr * 0.05).sin());
                    let f = f0 * (f1 / f0).powf((u - lag).clamp(0.0, 1.0));
                    *ph = (*ph + tau * f / sr) % tau;
                    let floor = rng.next_f32() as f64 * 150.0;
                    out.push(clip(ph.sin() * 12000.0 + floor));
                }
            }
        }
    }
    out
}

fn run_long_case(
    sig: LongSignal,
    secs: f32,
    rate: SampleRate,
    channels: Channels,
    base_quality: f32,
) {
    let samples = long_signal(sig, secs, rate_hz(rate), channel_count(channels));
    // Never silent: every second must carry signal on every channel.
    let ch = channel_count(channels) as usize;
    let sr = rate_hz(rate) as usize;
    for (k, second) in samples.chunks(sr * ch).enumerate() {
        for c in 0..ch {
            let peak = second
                .iter()
                .skip(c)
                .step_by(ch)
                .map(|s| s.unsigned_abs())
                .max();
            assert!(
                peak.unwrap_or(0) > 100,
                "second {k} ch {c} is (near-)silent"
            );
        }
    }
    assert_parity_oracle_q(&samples, rate, channels, base_quality);
}

/// `oracle_parity_long::cases::<signal>_<secs>s_<config>_<quality>`.
mod oracle_parity_long {
    use super::*;

    macro_rules! long {
        ($name:ident, $sig:ident, $secs:expr, $rate:ident, $ch:ident, $q:expr) => {
            #[test]
            fn $name() {
                run_long_case(
                    LongSignal::$sig,
                    $secs,
                    SampleRate::$rate,
                    Channels::$ch,
                    $q,
                );
            }
        };
    }

    #[rustfmt::skip]
    mod cases {
        use super::*;
        long!(noise_tone_60s_stereo44_qm1, NoiseTone, 60.0, Hz44100, Stereo, -0.1);
        long!(noise_tone_60s_mono48_q10, NoiseTone, 60.0, Hz48000, Mono, 1.0);
        long!(noise_tone_45s_stereo48_q5, NoiseTone, 45.0, Hz48000, Stereo, 0.5);
        long!(noise_tone_30s_mono44_q0, NoiseTone, 30.0, Hz44100, Mono, 0.0);
        long!(transients_30s_mono44_q10, Transients, 30.0, Hz44100, Mono, 1.0);
        long!(transients_40s_stereo48_qm1, Transients, 40.0, Hz48000, Stereo, -0.1);
        long!(transients_30s_stereo44_q2, Transients, 30.0, Hz44100, Stereo, 0.2);
        long!(chirp_120s_mono44_q5, Chirp, 120.0, Hz44100, Mono, 0.5);
        long!(chirp_60s_mono48_qm1, Chirp, 60.0, Hz48000, Mono, -0.1);
        long!(chirp_45s_stereo48_q10, Chirp, 45.0, Hz48000, Stereo, 1.0);
    }
}

// --- Short-input sweep ---

/// Frame counts for the short-input sweep: every length 0..=130 (0 = empty
/// input, a single audio packet in libvorbis), every 7th
/// up to 2100, and the bs1 (2048) boundary.
fn short_input_lengths() -> Vec<usize> {
    let mut v: Vec<usize> = (0..=130).collect();
    v.extend((137..=2100).step_by(7));
    v.extend([2047, 2048, 2049]);
    v.sort_unstable();
    v.dedup();
    v
}

/// q-1 uses 512/4096 blocks, so its short-stream regime reaches twice as
/// far: every length 0..=64, every 37th up to 8400, and +-1 around the
/// short/long block sizes, the 64-frame chunk after them, and their
/// multiples (4095..4097, 4159..4161, 8191..8193, ...).
fn short_input_lengths_q_minus_1() -> Vec<usize> {
    let mut v: Vec<usize> = (0..=64).collect();
    v.extend((65..=8400).step_by(37));
    for edge in [
        256, 512, 576, 1024, 2048, 2112, 3072, 4096, 4160, 6144, 8192, 8256,
    ] {
        v.extend([edge - 1, edge, edge + 1]);
    }
    v.sort_unstable();
    v.dedup();
    v
}

/// Seeded noise + tone so the LPC pre/post extrapolation has real content.
fn short_input_signal(frames: usize, rate: u32, channels: u16) -> Vec<i16> {
    let mut rng = XorShift32(0xC0FF_EE11);
    let tau = 2.0 * std::f32::consts::PI;
    let mut out = Vec::with_capacity(frames * channels as usize);
    for i in 0..frames {
        let t = i as f32 / rate as f32;
        for c in 0..channels {
            let tone = f32::sin(tau * (440.0 + 220.0 * c as f32) * t) * 9000.0;
            out.push((tone + rng.next_f32() * 3000.0) as i16);
        }
    }
    out
}

/// Inputs shorter than bs1 used to diverge: libvorbis trains the EOS
/// post-extrapolation LPC on its own buffer, which for short streams still
/// starts with the bs1/2 pre-extrapolated samples. Collects every failing
/// length so a regression report shows the whole pattern.
fn run_short_input_sweep(
    rate: SampleRate,
    channels: Channels,
    base_quality: f32,
    range: std::ops::RangeInclusive<usize>,
) {
    let lengths = if base_quality < 0.0 {
        short_input_lengths_q_minus_1()
    } else {
        short_input_lengths()
    };
    let lengths: Vec<usize> = lengths.into_iter().filter(|f| range.contains(f)).collect();
    assert!(!lengths.is_empty(), "empty sweep range {range:?}");
    let mut failures = Vec::new();
    for frames in lengths {
        let samples = short_input_signal(frames, rate_hz(rate), channel_count(channels));
        let oracle_bytes = oracle_encode_q(
            &samples,
            rate_hz(rate),
            channel_count(channels),
            base_quality,
        );
        let serial = extract_serial(&oracle_bytes);
        let (vendor, encoder_tag) = extract_comment_strings(&oracle_bytes);
        let oxivorbis_bytes = common::oxivorbis_encode_q(
            &samples,
            rate,
            channels,
            base_quality,
            serial,
            Some(&vendor),
            Some(&encoder_tag),
        );
        if oxivorbis_bytes != oracle_bytes {
            failures.push(frames);
        }
    }
    assert!(
        failures.is_empty(),
        "base_quality {base_quality}: {} lengths diverged: {failures:?}",
        failures.len()
    );
}

/// `oracle_parity_short_input::<config>_<quality>::<length range>` — split
/// so nextest runs the ~430 lengths per config/quality in parallel.
mod oracle_parity_short_input {
    use super::*;

    macro_rules! sweep {
        ($name:ident, $rate:expr, $ch:expr, $q:expr) => {
            mod $name {
                use super::*;
                #[test]
                fn upto_130() {
                    run_short_input_sweep($rate, $ch, $q, 0..=130);
                }
                #[test]
                fn upto_1100() {
                    run_short_input_sweep($rate, $ch, $q, 131..=1100);
                }
                #[test]
                fn upto_2100() {
                    run_short_input_sweep($rate, $ch, $q, 1101..=2100);
                }
            }
        };
    }

    sweep!(mono44_q5, SampleRate::Hz44100, Channels::Mono, 0.5);
    sweep!(stereo48_q5, SampleRate::Hz48000, Channels::Stereo, 0.5);
    sweep!(mono44_q2, SampleRate::Hz44100, Channels::Mono, 0.2);
    sweep!(stereo48_q2, SampleRate::Hz48000, Channels::Stereo, 0.2);

    /// q-1 (512/4096 blocks): lengths up to 8400 frames, see
    /// [`short_input_lengths_q_minus_1`].
    macro_rules! sweep_q_minus_1 {
        ($name:ident, $rate:expr, $ch:expr, $q:expr) => {
            mod $name {
                use super::*;
                #[test]
                fn upto_130() {
                    run_short_input_sweep($rate, $ch, $q, 0..=130);
                }
                #[test]
                fn upto_2200() {
                    run_short_input_sweep($rate, $ch, $q, 131..=2200);
                }
                #[test]
                fn upto_4200() {
                    run_short_input_sweep($rate, $ch, $q, 2201..=4200);
                }
                #[test]
                fn upto_6200() {
                    run_short_input_sweep($rate, $ch, $q, 4201..=6200);
                }
                #[test]
                fn upto_8400() {
                    run_short_input_sweep($rate, $ch, $q, 6201..=8400);
                }
            }
        };
    }

    sweep_q_minus_1!(mono44_qm1, SampleRate::Hz44100, Channels::Mono, -0.1);
    sweep_q_minus_1!(stereo48_qm005, SampleRate::Hz48000, Channels::Stereo, -0.05);
}

// --- Minimized parity-fuzz repros ---

/// Repros saved before tools/parity-fuzz put the quality in case ids. They
/// were all found at the fuzzer's default base_quality, 0.5. New repros
/// must carry a `-q<quality>` tag.
const LEGACY_UNTAGGED_REPROS: &[&str] = &[
    "s2002-i676-44100-1ch",
    "s3003-i302-48000-1ch",
    "s3003-i4379-48000-2ch",
];

/// A parsed repro file name: `<id>-q<quality>[-f32]-<rate>-<ch>ch.<s16le|f32le>`
/// (the parity-fuzz case id plus the input format's extension).
#[derive(Debug, PartialEq)]
struct ReproName {
    quality: f32,
    f32_input: bool,
    rate: u32,
    channels: u16,
}

fn parse_repro_name(file_name: &str) -> Result<ReproName, String> {
    let (stem, ext) = file_name
        .rsplit_once('.')
        .ok_or_else(|| format!("{file_name}: no extension"))?;
    let ext_f32 = match ext {
        "s16le" => false,
        "f32le" => true,
        _ => return Err(format!("{file_name}: extension must be .s16le or .f32le")),
    };
    let (rest, ch) = stem
        .rsplit_once('-')
        .ok_or_else(|| format!("{file_name}: missing -<ch>ch"))?;
    let channels: u16 = ch
        .strip_suffix("ch")
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| format!("{file_name}: bad channel tag {ch:?}"))?;
    let (rest, rate) = rest
        .rsplit_once('-')
        .ok_or_else(|| format!("{file_name}: missing -<rate>"))?;
    let rate: u32 = rate
        .parse()
        .map_err(|_| format!("{file_name}: bad rate {rate:?}"))?;
    let (rest, tag_f32) = match rest.strip_suffix("-f32") {
        Some(r) => (r, true),
        None => (rest, false),
    };
    if tag_f32 != ext_f32 {
        return Err(format!(
            "{file_name}: -f32 tag and .{ext} extension disagree"
        ));
    }
    let quality = match rest.rsplit_once("-q") {
        Some((_, q)) => q
            .parse()
            .map_err(|_| format!("{file_name}: bad quality tag {q:?}"))?,
        None if LEGACY_UNTAGGED_REPROS.contains(&stem) => 0.5,
        None => {
            return Err(format!(
                "{file_name}: no -q<quality> tag (name repros like parity-fuzz case ids, \
                 e.g. s1-i42-q0.5-44100-2ch.s16le)"
            ));
        }
    };
    if !(-0.1..=1.0).contains(&quality)
        || ![44100, 48000].contains(&rate)
        || !(1..=2).contains(&channels)
    {
        return Err(format!("{file_name}: unsupported quality/rate/channels"));
    }
    Ok(ReproName {
        quality,
        f32_input: ext_f32,
        rate,
        channels,
    })
}

#[test]
fn oracle_parity_fuzz_repro_names() {
    let p = |s: &str| parse_repro_name(s);
    assert_eq!(
        p("s5151-i81-q-0.01-44100-2ch.s16le"),
        Ok(ReproName {
            quality: -0.01,
            f32_input: false,
            rate: 44100,
            channels: 2
        })
    );
    assert_eq!(
        p("s1-i7-q1-48000-1ch.s16le"),
        Ok(ReproName {
            quality: 1.0,
            f32_input: false,
            rate: 48000,
            channels: 1
        })
    );
    assert_eq!(
        p("replay-x-q0.2-f32-48000-2ch.f32le"),
        Ok(ReproName {
            quality: 0.2,
            f32_input: true,
            rate: 48000,
            channels: 2
        })
    );
    assert_eq!(p("s2002-i676-44100-1ch.s16le").unwrap().quality, 0.5);
    assert!(p("s9-i9-44100-1ch.s16le").is_err(), "untagged, not legacy");
    assert!(
        p("s9-i9-q0.5-44100-1ch.f32le").is_err(),
        "f32 ext without tag"
    );
    assert!(p("s9-i9-q0.5-22050-1ch.s16le").is_err());
}

/// Replays every minimized tools/parity-fuzz failure kept in
/// `tests/vectors/fuzz/` against the oracle. File names are parity-fuzz
/// case ids (`<id>-q<quality>[-f32]-<rate>-<ch>ch`) plus `.s16le` or
/// `.f32le`; see [`parse_repro_name`]. Any other file fails the test.
#[test]
fn oracle_parity_fuzz_repros() {
    let dir = std::env::current_dir().unwrap().join("tests/vectors/fuzz");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no repros in {}", dir.display());
    let mut failures = Vec::new();
    for path in &paths {
        let name = path.file_name().unwrap().to_str().unwrap();
        let repro = parse_repro_name(name).unwrap_or_else(|e| panic!("{e}"));
        let raw = std::fs::read(path).unwrap();
        let rate = if repro.rate == 48000 {
            SampleRate::Hz48000
        } else {
            SampleRate::Hz44100
        };
        let channels = if repro.channels == 2 {
            Channels::Stereo
        } else {
            Channels::Mono
        };
        let (oracle_bytes, oxivorbis_bytes) = if repro.f32_input {
            let samples: Vec<f32> = raw
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            let oracle =
                common::oracle_encode_raw(&raw, repro.rate, repro.channels, repro.quality, "f32");
            let serial = extract_serial(&oracle);
            let (vendor, tag) = extract_comment_strings(&oracle);
            let lw = oxivorbis::encode_f32_with_base_quality_and_meta(
                &samples,
                rate,
                channels,
                repro.quality,
                serial,
                Some(&vendor),
                Some(&tag),
            );
            (oracle, lw)
        } else {
            let samples: Vec<i16> = raw
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect();
            let oracle = oracle_encode_q(&samples, repro.rate, repro.channels, repro.quality);
            let serial = extract_serial(&oracle);
            let (vendor, tag) = extract_comment_strings(&oracle);
            let lw = common::oxivorbis_encode_q(
                &samples,
                rate,
                channels,
                repro.quality,
                serial,
                Some(&vendor),
                Some(&tag),
            );
            (oracle, lw)
        };
        if oxivorbis_bytes != oracle_bytes {
            failures.push(name.to_string());
        }
    }
    assert!(failures.is_empty(), "diverged: {failures:?}");
}
