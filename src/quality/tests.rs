use super::*;

/// Render a config in the exact text format printed by
/// `tools/oracle-encoder/dump_params.c` (minus the leading `q` line).
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

// ---------------------------------------------------------------------------
// (i) q=0.5 vs the hand-written Q5 builders in encode.rs
// ---------------------------------------------------------------------------

/// Assert `a == b` bitwise for f32 slices, with a readable label.
fn eq_f32(label: &str, a: &[f32], b: &[f32]) {
    let ab: Vec<u32> = a.iter().map(|v| v.to_bits()).collect();
    let bb: Vec<u32> = b.iter().map(|v| v.to_bits()).collect();
    assert_eq!(ab, bb, "{label}: derived {a:?} != encode.rs {b:?}");
}

#[test]
fn q5_matches_encoder_builders() {
    for (rate, ch) in COMBOS {
        let c = quality_config(0.5, rate, ch);
        assert_eq!(c.is, 6);
        assert_eq!(c.blocksizes, [256, 2048]);
        assert_eq!(c.bitrate_nominal, if ch == 2 { 160_000 } else { 96_000 });

        let reference = q5_reference::q5_psy();
        for (b, (ours, theirs)) in c.psy.iter().zip(reference.iter()).enumerate() {
            let l = |f: &str| format!("rate={rate} ch={ch} psy[{b}].{f}");
            assert_eq!(ours.blockflag, theirs.blockflag, "{}", l("blockflag"));
            eq_f32(&l("ath_adjatt"), &[ours.ath_adjatt], &[theirs.ath_adjatt]);
            eq_f32(&l("ath_maxatt"), &[ours.ath_maxatt], &[theirs.ath_maxatt]);
            eq_f32(
                &l("tone_masteratt"),
                &ours.tone_masteratt,
                &theirs.tone_masteratt,
            );
            eq_f32(
                &l("tone_centerboost"),
                &[ours.tone_centerboost],
                &[theirs.tone_centerboost],
            );
            eq_f32(&l("tone_decay"), &[ours.tone_decay], &[theirs.tone_decay]);
            // KNOWN BUG in encode.rs: make_q5_psy_short / make_q5_psy_transition
            // use -30.0; libvorbis produces 0xc1f00005 for all four block types
            // (interp of _psy_tone_suppress[6..7] at ds~1e-6). Latent: parity
            // holds today because the tone peak limit rarely binds.
            if b == 1 || b == 2 {
                assert_eq!(theirs.tone_abs_limit, -30.0);
                assert_eq!(ours.tone_abs_limit.to_bits(), 0xc1f00005, "{}", l("tal"));
            } else {
                eq_f32(
                    &l("tone_abs_limit"),
                    &[ours.tone_abs_limit],
                    &[theirs.tone_abs_limit],
                );
            }
            eq_f32(&l("toneatt"), &ours.toneatt, &theirs.toneatt);
            assert_eq!(ours.noisemaskp, theirs.noisemaskp, "{}", l("noisemaskp"));
            eq_f32(
                &l("noisemaxsupp"),
                &[ours.noisemaxsupp],
                &[theirs.noisemaxsupp],
            );
            eq_f32(
                &l("noisewindowlo"),
                &[ours.noisewindowlo],
                &[theirs.noisewindowlo],
            );
            eq_f32(
                &l("noisewindowhi"),
                &[ours.noisewindowhi],
                &[theirs.noisewindowhi],
            );
            assert_eq!(ours.noisewindowlomin, theirs.noisewindowlomin);
            assert_eq!(ours.noisewindowhimin, theirs.noisewindowhimin);
            assert_eq!(ours.noisewindowfixed, theirs.noisewindowfixed);
            for j in 0..P_NOISECURVES {
                eq_f32(
                    &l(&format!("noiseoff[{j}]")),
                    &ours.noiseoff[j],
                    &theirs.noiseoff[j],
                );
            }
            eq_f32(&l("noisecompand"), &ours.noisecompand, &theirs.noisecompand);
            eq_f32(
                &l("max_curve_dB"),
                &[ours.max_curve_dB],
                &[theirs.max_curve_dB],
            );
            assert_eq!(ours.normal_p, theirs.normal_p);
            assert_eq!(ours.normal_start, theirs.normal_start);
            // KNOWN DIFF: encode.rs uses normal_partition=32 for short blocks;
            // libvorbis uses _noise_part_short_44[is]=8. Output-neutral at Q5
            // because normal_start=9999 disables noise normalization, but it
            // matters at q<=0.4 where normalization is active.
            if b < 2 {
                assert_eq!(theirs.normal_partition, 32);
                assert_eq!(ours.normal_partition, 8);
            } else {
                assert_eq!(ours.normal_partition, theirs.normal_partition);
            }
            assert_eq!(ours.normal_thresh.to_bits(), theirs.normal_thresh.to_bits());
        }

        let g = &c.psy_global;
        let r = q5_reference::q5_psy_global(rate as i64, ch);
        assert_eq!(g.eighth_octave_lines, r.eighth_octave_lines);
        eq_f32("preecho_thresh", &g.preecho_thresh, &r.preecho_thresh);
        // KNOWN DIFF: bands 2,3 of postecho_thresh are -13.5 in encode.rs but
        // libvorbis gives 0xc157ffff (-13.499999) from the interpolation.
        let mut post = r.postecho_thresh;
        post[2] = f32::from_bits(0xc157ffff);
        post[3] = f32::from_bits(0xc157ffff);
        eq_f32("postecho_thresh", &g.postecho_thresh, &post);
        eq_f32(
            "stretch_penalty",
            &[g.stretch_penalty],
            &[r.stretch_penalty],
        );
        eq_f32(
            "preecho_minenergy",
            &[g.preecho_minenergy],
            &[r.preecho_minenergy],
        );
        eq_f32(
            "ampmax_att_per_sec",
            &[g.ampmax_att_per_sec],
            &[r.ampmax_att_per_sec],
        );
        assert_eq!(g.coupling_prepointamp, r.coupling_prepointamp);
        assert_eq!(g.coupling_postpointamp, r.coupling_postpointamp);

        // KNOWN DIFFS (output-neutral today), libvorbis vs encode.rs:
        // * libvorbis fills coupling_* and sliding_lowpass for mono too (the
        //   uncoupled template still has stereo_modes); encode.rs puts n/2
        //   placeholders there. Mono never reads them.
        // * sliding_lowpass is unclamped in libvorbis (e.g. 4597 for 99 kHz
        //   at 44.1k); encode.rs clamps to n/2 (1024 / 128). Consumers only
        //   compare `j < sliding_lowpass - i` with j < n/2, so identical.
        // * coupling_pointlimit[1] is clamped to n/2 in encode.rs (no-op at
        //   12 kHz) and [0] is computed as limit/8 rather than from
        //   blocksizes[0] (equal at Q5: 69 both ways).
        let khz12 = |bs: f64| (12.0_f64 * 1000. / rate as f64 * bs) as i32;
        let lp99 = |bs: f64| (99.0_f64 * 1000. / rate as f64 * bs) as i32;
        assert_eq!(g.coupling_pkHz, [12; PACKETBLOBS]);
        assert_eq!(g.coupling_pointlimit[0], [khz12(256.); PACKETBLOBS]);
        assert_eq!(g.coupling_pointlimit[1], [khz12(2048.); PACKETBLOBS]);
        assert_eq!(g.sliding_lowpass[0], [lp99(256.); PACKETBLOBS]);
        assert_eq!(g.sliding_lowpass[1], [lp99(2048.); PACKETBLOBS]);
        if ch == 2 {
            assert_eq!(g.coupling_pkHz, r.coupling_pkHz);
            assert_eq!(g.coupling_pointlimit, r.coupling_pointlimit);
            for k in 0..2 {
                for i in 0..PACKETBLOBS {
                    let n2 = if k == 0 { 128 } else { 1024 };
                    assert_eq!(g.sliding_lowpass[k][i].min(n2), r.sliding_lowpass[k][i]);
                }
            }
        }
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
