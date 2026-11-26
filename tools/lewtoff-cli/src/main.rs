//! Thin CLI: read interleaved s16le PCM from stdin, encode to Ogg Vorbis,
//! write to stdout. Exists for hyperfine-style benchmarking against
//! `oggenc -q <quality>`. Usage:
//!
//!     lewtoff [--stream] <rate> <channels> [quality] < input.s16le > output.ogg
//!
//! `rate` must be 44100 or 48000; `channels` must be 1 or 2 (or `mono`/`stereo`);
//! `quality` is on oggenc's `-q` scale, -1..10 (fractional ok), default 5.
//! By default the whole input is read, then encoded one-shot. `--stream`
//! instead pipes stdin through `lewtoff::Encoder` in 64 KiB reads, writing
//! pages as they complete (memory stays bounded for any input length).

use std::io::{Read, Write};

fn die(msg: &str) -> ! {
    eprintln!("lewtoff: {msg}");
    eprintln!(
        "usage: lewtoff [--stream] <44100|48000> <1|2|mono|stereo> [quality -1..10] < in.s16le > out.ogg"
    );
    std::process::exit(2);
}

/// `r.read(buf)`, retried when a signal interrupts it (EINTR) before any
/// data was read.
fn read_retrying(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        match r.read(buf) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            other => return other,
        }
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let stream = args.first().is_some_and(|a| a == "--stream");
    if stream {
        args.remove(0);
    }
    if args.len() != 2 && args.len() != 3 {
        die("expected 2 or 3 arguments: <rate> <channels> [quality]");
    }
    let quality = match args.get(2) {
        None => lewtoff::Quality::Q5,
        Some(q) => q
            .parse::<f32>()
            .ok()
            .and_then(lewtoff::Quality::new)
            .unwrap_or_else(|| die(&format!("unsupported quality {q:?}; want -1..10"))),
    };
    let opts = lewtoff::Options::default().with_quality(quality);
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

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if stream {
        let mut enc = lewtoff::Encoder::new(rate, channels, &opts);
        let mut stdin = std::io::stdin().lock();
        let mut buf = vec![0u8; 1 << 16];
        let mut carry: Option<u8> = None;
        let mut samples: Vec<i16> = Vec::with_capacity(buf.len() / 2 + 1);
        loop {
            let n = read_retrying(&mut stdin, &mut buf)
                .unwrap_or_else(|e| die(&format!("read stdin failed: {e}")));
            if n == 0 {
                break;
            }
            samples.clear();
            let mut bytes = &buf[..n];
            if let Some(lo) = carry.take() {
                samples.push(i16::from_le_bytes([lo, bytes[0]]));
                bytes = &bytes[1..];
            }
            let mut it = bytes.chunks_exact(2);
            samples.extend(it.by_ref().map(|c| i16::from_le_bytes([c[0], c[1]])));
            carry = it.remainder().first().copied();
            out.write_all(enc.write(&samples))
                .expect("write stdout failed");
        }
        out.write_all(&enc.finish()).expect("write stdout failed");
        return;
    }

    let mut raw = Vec::new();
    std::io::stdin()
        .read_to_end(&mut raw)
        .expect("read stdin failed");

    let samples: Vec<i16> = raw
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();

    let ogg = lewtoff::encode(&samples, rate, channels, &opts);
    out.write_all(&ogg).expect("write stdout failed");
}

#[cfg(test)]
mod tests {
    use super::read_retrying;
    use std::io::{self, Read};

    /// Fails with `Interrupted` on every other call.
    struct Flaky<'a>(&'a [u8], bool);
    impl Read for Flaky<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.1 = !self.1;
            if self.1 {
                return Err(io::ErrorKind::Interrupted.into());
            }
            self.0.read(buf)
        }
    }

    #[test]
    fn retries_interrupted_reads() {
        let mut r = Flaky(b"abc", false);
        let mut buf = [0u8; 8];
        assert_eq!(read_retrying(&mut r, &mut buf).unwrap(), 3);
        assert_eq!(&buf[..3], b"abc");
        assert_eq!(read_retrying(&mut r, &mut buf).unwrap(), 0);
    }

    #[test]
    fn other_errors_propagate() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
        }
        let err = read_retrying(&mut Broken, &mut [0u8; 4]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }
}
