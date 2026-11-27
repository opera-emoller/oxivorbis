//! debug-diff: compare C and Rust debug dumps to find the first diverging layer.
//!
//! Usage: cargo run -p debug-diff (or `just debug-diff`)
//! Expects /tmp/oxivorbis-debug/c_* (`just debug-dump-c`) and r_*
//! (`just debug-dump-rust`) to already exist.

use std::fs;

fn read_f32_bin(path: &str) -> Vec<f32> {
    let bytes = fs::read(path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
    assert!(
        bytes.len().is_multiple_of(4),
        "{path}: length not multiple of 4"
    );
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn read_i32_bin(path: &str) -> Vec<i32> {
    let bytes = fs::read(path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
    assert!(
        bytes.len().is_multiple_of(4),
        "{path}: length not multiple of 4"
    );
    bytes
        .chunks_exact(4)
        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn read_txt(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

fn compare_f32(name: &str, c: &[f32], r: &[f32]) -> bool {
    let len = c.len().min(r.len());
    let mut max_abs = 0.0f32;
    let mut max_ulp = 0u32;
    for i in 0..len {
        let abs_diff = (c[i] - r[i]).abs();
        let ulp_diff = c[i].to_bits().abs_diff(r[i].to_bits());
        if abs_diff > max_abs {
            max_abs = abs_diff;
        }
        if ulp_diff > max_ulp {
            max_ulp = ulp_diff;
        }
        if abs_diff > 0.001 {
            println!(
                "{name} DIVERGES at index {i} (c={:.6}, r={:.6}, abs={:.6}, ulp={})",
                c[i], r[i], abs_diff, ulp_diff
            );
            let start = i.saturating_sub(2);
            let end = (i + 5).min(len);
            for j in start..end {
                let ad = (c[j] - r[j]).abs();
                let ud = c[j].to_bits().abs_diff(r[j].to_bits());
                let marker = if j == i { " <--" } else { "" };
                println!(
                    "  [{j}] c={:.6} r={:.6} abs={:.6} ulp={}{}",
                    c[j], r[j], ad, ud, marker
                );
            }
            return false;
        }
    }
    // Some Rust dumps write a whole scratch buffer (e.g. logfft is sized
    // for the long block, 2048 floats) while libvorbis dumps exactly the
    // n/2(+1) values of the block; only that prefix is meaningful.
    if c.len() > r.len() {
        println!(
            "{name} length mismatch: c={} r={} (max_abs={:.2e} max_ulp={})",
            c.len(),
            r.len(),
            max_abs,
            max_ulp
        );
        return false;
    }
    let note = if r.len() > c.len() {
        format!(" of rust's {}", r.len())
    } else {
        String::new()
    };
    println!(
        "{name} matches ({len} elements{note}, max_abs={:.2e}, max_ulp={})",
        max_abs, max_ulp
    );
    true
}

fn compare_i32(name: &str, c: &[i32], r: &[i32]) -> bool {
    let len = c.len().min(r.len());
    for i in 0..len {
        if c[i] != r[i] {
            println!("{name} DIVERGE at index {i} (c={}, r={})", c[i], r[i]);
            let start = i.saturating_sub(2);
            let end = (i + 5).min(len);
            for j in start..end {
                let marker = if j == i { " <--" } else { "" };
                println!("  [{j}] c={} r={}{}", c[j], r[j], marker);
            }
            return false;
        }
    }
    if c.len() != r.len() {
        println!("{name} length mismatch: c={}, r={}", c.len(), r.len());
        return false;
    }
    println!("{name} matches ({} elements)", len);
    true
}

fn main() {
    let base = "/tmp/oxivorbis-debug";

    println!("=== Layer diff: C vs Rust first short block ===\n");

    let c_windowed = read_f32_bin(&format!("{base}/c_windowed.bin"));
    let r_windowed = read_f32_bin(&format!("{base}/r_windowed.bin"));
    if !compare_f32("windowed_pcm", &c_windowed, &r_windowed) {
        println!("\nFirst divergence: windowed_pcm");
        println!(
            "Hypothesis: window.rs has a bug for short blocks (LPC preextrapolation or overlap)."
        );
        return;
    }

    let c_drft = read_f32_bin(&format!("{base}/c_drft.bin"));
    let r_drft = read_f32_bin(&format!("{base}/r_drft.bin"));
    if !compare_f32("drft", &c_drft, &r_drft) {
        println!("\nFirst divergence: drft");
        println!("Hypothesis: drft.rs port has a numerical bug.");
        return;
    }

    let c_logfft = read_f32_bin(&format!("{base}/c_logfft.bin"));
    let r_logfft = read_f32_bin(&format!("{base}/r_logfft.bin"));
    if !compare_f32("logfft", &c_logfft, &r_logfft) {
        println!("\nFirst divergence: logfft");
        println!(
            "Hypothesis: mapping0.rs logfft computation has a bug (maybe sqrt/todB or scale_dB)."
        );
        return;
    }

    let c_mask = read_f32_bin(&format!("{base}/c_mask.bin"));
    let r_mask = read_f32_bin(&format!("{base}/r_mask.bin"));
    if !compare_f32("mask", &c_mask, &r_mask) {
        println!("\nFirst divergence: mask");
        println!("Hypothesis: psy.rs has a bug in noise/tone masking or vp_offset_and_mix.");
        return;
    }

    let c_floor_count_str = read_txt(&format!("{base}/c_floor_count.txt"));
    let c_floor_count: usize = c_floor_count_str
        .trim()
        .parse()
        .expect("c_floor_count.txt parse");
    let c_posts_bytes = fs::read(format!("{base}/c_floor_posts.bin")).expect("c_floor_posts.bin");
    let c_floor_posts: Vec<i32> = c_posts_bytes
        .chunks_exact(4)
        .take(c_floor_count)
        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let r_floor_posts = read_i32_bin(&format!("{base}/r_floor_posts.bin"));

    if !compare_i32("floor_posts", &c_floor_posts, &r_floor_posts) {
        println!("\nFirst divergence: floor_posts");
        println!("Hypothesis: floor1.rs fit logic has a bug.");
        return;
    }

    let c_floor_bits_str = read_txt(&format!("{base}/c_floor_bits.txt"));
    // Format: one "<value> <bits>" line per entry, then
    // "# total_floor_bits=<n>".
    let c_floor_bits: usize = c_floor_bits_str
        .split_once("total_floor_bits=")
        .and_then(|(_, n)| n.trim().parse().ok())
        .expect("parse total_floor_bits= in c_floor_bits.txt");
    let r_floor_bits_str = read_txt(&format!("{base}/r_floor_bits.txt"));
    let r_floor_bits: usize = r_floor_bits_str
        .trim()
        .parse()
        .expect("r_floor_bits.txt parse");

    if c_floor_bits != r_floor_bits {
        println!("floor_bits DIVERGE (c={c_floor_bits}, r={r_floor_bits})");
        println!("\nFirst divergence: floor_bits");
        println!("Hypothesis: floor1.rs encode has a bug — posts match but bit-emission differs.");
        println!("This points at the codebook lookups for partition values.");
        return;
    }
    println!("floor_bits matches ({c_floor_bits} bits)");

    println!(
        "\nAll dumped layers match through floor_bits. If the streams still differ, look downstream (residue or coupling)."
    );
}
