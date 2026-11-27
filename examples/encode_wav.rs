//! Encode a 16-bit PCM WAV file to Ogg Vorbis.
//!
//! ```sh
//! cargo run --release --example encode_wav -- input.wav output.ogg [quality]
//! ```
//!
//! `quality` is on oggenc's `-q` scale (-1..10, default 5). The WAV must be
//! 16-bit integer PCM, 1 Hz to 200 kHz, mono or stereo. The RIFF parser
//! below is deliberately minimal (no dependencies); use a crate such as
//! `hound` or `symphonia` for anything more general.

use oxivorbis::{Channels, Options, Quality, SampleRate};
use std::process::ExitCode;

struct Wav {
    rate: u32,
    channels: u16,
    samples: Vec<i16>,
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// Parse a RIFF/WAVE file holding 16-bit PCM (`WAVE_FORMAT_PCM`, or
/// `WAVE_FORMAT_EXTENSIBLE` with a PCM sub-format).
fn parse_wav(b: &[u8]) -> Result<Wav, String> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    let mut pos = 12;
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let len = u32_at(b, pos + 4) as usize;
        let body = &b[pos + 8..(pos + 8).saturating_add(len).min(b.len())];
        match id {
            b"fmt " if body.len() >= 16 => {
                let mut tag = u16_at(body, 0);
                if tag == 0xFFFE && body.len() >= 26 {
                    tag = u16_at(body, 24); // first two bytes of the sub-format GUID
                }
                fmt = Some((tag, u16_at(body, 2), u32_at(body, 4), u16_at(body, 14)));
            }
            b"data" => {
                let (tag, channels, rate, bits) = fmt.ok_or("data chunk before fmt chunk")?;
                if tag != 1 || bits != 16 {
                    return Err(format!(
                        "only 16-bit integer PCM is supported (format {tag:#x}, {bits} bits)"
                    ));
                }
                let samples = body
                    .chunks_exact(2)
                    .map(|s| i16::from_le_bytes([s[0], s[1]]))
                    .collect();
                return Ok(Wav {
                    rate,
                    channels,
                    samples,
                });
            }
            _ => {}
        }
        pos = pos.saturating_add(8 + len + (len & 1)); // chunks are word-aligned
    }
    Err("no data chunk".into())
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (input, output) = match args.as_slice() {
        [i, o] | [i, o, _] => (i, o),
        _ => return Err("usage: encode_wav <input.wav> <output.ogg> [quality -1..10]".into()),
    };
    let quality = match args.get(2) {
        Some(q) => q
            .parse()
            .ok()
            .and_then(Quality::new)
            .ok_or_else(|| format!("quality must be a number in -1..=10, got {q:?}"))?,
        None => Quality::Q5,
    };

    let bytes = std::fs::read(input).map_err(|e| format!("{input}: {e}"))?;
    let wav = parse_wav(&bytes).map_err(|e| format!("{input}: {e}"))?;
    let rate = SampleRate::try_from(wav.rate).map_err(|e| e.to_string())?;
    let channels = Channels::try_from(wav.channels).map_err(|e| e.to_string())?;

    let opts = Options::default().with_quality(quality);
    let ogg = oxivorbis::encode(&wav.samples, rate, channels, &opts);
    std::fs::write(output, &ogg).map_err(|e| format!("{output}: {e}"))?;

    let frames = wav.samples.len() / channels.count() as usize;
    eprintln!(
        "{input}: {:.2} s, {} Hz, {} ch -> {output}: {} bytes at q{}",
        frames as f64 / rate.hz() as f64,
        rate.hz(),
        channels.count(),
        ogg.len(),
        quality.get(),
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("encode_wav: {e}");
            ExitCode::FAILURE
        }
    }
}
