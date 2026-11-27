//! Stream a 16-bit PCM WAV file to an Ogg Vorbis file with bounded memory.
//!
//! ```sh
//! cargo run --release --example encode_file -- input.wav output.ogg [quality]
//! ```
//!
//! Unlike `encode_wav`, which reads the whole file and calls
//! `oxivorbis::encode`, this reads only the RIFF header, then
//! `std::io::copy`s the data chunk into an [`S16LeWriter`] over a
//! `BufWriter<File>`. Pages reach the output file as they are completed,
//! so memory use does not depend on the input length.
//!
//! The WAV must be 16-bit integer PCM, 44100 or 48000 Hz, mono or stereo.

use oxivorbis::{Channels, Options, Quality, S16LeWriter, SampleRate};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read};
use std::process::ExitCode;

/// Format of a WAV file and the length of its data chunk.
struct WavHeader {
    rate: u32,
    channels: u16,
    data_len: u64,
}

fn read_bytes<const N: usize>(r: &mut impl Read) -> io::Result<[u8; N]> {
    let mut b = [0u8; N];
    r.read_exact(&mut b)?;
    Ok(b)
}

/// Read chunks up to the start of the data chunk, leaving `r` positioned
/// at the first sample.
fn read_header(r: &mut impl Read) -> Result<WavHeader, String> {
    let e = |e: io::Error| e.to_string();
    let riff: [u8; 12] = read_bytes(r).map_err(e)?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    let mut fmt: Option<(u16, u16, u32)> = None; // (channels, bits, rate)
    loop {
        let head: [u8; 8] = read_bytes(r).map_err(|_| "no data chunk")?;
        let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as u64;
        match &head[0..4] {
            b"fmt " => {
                if len > 1024 {
                    return Err("oversized fmt chunk".into());
                }
                let mut body = vec![0u8; len as usize];
                r.read_exact(&mut body).map_err(e)?;
                if body.len() < 16 {
                    return Err("short fmt chunk".into());
                }
                let u16_at = |i: usize| u16::from_le_bytes([body[i], body[i + 1]]);
                let mut tag = u16_at(0);
                if tag == 0xFFFE && body.len() >= 26 {
                    tag = u16_at(24); // WAVE_FORMAT_EXTENSIBLE sub-format
                }
                let rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                if tag != 1 {
                    return Err(format!("only integer PCM is supported (format {tag:#x})"));
                }
                fmt = Some((u16_at(2), u16_at(14), rate));
                if len % 2 == 1 {
                    read_bytes::<1>(r).map_err(e)?;
                }
            }
            b"data" => {
                let (channels, bits, rate) = fmt.ok_or("data chunk before fmt chunk")?;
                if bits != 16 {
                    return Err(format!("only 16-bit PCM is supported ({bits} bits)"));
                }
                return Ok(WavHeader {
                    rate,
                    channels,
                    data_len: len,
                });
            }
            _ => {
                // Skip other chunks (LIST, fact, ...), word-aligned.
                io::copy(&mut r.take(len + (len & 1)), &mut io::sink()).map_err(e)?;
            }
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (input, output) = match args.as_slice() {
        [i, o] | [i, o, _] => (i, o),
        _ => return Err("usage: encode_file <input.wav> <output.ogg> [quality -1..10]".into()),
    };
    let quality = match args.get(2) {
        Some(q) => q
            .parse()
            .ok()
            .and_then(Quality::new)
            .ok_or_else(|| format!("quality must be a number in -1..=10, got {q:?}"))?,
        None => Quality::Q5,
    };

    let mut reader = BufReader::new(File::open(input).map_err(|e| format!("{input}: {e}"))?);
    let wav = read_header(&mut reader).map_err(|e| format!("{input}: {e}"))?;
    let rate = SampleRate::try_from(wav.rate).map_err(|e| e.to_string())?;
    let channels = Channels::try_from(wav.channels).map_err(|e| e.to_string())?;

    let sink = BufWriter::new(File::create(output).map_err(|e| format!("{output}: {e}"))?);
    let opts = Options::default().with_quality(quality);
    let mut writer =
        S16LeWriter::new(sink, rate, channels, &opts).map_err(|e| format!("{output}: {e}"))?;

    // The data chunk's bytes go straight in; reads may split samples.
    let copied = io::copy(&mut reader.take(wav.data_len), &mut writer)
        .map_err(|e| format!("{input} -> {output}: {e}"))?;
    let file = writer
        .finish()
        .and_then(|buf| buf.into_inner().map_err(|e| e.into_error()))
        .map_err(|e| format!("{output}: {e}"))?;
    let out_len = file.metadata().map(|m| m.len()).unwrap_or(0);

    let frames = copied / (2 * channels.count() as u64);
    eprintln!(
        "{input}: {:.2} s, {} Hz, {} ch -> {output}: {out_len} bytes at q{}",
        frames as f64 / rate.hz() as f64,
        rate.hz(),
        channels.count(),
        quality.get(),
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("encode_file: {e}");
            ExitCode::FAILURE
        }
    }
}
