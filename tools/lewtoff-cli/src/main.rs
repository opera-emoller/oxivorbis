//! Thin CLI: read interleaved s16le PCM from stdin, encode to Ogg Vorbis,
//! write to stdout. Exists for hyperfine-style benchmarking against
//! `oggenc -q <quality>`. Usage:
//!
//!     lewtoff <rate> <channels> [quality] < input.s16le > output.ogg
//!
//! `rate` must be 44100 or 48000; `channels` must be 1 or 2 (or `mono`/`stereo`);
//! `quality` is on oggenc's `-q` scale, 0..10 (fractional ok), default 5.

use std::io::{Read, Write};

fn die(msg: &str) -> ! {
    eprintln!("lewtoff: {msg}");
    eprintln!(
        "usage: lewtoff <44100|48000> <1|2|mono|stereo> [quality 0..10] < in.s16le > out.ogg"
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 2 && args.len() != 3 {
        die("expected 2 or 3 arguments: <rate> <channels> [quality]");
    }
    let quality: Option<f32> = args.get(2).map(|q| match q.parse::<f32>() {
        Ok(v) if (0.0..=10.0).contains(&v) => v,
        _ => die(&format!("unsupported quality {q:?}; want 0..10")),
    });
    let rate = match args[0].as_str() {
        "44100" => lewtoff::SampleRate::Hz44100,
        "48000" => lewtoff::SampleRate::Hz48000,
        other => die(&format!("unsupported rate {other:?}; want 44100 or 48000")),
    };
    let channels = match args[1].as_str() {
        "1" | "mono" => lewtoff::Channels::Mono,
        "2" | "stereo" => lewtoff::Channels::Stereo,
        other => die(&format!(
            "unsupported channels {other:?}; want 1/2/mono/stereo"
        )),
    };

    let mut raw = Vec::new();
    std::io::stdin()
        .read_to_end(&mut raw)
        .expect("read stdin failed");

    let samples: Vec<i16> = raw
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();

    let ogg = match quality {
        Some(q) => lewtoff::encode_with_quality(&samples, rate, channels, q),
        None => lewtoff::encode(&samples, rate, channels),
    };

    let stdout = std::io::stdout();
    let mut h = stdout.lock();
    h.write_all(&ogg).expect("write stdout failed");
}
