//! Synthesize planar (one buffer per channel) stereo audio and stream it
//! to an Ogg Vorbis file, the way a DSP pipeline that works per channel
//! would.
//!
//! ```sh
//! cargo run --release --example planar -- [output.ogg]
//! ```
//!
//! Writes 5 s of a two-tone chord with a slow stereo pan, in 512-frame
//! blocks, to `output.ogg` (default: `planar.ogg` in the temp directory).

use oxivorbis::{Channels, OggWriter, Options, Quality, SampleRate};
use std::f32::consts::TAU;
use std::fs::File;
use std::io::BufWriter;
use std::process::ExitCode;

const RATE: u32 = 48_000;
const BLOCK: usize = 512;
const SECONDS: usize = 5;

fn run() -> std::io::Result<std::path::PathBuf> {
    let path = std::env::args_os()
        .nth(1)
        .map(Into::into)
        .unwrap_or_else(|| std::env::temp_dir().join("planar.ogg"));

    let opts = Options::default()
        .with_quality(Quality::new(4.0).expect("in range"))
        .with_comment("TITLE", "oxivorbis planar example");
    let sink = BufWriter::new(File::create(&path)?);
    let mut ogg = OggWriter::new(sink, SampleRate::HZ_48000, Channels::STEREO, &opts)?;

    // Per-channel block buffers, reused for every block.
    let mut left = vec![0.0f32; BLOCK];
    let mut right = vec![0.0f32; BLOCK];
    let total = SECONDS * RATE as usize;
    for start in (0..total).step_by(BLOCK) {
        let n = BLOCK.min(total - start);
        for (i, (l, r)) in left[..n].iter_mut().zip(&mut right[..n]).enumerate() {
            let t = (start + i) as f32 / RATE as f32;
            let tone = 0.3 * (TAU * 261.63 * t).sin() + 0.2 * (TAU * 392.0 * t).sin();
            let pan = 0.5 + 0.5 * (TAU * 0.2 * t).sin(); // 0 = left, 1 = right
            *l = tone * (1.0 - pan);
            *r = tone * pan;
        }
        ogg.write_planar_f32(&[&left[..n], &right[..n]])?;
    }
    ogg.finish()?; // flushes the BufWriter
    Ok(path)
}

fn main() -> ExitCode {
    match run() {
        Ok(path) => {
            let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            eprintln!("wrote {} ({len} bytes)", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("planar: {e}");
            ExitCode::FAILURE
        }
    }
}
