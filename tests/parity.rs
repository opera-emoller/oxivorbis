//! Byte-parity tests: lewtoff vs oggenc (libvorbis) -q 5.
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

use lewtoff::{Channels, SampleRate};
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
    let rate_hz: u32 = match rate {
        SampleRate::Hz44100 => 44100,
        SampleRate::Hz48000 => 48000,
    };
    let ch: u16 = match channels {
        Channels::Mono => 1,
        Channels::Stereo => 2,
    };
    let oracle_bytes = oracle_encode_q5(samples, rate_hz, ch);
    let serial = extract_serial(&oracle_bytes);
    let (vendor, encoder_tag) = extract_comment_strings(&oracle_bytes);
    let lewtoff_bytes = lewtoff::encode_with_serial_and_meta(
        samples,
        rate,
        channels,
        serial,
        Some(&vendor),
        Some(&encoder_tag),
    );
    if lewtoff_bytes != oracle_bytes {
        let div = first_diff(&lewtoff_bytes, &oracle_bytes);
        let lw_end = (div + 16).min(lewtoff_bytes.len());
        let or_end = (div + 16).min(oracle_bytes.len());
        let start = div.saturating_sub(8);
        panic!(
            "ORACLE parity diverged at byte {div}\n  lewtoff len: {}\n  oracle  len: {}\n  lewtoff ctx: {:02x?}\n  oracle  ctx: {:02x?}",
            lewtoff_bytes.len(),
            oracle_bytes.len(),
            &lewtoff_bytes[start..lw_end],
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
    let rate_hz: u32 = match rate {
        SampleRate::Hz44100 => 44100,
        SampleRate::Hz48000 => 48000,
    };
    let ch: u16 = match channels {
        Channels::Mono => 1,
        Channels::Stereo => 2,
    };

    let oracle_bytes = oggenc_encode_q5(samples, rate_hz, ch);
    let serial = extract_serial(&oracle_bytes);
    let (vendor, encoder_tag) = extract_comment_strings(&oracle_bytes);
    let lewtoff_bytes = lewtoff::encode_with_serial_and_meta(
        samples,
        rate,
        channels,
        serial,
        Some(&vendor),
        Some(&encoder_tag),
    );

    if lewtoff_bytes != oracle_bytes {
        let div = first_diff(&lewtoff_bytes, &oracle_bytes);
        let lw_start = div.saturating_sub(8);
        let lw_end = (div + 16).min(lewtoff_bytes.len());
        let or_end = (div + 16).min(oracle_bytes.len());
        let or_start = div.saturating_sub(8).min(oracle_bytes.len());
        panic!(
            "parity diverged at byte {div}\n  lewtoff len: {}\n  oggenc  len: {}\n  lewtoff ctx: {:02x?}\n  oggenc  ctx: {:02x?}",
            lewtoff_bytes.len(),
            oracle_bytes.len(),
            &lewtoff_bytes[lw_start..lw_end],
            &oracle_bytes[or_start..or_end],
        );
    }
}

/// `lewtoff::oggenc_base_quality` must reproduce oggenc's `-q` float
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
        let got = lewtoff::encode_with_base_quality_and_meta(
            &samples,
            SampleRate::Hz44100,
            Channels::Stereo,
            lewtoff::oggenc_base_quality(qf),
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
    assert_ne!(lewtoff::oggenc_base_quality(9.344), naive(9.344));
    assert_ne!(lewtoff::oggenc_base_quality(9.303), naive(9.303));
    assert_ne!(lewtoff::oggenc_base_quality(9.344), 9.344f32 * 0.1f32);
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
    let lewtoff_bytes = lewtoff::encode_with_serial_and_meta(
        &samples,
        SampleRate::Hz44100,
        Channels::Stereo,
        serial,
        Some(&vendor),
        Some(&encoder_tag),
    );
    std::fs::write("/tmp/oracle_ramp.ogg", &oracle_bytes).unwrap();
    std::fs::write("/tmp/lw_ramp.ogg", &lewtoff_bytes).unwrap();
    eprintln!(
        "Wrote oracle_ramp.ogg ({} bytes) and lw_ramp.ogg ({} bytes)",
        oracle_bytes.len(),
        lewtoff_bytes.len()
    );
}

#[test]
#[ignore = "manual dump for parity-diff analysis"]
fn parity_dump_files() {
    let samples = vec![0i16; 44100];
    let oracle_bytes = oggenc_encode_q5(&samples, 44100, 1);
    let serial = extract_serial(&oracle_bytes);
    let lewtoff_bytes =
        lewtoff::encode_with_serial(&samples, SampleRate::Hz44100, Channels::Mono, serial);
    std::fs::write("/tmp/oggenc_parity.ogg", &oracle_bytes).unwrap();
    std::fs::write("/tmp/lw_parity.ogg", &lewtoff_bytes).unwrap();
    eprintln!(
        "Wrote oggenc_parity.ogg ({} bytes) and lw_parity.ogg ({} bytes)",
        oracle_bytes.len(),
        lewtoff_bytes.len()
    );
}

#[test]
#[ignore = "manual dump for parity-diff analysis — sine wave"]
fn parity_dump_sine_files() {
    let samples = make_sine_mono(44100, 440.0, 1.0);
    let oracle_bytes = oggenc_encode_q5(&samples, 44100, 1);
    let serial = extract_serial(&oracle_bytes);
    let (vendor, encoder_tag) = extract_comment_strings(&oracle_bytes);
    let lewtoff_bytes = lewtoff::encode_with_serial_and_meta(
        &samples,
        SampleRate::Hz44100,
        Channels::Mono,
        serial,
        Some(&vendor),
        Some(&encoder_tag),
    );
    std::fs::write("/tmp/oggenc_sine.ogg", &oracle_bytes).unwrap();
    std::fs::write("/tmp/lw_sine.ogg", &lewtoff_bytes).unwrap();
    eprintln!(
        "Wrote oggenc_sine.ogg ({} bytes) and lw_sine.ogg ({} bytes)",
        oracle_bytes.len(),
        lewtoff_bytes.len()
    );
}

// Real-audio corpus parity is now in tests/corpus_sweep.rs — it walks
// <repo_root>/corpus/ recursively and runs every file through the same
// assert_parity_oracle path. That's gated by the `oracle` feature and
// `#[ignore]` (manual run) since it depends on an out-of-tree corpus.

// --- Quality-parameterized oracle parity ---

fn rate_hz(rate: SampleRate) -> u32 {
    match rate {
        SampleRate::Hz44100 => 44100,
        SampleRate::Hz48000 => 48000,
    }
}

fn channel_count(channels: Channels) -> u16 {
    match channels {
        Channels::Mono => 1,
        Channels::Stereo => 2,
    }
}

/// Oracle parity at an arbitrary libvorbis base_quality (-0.1..1.0).
///
/// The oracle gets `base_quality` as its 3rd argument; lewtoff goes through
/// `common::lewtoff_encode_q`. q-1 (base_quality < 0) is not supported yet
/// and its matrix entries are `#[ignore]`d.
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
    let lewtoff_bytes = common::lewtoff_encode_q(
        samples,
        rate,
        channels,
        base_quality,
        serial,
        Some(&vendor),
        Some(&encoder_tag),
    );
    if lewtoff_bytes != oracle_bytes {
        let div = first_diff(&lewtoff_bytes, &oracle_bytes);
        let start = div.saturating_sub(8);
        let lw_end = (div + 16).min(lewtoff_bytes.len());
        let or_end = (div + 16).min(oracle_bytes.len());
        panic!(
            "ORACLE parity (base_quality {base_quality}) diverged at byte {div}\n  lewtoff len: {}\n  oracle  len: {}\n  lewtoff ctx: {:02x?}\n  oracle  ctx: {:02x?}",
            lewtoff_bytes.len(),
            oracle_bytes.len(),
            &lewtoff_bytes[start.min(lewtoff_bytes.len())..lw_end],
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

    quality!(q_m005, -0.05, #[ignore = "pending 512/4096 block support"]);
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
/// lewtoff's page accounting used to carry its byte/packet counters across
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
