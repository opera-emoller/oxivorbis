//! debug-rust-dump: trigger OXIVORBIS_DEBUG_DUMP=1 on a 440 Hz sine input
//! matching the C harness (1 second, 44100 Hz mono, peak amplitude 32767).
//!
//! Run with: OXIVORBIS_DEBUG_DUMP=1 cargo run --bin debug-rust-dump
//! Dumps land in /tmp/oxivorbis-debug/r_*.

fn main() {
    // Dumps are only compiled into debug builds. Edition 2024 makes env::set_var unsafe (and the crate forbids unsafe),
    // so the caller must set OXIVORBIS_DEBUG_DUMP=1 (`just debug-dump-rust` does).
    if std::env::var_os("OXIVORBIS_DEBUG_DUMP").is_none() {
        eprintln!("debug-rust-dump: set OXIVORBIS_DEBUG_DUMP=1 (or run `just debug-dump-rust`)");
        std::process::exit(2);
    }

    let rate_hz = 44100usize;
    let duration_secs = 1usize;
    let n_samples = rate_hz * duration_secs;
    let freq = 440.0f32;
    let amplitude = 16384.0f32;

    let samples: Vec<i16> = (0..n_samples)
        .map(|i| {
            let t = i as f32 / rate_hz as f32;
            let v = amplitude * (2.0 * std::f32::consts::PI * freq * t).sin();
            v as i16
        })
        .collect();

    oxivorbis::encode_with_serial(
        &samples,
        oxivorbis::SampleRate::HZ_44100,
        oxivorbis::Channels::MONO,
        0,
    );

    println!("Dumps written to /tmp/oxivorbis-debug/r_*");
}
