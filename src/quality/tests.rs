use super::*;

/// Render a config in the exact text format printed by
/// `tools/oracle-encoder/dump_params.c` (minus the leading `q` line).
#[cfg(feature = "oracle")]
fn oracle_text(c: &QualityConfig) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let fa = |s: &mut String, name: &str, a: &[f32]| {
        s.push_str(name);
        for v in a {
            write!(s, " {:08x}", v.to_bits()).unwrap();
        }
        s.push('\n');
    };
    let ia = |s: &mut String, name: &str, a: &[i32]| {
        s.push_str(name);
        for v in a {
            write!(s, " {v}").unwrap();
        }
        s.push('\n');
    };
    writeln!(s, "base_setting {:016x}", c.base_setting.to_bits()).unwrap();
    writeln!(s, "lowpass_khz {:016x}", c.lowpass_khz.to_bits()).unwrap();
    writeln!(s, "blocksizes {} {}", c.blocksizes[0], c.blocksizes[1]).unwrap();
    writeln!(s, "bitrate 0 {} 0", c.bitrate_nominal).unwrap();
    writeln!(s, "psys 4").unwrap();
    let g = &c.psy_global;
    writeln!(s, "g.eighth_octave_lines {}", g.eighth_octave_lines).unwrap();
    fa(&mut s, "g.preecho_thresh", &g.preecho_thresh);
    fa(&mut s, "g.postecho_thresh", &g.postecho_thresh);
    writeln!(s, "g.stretch_penalty {:08x}", g.stretch_penalty.to_bits()).unwrap();
    writeln!(
        s,
        "g.preecho_minenergy {:08x}",
        g.preecho_minenergy.to_bits()
    )
    .unwrap();
    writeln!(
        s,
        "g.ampmax_att_per_sec {:08x}",
        g.ampmax_att_per_sec.to_bits()
    )
    .unwrap();
    ia(&mut s, "g.coupling_pkHz", &g.coupling_pkHz);
    ia(&mut s, "g.coupling_pointlimit0", &g.coupling_pointlimit[0]);
    ia(&mut s, "g.coupling_pointlimit1", &g.coupling_pointlimit[1]);
    ia(&mut s, "g.coupling_prepointamp", &g.coupling_prepointamp);
    ia(&mut s, "g.coupling_postpointamp", &g.coupling_postpointamp);
    ia(&mut s, "g.sliding_lowpass0", &g.sliding_lowpass[0]);
    ia(&mut s, "g.sliding_lowpass1", &g.sliding_lowpass[1]);
    for (b, p) in c.psy.iter().enumerate() {
        let f = |s: &mut String, n: &str, v: f32| {
            writeln!(s, "p{b}.{n} {:08x}", v.to_bits()).unwrap();
        };
        let i = |s: &mut String, n: &str, v: i32| {
            writeln!(s, "p{b}.{n} {v}").unwrap();
        };
        i(&mut s, "blockflag", p.blockflag);
        f(&mut s, "ath_adjatt", p.ath_adjatt);
        f(&mut s, "ath_maxatt", p.ath_maxatt);
        fa(&mut s, &format!("p{b}.tone_masteratt"), &p.tone_masteratt);
        f(&mut s, "tone_centerboost", p.tone_centerboost);
        f(&mut s, "tone_decay", p.tone_decay);
        f(&mut s, "tone_abs_limit", p.tone_abs_limit);
        fa(&mut s, &format!("p{b}.toneatt"), &p.toneatt);
        i(&mut s, "noisemaskp", p.noisemaskp);
        f(&mut s, "noisemaxsupp", p.noisemaxsupp);
        f(&mut s, "noisewindowlo", p.noisewindowlo);
        f(&mut s, "noisewindowhi", p.noisewindowhi);
        i(&mut s, "noisewindowlomin", p.noisewindowlomin);
        i(&mut s, "noisewindowhimin", p.noisewindowhimin);
        i(&mut s, "noisewindowfixed", p.noisewindowfixed);
        for j in 0..P_NOISECURVES {
            fa(&mut s, &format!("p{b}.noiseoff{j}"), &p.noiseoff[j]);
        }
        fa(&mut s, &format!("p{b}.noisecompand"), &p.noisecompand);
        f(&mut s, "max_curve_dB", p.max_curve_dB);
        i(&mut s, "normal_p", p.normal_p);
        i(&mut s, "normal_start", p.normal_start);
        i(&mut s, "normal_partition", p.normal_partition);
        writeln!(s, "p{b}.normal_thresh {:016x}", p.normal_thresh.to_bits()).unwrap();
    }
    s
}

const COMBOS: [(u32, usize); 4] = [(44100, 1), (44100, 2), (48000, 1), (48000, 2)];

#[test]
fn q5_template_and_id_header_values() {
    for (rate, ch) in COMBOS {
        let c = quality_config(0.5, rate, ch);
        assert_eq!(c.is, 6);
        assert_eq!(c.blocksizes, [256, 2048]);
        assert_eq!(c.bitrate_nominal, if ch == 2 { 160_000 } else { 96_000 });
    }
}

#[test]
fn base_setting_domain() {
    assert!(base_setting_for(-0.1).is_some());
    assert!(base_setting_for(-0.2).is_none());
    assert!(base_setting_for(f32::NAN).is_none());
    // >= 1.0 clamps to .9999
    assert_eq!(base_setting_for(1.0), base_setting_for(5.0));
    let q10 = quality_config(1.0, 44100, 2);
    assert_eq!(q10.is, 10);
    let qm1 = quality_config(-0.1, 44100, 2);
    assert_eq!(qm1.is, 0);
    assert_eq!(qm1.blocksizes, [512, 4096]);
}

// ---------------------------------------------------------------------------
// (ii) oracle grid vs tools/oracle-encoder/dump_params
// ---------------------------------------------------------------------------

#[cfg(feature = "oracle")]
fn next_up(x: f32) -> f32 {
    let b = x.to_bits();
    f32::from_bits(if x >= 0.0 {
        if x == 0.0 { 1 } else { b + 1 }
    } else {
        b - 1
    })
}
#[cfg(feature = "oracle")]
fn next_down(x: f32) -> f32 {
    -next_up(-x)
}

/// Dense q grid: 0.01 steps, every mapping boundary (and its libvorbis
/// `-1e-7` shifted twin) with ±1..2 ulp neighbours, clamp edges, out-of-range
/// values, plus 50 seeded random floats.
#[cfg(feature = "oracle")]
fn q_grid() -> Vec<f32> {
    let mut qs = Vec::new();
    for i in 0..=110 {
        qs.push((i as f64 * 0.01 - 0.1) as f32);
    }
    let mut centres: Vec<f32> = Vec::new();
    for &m in &t::QUALITY_MAPPING_44 {
        centres.push(m as f32);
        centres.push((m - 0.0000001) as f32);
    }
    centres.extend([0.9999_f32, (0.9999_f64 - 0.0000001) as f32]);
    for c in centres {
        let (mut up, mut dn) = (c, c);
        qs.push(c);
        for _ in 0..2 {
            up = next_up(up);
            dn = next_down(dn);
            qs.push(up);
            qs.push(dn);
        }
    }
    qs.extend([1.5, 10.0, -0.2, -0.1000002, 0.05, 0.123_456_7]);
    // xorshift32, fixed seed
    let mut x: u32 = 0x9e37_79b9;
    for _ in 0..50 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        let u = (x >> 8) as f64 / (1u32 << 24) as f64;
        qs.push((-0.1 + 1.1 * u) as f32);
    }
    qs
}

#[cfg(feature = "oracle")]
#[test]
fn quality_oracle_grid() {
    use std::process::{Command, Stdio};
    let bin =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/oracle-encoder/dump_params");
    assert!(
        bin.exists(),
        "{} missing; run ./tools/oracle-encoder/build_quality_dumpers.sh",
        bin.display()
    );
    let qs = q_grid();
    let mut checked = 0usize;
    let mut failures = Vec::new();
    for (rate, ch) in COMBOS {
        let out = Command::new(&bin)
            .arg(rate.to_string())
            .arg(ch.to_string())
            .args(qs.iter().map(|q| format!("{:08x}", q.to_bits())))
            .stderr(Stdio::null())
            .output()
            .expect("spawn dump_params");
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        let blocks: Vec<&str> = text.split("end\n").filter(|b| !b.is_empty()).collect();
        assert_eq!(blocks.len(), qs.len());
        for (q, block) in qs.iter().zip(blocks) {
            let (head, body) = block.split_once('\n').unwrap();
            assert_eq!(head, format!("q {:08x}", q.to_bits()));
            let ours = match base_setting_for(*q) {
                None => "error\n".to_string(),
                Some(_) => oracle_text(&quality_config(*q, rate, ch)),
            };
            checked += 1;
            if ours != body {
                let diff: Vec<String> = ours
                    .lines()
                    .zip(body.lines())
                    .filter(|(a, b)| a != b)
                    .take(4)
                    .map(|(a, b)| format!("    ours:   {a}\n    oracle: {b}"))
                    .collect();
                failures.push(format!(
                    "rate={rate} ch={ch} q={q:e} ({:08x}):\n{}",
                    q.to_bits(),
                    diff.join("\n")
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {checked} configs differ:\n{}",
        failures.len(),
        failures[..failures.len().min(10)].join("\n")
    );
    eprintln!("quality_oracle_grid: {checked} configs bit-identical");
}
