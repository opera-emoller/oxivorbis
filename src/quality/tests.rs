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
    writeln!(s, "psys {}", c.psys).unwrap();
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
        if b >= c.psys {
            writeln!(s, "p{b} missing").unwrap();
            continue;
        }
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

/// Rates for the oracle grid: a representative rate per template family,
/// every range edge with its neighbours, and odd rates.
#[cfg(feature = "oracle")]
const GRID_RATES: &[u32] = &[
    1, 2, 100, 1000, 4000, 7999, 8000, 8001, 8999, 9000, 9001, 11025, 12000, 14999, 15000, 15001,
    16000, 18999, 19000, 19001, 22050, 24000, 25999, 26000, 26001, 32000, 37800, 39999, 40000,
    40001, 44056, 44100, 48000, 49999, 50000, 50001, 64000, 69999, 70000, 70001, 88200, 96000,
    176400, 192000, 199999, 200000, 200001, 1_000_000,
];

fn base_setting_for(q: f32) -> Option<f64> {
    select_template(q, 44100, 2).map(|(_, s)| s)
}

#[test]
fn q5_template_and_id_header_values() {
    for (rate, ch) in COMBOS {
        let c = quality_config(0.5, rate, ch);
        assert_eq!(c.is, 6);
        assert_eq!(c.blocksizes, [256, 2048]);
        assert_eq!(c.bitrate_nominal, if ch == 2 { 160_000 } else { 96_000 });
        assert_eq!(
            t::SETUP_LIST[c.template].name,
            ["44_UNCOUPLED", "44_STEREO"][ch - 1]
        );
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

/// The public API promises every rate in `MIN_RATE..=MAX_RATE` at every
/// quality in `MIN_BASE_QUALITY..=1.0`, mono and stereo: every template's
/// quality mapping spans exactly that range, and the rate ranges of the
/// stereo and the uncoupled templates each cover 0..=MAX_RATE with no gap.
#[test]
fn templates_cover_every_rate_and_quality() {
    for tpl in t::SETUP_LIST {
        assert_eq!(
            tpl.quality_mapping[0] as f32, MIN_BASE_QUALITY,
            "{}",
            tpl.name
        );
        assert_eq!(tpl.quality_mapping[tpl.mappings], 1.0, "{}", tpl.name);
    }
    for ch in [1usize, 2, 3, 6, 7, 255] {
        let mut ranges: Vec<(i64, i64)> = t::SETUP_LIST
            .iter()
            .filter(|t| t.coupling_restriction == -1 || t.coupling_restriction as usize == ch)
            .map(|t| (t.samplerate_min_restriction, t.samplerate_max_restriction))
            .collect();
        ranges.sort();
        assert_eq!(ranges[0].0, 0);
        for w in ranges.windows(2) {
            assert!(
                w[1].0 <= w[0].1 + 1,
                "gap between {:?} and {:?}",
                w[0],
                w[1]
            );
        }
        assert_eq!(ranges.iter().map(|r| r.1).max(), Some(MAX_RATE as i64));
    }
    for rate in [MIN_RATE, 8000, 22050, 44100, MAX_RATE] {
        for ch in [1, 2, 6, MAX_CHANNELS] {
            for q in [MIN_BASE_QUALITY, 0.0, 0.5, 1.0] {
                assert!(select_template(q, rate, ch).is_some(), "{rate} {ch} {q}");
            }
        }
    }
    assert!(select_template(0.5, MAX_RATE + 1, 2).is_none());
}

/// The only requests where libvorbis indexes past its mode tables: base
/// quality `0x3f7ffffd` on the templates with a wide top interval (every
/// family below 26 kHz). Scans every f32 from 0.99 to 1.0 per family.
#[test]
fn libvorbis_oob_inputs_are_exactly_known() {
    let lo = 0.99_f32.to_bits();
    let hi = 1.0_f32.to_bits();
    for (rate, affected) in [
        (4000, true),
        (8000, true),
        (11025, true),
        (16000, true),
        (22050, true),
        (32000, false),
        (44100, false),
        (48000, false),
        (96000, false),
    ] {
        for ch in [1, 2, 6] {
            let oob: Vec<u32> = (lo..=hi)
                .filter(|&b| is_libvorbis_oob(f32::from_bits(b), rate, ch))
                .collect();
            let want: &[u32] = if affected { &[0x3f7f_fffd] } else { &[] };
            assert_eq!(oob, want, "{rate} Hz {ch} ch");
            if affected {
                // Encoded as libvorbis's own top clamp (q10).
                let a = quality_config(f32::from_bits(0x3f7f_fffd), rate, ch);
                let b = quality_config(1.0, rate, ch);
                assert_eq!(a.base_setting, b.base_setting);
            }
        }
    }
}

/// Template selection by rate, as `get_setup_template` scans
/// `setup_list` (first match wins at shared range edges).
#[test]
fn template_selection_edges() {
    let name = |rate: u32, ch: usize| t::SETUP_LIST[select_template(0.5, rate, ch).unwrap().0].name;
    let cases: &[(u32, &str)] = &[
        (1, "XX"),
        (7999, "XX"),
        (8000, "8"),
        (8999, "8"),
        (9000, "11"),
        (14999, "11"),
        (15000, "16"),
        (18999, "16"),
        (19000, "22"),
        (25999, "22"),
        (26000, "32"),
        (39999, "32"),
        (40000, "44"),
        (50000, "44"),
        (50001, "X"),
        (200000, "X"),
    ];
    for &(rate, fam) in cases {
        assert_eq!(name(rate, 2), format!("{fam}_STEREO"), "{rate}");
        assert_eq!(name(rate, 1), format!("{fam}_UNCOUPLED"), "{rate}");
        for ch in [3, 4, 5, 7, 8, MAX_CHANNELS] {
            assert_eq!(name(rate, ch), format!("{fam}_UNCOUPLED"), "{rate} {ch} ch");
        }
        // 6 channels: the 5.1 template at 40..=70 kHz (ahead of the
        // uncoupled 44 and X templates in setup_list), else uncoupled.
        let six = if (40_000..=70_000).contains(&rate) {
            "44_51".to_string()
        } else {
            format!("{fam}_UNCOUPLED")
        };
        assert_eq!(name(rate, 6), six, "{rate} 6 ch");
    }
    for (rate, want) in [
        (39_999, "32_UNCOUPLED"),
        (40_000, "44_51"),
        (60_000, "44_51"),
        (70_000, "44_51"),
        (70_001, "X_UNCOUPLED"),
    ] {
        assert_eq!(name(rate, 6), want, "{rate} 6 ch");
    }
    // The 5.1 template covers every quality (its mapping is the 44.1 kHz
    // one, -0.1..=1.0), so 6 channels never fall through to uncoupled.
    for i in -10..=100 {
        let q = i as f32 / 100.0;
        assert_eq!(
            t::SETUP_LIST[select_template(q, 48_000, 6).unwrap().0].name,
            "44_51"
        );
    }
}

/// `tables::quality::SETUP_LIST` and `setup_templates::SETUP_LIST` come
/// from two generators; they must describe the same templates in the same
/// order, since an index into one is used with the other.
#[test]
fn template_lists_agree() {
    let q = &t::SETUP_LIST;
    let s = &crate::setup_templates::SETUP_LIST;
    assert_eq!(q.len(), s.len());
    for (a, b) in q.iter().zip(s.iter()) {
        assert_eq!(a.mappings, b.mappings as usize, "{}", a.name);
        assert_eq!(a.quality_mapping, b.quality_mapping, "{}", a.name);
        assert_eq!(a.coupling_restriction, b.coupling_restriction, "{}", a.name);
        assert_eq!(a.samplerate_min_restriction, b.samplerate_min_restriction);
        assert_eq!(a.samplerate_max_restriction, b.samplerate_max_restriction);
        assert_eq!(
            &a.blocksize_short[..a.mappings],
            b.blocksize_short,
            "{}",
            a.name
        );
        assert_eq!(
            &a.blocksize_long[..a.mappings],
            b.blocksize_long,
            "{}",
            a.name
        );
        assert_eq!(a.psy_lowpass, b.psy_lowpass, "{}", a.name);
    }
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
    for tpl in t::SETUP_LIST {
        for &m in tpl.quality_mapping {
            centres.push(m as f32);
            centres.push((m - 0.0000001) as f32);
        }
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
    let all_qs = q_grid();
    let mut checked = 0usize;
    let mut skipped_oob = 0usize;
    let mut failures = Vec::new();
    // Mono, stereo, 5.1 (coupled at 40..=70 kHz, uncoupled elsewhere) and
    // uncoupled 3 and 255 channels.
    let combos = GRID_RATES
        .iter()
        .flat_map(|&r| [1usize, 2, 3, 6, 255].map(|ch| (r, ch)));
    for (rate, ch) in combos {
        // libvorbis reads out of bounds for these (undefined output; at
        // 1 Hz it can even crash dump_params); see `is_libvorbis_oob`.
        let qs: Vec<f32> = all_qs
            .iter()
            .copied()
            .filter(|&q| !is_libvorbis_oob(q, rate, ch))
            .collect();
        skipped_oob += all_qs.len() - qs.len();
        let out = Command::new(&bin)
            .arg(rate.to_string())
            .arg(ch.to_string())
            .args(qs.iter().map(|q| format!("{:08x}", q.to_bits())))
            .stderr(Stdio::null())
            .output()
            .expect("spawn dump_params");
        assert!(
            out.status.success(),
            "dump_params {rate} {ch}: {:?}",
            out.status
        );
        let text = String::from_utf8(out.stdout).unwrap();
        let blocks: Vec<&str> = text.split("end\n").filter(|b| !b.is_empty()).collect();
        assert_eq!(blocks.len(), qs.len());
        for (q, block) in qs.iter().zip(blocks) {
            let (head, body) = block.split_once('\n').unwrap();
            assert_eq!(head, format!("q {:08x}", q.to_bits()));
            let ours = match select_template(*q, rate, ch) {
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
    // Guard against a degenerate grid (CI also checks the count).
    assert!(checked > 30_000, "only {checked} configs checked");
    assert!(
        skipped_oob > 0,
        "the grid should reach the libvorbis OOB input"
    );
    eprintln!(
        "quality_oracle_grid: {checked} configs bit-identical \
         ({skipped_oob} skipped: libvorbis reads out of bounds)"
    );
}
