//! Stream raw interleaved s16le PCM from stdin to Ogg Vorbis on stdout.
//!
//! ```sh
//! ffmpeg -i song.flac -f s16le -ar 44100 -ac 2 - \
//!     | cargo run --release --example stream -- 44100 2 > song.ogg
//! ```
//!
//! Arguments: `<44100|48000> <1|2> [quality -1..10]` (quality defaults to
//! 5). Pages are written as soon as the encoder completes them, so memory
//! stays bounded for any input length.

use oxivorbis::{Channels, Encoder, Options, Quality, SampleRate};
use std::io::{Read, Write};
use std::process::ExitCode;

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 2 && args.len() != 3 {
        return Err(
            "usage: stream <44100|48000> <1|2> [quality -1..10] < in.s16le > out.ogg".into(),
        );
    }
    let rate: u32 = args[0].parse().map_err(|_| "rate must be a number")?;
    let rate = SampleRate::try_from(rate).map_err(|e| e.to_string())?;
    let channels: u16 = args[1].parse().map_err(|_| "channels must be a number")?;
    let channels = Channels::try_from(channels).map_err(|e| e.to_string())?;
    let mut opts = Options::default();
    if let Some(q) = args.get(2) {
        opts.quality = q
            .parse()
            .ok()
            .and_then(Quality::new)
            .ok_or_else(|| format!("quality must be a number in -1..=10, got {q:?}"))?;
    }

    let mut enc = Encoder::new(rate, channels, &opts);
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    let mut buf = vec![0u8; 64 * 1024];
    let mut samples = Vec::with_capacity(buf.len() / 2 + 1);
    let mut odd_byte: Option<u8> = None; // a read can end mid-sample

    loop {
        let n = match stdin.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("stdin: {e}")),
        };
        samples.clear();
        let mut bytes = &buf[..n];
        if let Some(lo) = odd_byte.take() {
            samples.push(i16::from_le_bytes([lo, bytes[0]]));
            bytes = &bytes[1..];
        }
        let mut pairs = bytes.chunks_exact(2);
        samples.extend(pairs.by_ref().map(|s| i16::from_le_bytes([s[0], s[1]])));
        odd_byte = pairs.remainder().first().copied();

        // `write` accepts any number of samples, even ones that split a
        // stereo frame, and returns the Ogg pages completed so far.
        stdout
            .write_all(enc.write(&samples))
            .map_err(|e| format!("stdout: {e}"))?;
    }
    stdout
        .write_all(&enc.finish())
        .and_then(|()| stdout.flush())
        .map_err(|e| format!("stdout: {e}"))?;
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("stream: {e}");
            ExitCode::FAILURE
        }
    }
}
