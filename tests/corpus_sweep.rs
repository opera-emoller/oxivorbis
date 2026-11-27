//! Walk `<repo_root>/corpus/` and assert byte-identical parity against
//! libvorbis 1.3.7 for every audio file found. Skipped if the directory
//! doesn't exist (CI-friendly; contributors symlink their own corpus).
//!
//! Each input is decoded via ffmpeg to s16le at every requested
//! rate/channel config, encoded by both oxivorbis and the oracle-encoder at
//! every requested quality, and compared byte-for-byte.
//!
//! Environment:
//!
//! - `CORPUS_QUALITY=<q>[,<q>...]` libvorbis base_quality list (-0.1..1.0,
//!   default `0.5` = q5).
//! - `CORPUS_CONFIGS=<rate>x<ch>[,...]` or `all` (default `44100x2`).
//!   `all` = `44100x2,44100x1,48000x2,48000x1`. `ch` is 1..=255.
//!   6-channel configs synthesize 5.1 with distinct content per channel:
//!   FL/FR from the file, C its mid delayed 13 ms, BL/BR from the *next*
//!   file in the sorted list, LFE the mid low-passed at 120 Hz, in Vorbis
//!   order (FL, C, FR, BL, BR, LFE). Other channel counts use ffmpeg's
//!   `-ac` up/downmix.
//! - `CORPUS_JOBS=N` worker threads (default: available parallelism).
//! - `CORPUS_LIMIT=N` only the first N files (sorted).
//! - `CORPUS_BITRATE="<oracle flags>"` encode in a bitrate mode instead of
//!   VBR, given as oggenc-style oracle flags, e.g. `"--managed -b 128"`,
//!   `"--managed -b 128 -m 128 -M 128"`, `"-M 160"` or `"-q -M 128"`
//!   (quality mode at each `CORPUS_QUALITY`, with a hard maximum).
//!
//! Failing cases are saved to
//! `target/corpus-failures/<rate>x<ch>-q<q>/<relpath>/` as `input.s16le`,
//! `oracle.ogg`, `oxivorbis.ogg` and `descriptor.txt` (which includes a
//! `parity-fuzz --pcm` replay command).

#![cfg(feature = "oracle")]

use std::collections::BTreeMap;
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

const CORPUS_DIR: &str = "corpus";
const AUDIO_EXTENSIONS: &[&str] = &["wav", "mp3", "ogg", "flac", "m4a", "aif", "aiff"];
const ORACLE_BIN: &str = "./tools/oracle-encoder/oracle-encoder";
const DEFAULT_QUALITY: f32 = 0.5;
const ALL_CONFIGS: &[SweepConfig] = &[
    SweepConfig { rate: 44100, ch: 2 },
    SweepConfig { rate: 44100, ch: 1 },
    SweepConfig { rate: 48000, ch: 2 },
    SweepConfig { rate: 48000, ch: 1 },
];

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct SweepConfig {
    rate: u32,
    ch: u16,
}

impl SweepConfig {
    fn parse(s: &str) -> SweepConfig {
        let parsed = s.split_once(['x', 'X']).and_then(|(r, c)| {
            let rate: u32 = r.trim().parse().ok()?;
            let ch: u16 = c.trim().parse().ok()?;
            (oxivorbis::SampleRate::new(rate).is_some() && oxivorbis::Channels::new(ch).is_some())
                .then_some((rate, ch))
        });
        let Some((rate, ch)) = parsed else {
            panic!("CORPUS_CONFIGS entry {s:?} is not <rate 1..=200000>x<1..=255>");
        };
        SweepConfig { rate, ch }
    }

    /// Directory / table tag, e.g. `44100x2`.
    fn tag(&self) -> String {
        format!("{}x{}", self.rate, self.ch)
    }

    /// Human description used in the summary header, e.g. `stereo, 44.1k`.
    fn describe(&self) -> String {
        let ch = match self.ch {
            1 => "mono".to_string(),
            2 => "stereo".to_string(),
            6 => "5.1".to_string(),
            n => format!("{n} ch"),
        };
        let rate = if self.rate % 1000 == 0 {
            format!("{}k", self.rate / 1000)
        } else {
            format!("{}k", self.rate as f64 / 1000.0)
        };
        format!("{ch}, {rate}")
    }

    fn oxivorbis(&self) -> (oxivorbis::SampleRate, oxivorbis::Channels) {
        let r = oxivorbis::SampleRate::new(self.rate).expect("validated in parse");
        let c = oxivorbis::Channels::new(self.ch).expect("validated in parse");
        (r, c)
    }
}

fn parse_configs() -> Vec<SweepConfig> {
    let raw = std::env::var("CORPUS_CONFIGS").unwrap_or_default();
    let raw = raw.trim();
    if raw.is_empty() {
        return vec![ALL_CONFIGS[0]];
    }
    if raw.eq_ignore_ascii_case("all") {
        return ALL_CONFIGS.to_vec();
    }
    let mut out: Vec<SweepConfig> = Vec::new();
    for part in raw.split(',').filter(|p| !p.trim().is_empty()) {
        let c = SweepConfig::parse(part.trim());
        if !out.contains(&c) {
            out.push(c);
        }
    }
    assert!(!out.is_empty(), "CORPUS_CONFIGS={raw:?} lists no configs");
    out
}

fn parse_qualities() -> Vec<f32> {
    let raw = std::env::var("CORPUS_QUALITY").unwrap_or_default();
    let mut out: Vec<f32> = Vec::new();
    for part in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let q = part
            .parse::<f32>()
            .unwrap_or_else(|_| panic!("CORPUS_QUALITY entry {part:?} is not a float"));
        assert!(
            (-0.1..=1.0).contains(&q),
            "CORPUS_QUALITY {q} outside libvorbis range -0.1..1.0"
        );
        if !out.iter().any(|x| x.to_bits() == q.to_bits()) {
            out.push(q);
        }
    }
    if out.is_empty() {
        out.push(DEFAULT_QUALITY);
    }
    out
}

fn parse_jobs() -> usize {
    match std::env::var("CORPUS_JOBS") {
        Ok(s) if !s.trim().is_empty() => s
            .trim()
            .parse::<usize>()
            .unwrap_or_else(|_| panic!("CORPUS_JOBS={s:?} is not a positive integer"))
            .max(1),
        _ => std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4),
    }
}

fn failures_root() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"));
    // Absolute, so the replay command in descriptor.txt works from anywhere.
    std::env::current_dir()
        .unwrap()
        .join(target)
        .join("corpus-failures")
}

fn ffmpeg_decode(path: &Path, partner: &Path, rate: u32, ch: u16) -> Vec<i16> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-i",
        path.to_str().unwrap(),
    ]);
    if ch == 6 {
        // 5.1 with distinct channels (see the module docs). `apad` keeps
        // the partner from cutting the file short; amerge stops at the
        // file's own end.
        let graph = format!(
            "[0:a]aresample={rate},aformat=sample_fmts=fltp:channel_layouts=stereo,asplit=2[f][g];\
             [1:a]aresample={rate},aformat=sample_fmts=fltp:channel_layouts=stereo,apad[r];\
             [g]pan=mono|c0=0.5*c0+0.5*c1,asplit=2[c][l];\
             [c]adelay=13[cd];\
             [l]lowpass=f=120,lowpass=f=120[lfe];\
             [f][cd][r][lfe]amerge=inputs=4,pan=6c|c0=c0|c1=c2|c2=c1|c3=c3|c4=c4|c5=c5[out]"
        );
        cmd.args(["-i", partner.to_str().unwrap(), "-filter_complex", &graph]);
        cmd.args(["-map", "[out]", "-f", "s16le", "-"]);
    } else {
        cmd.args([
            "-f",
            "s16le",
            "-ac",
            &ch.to_string(),
            "-ar",
            &rate.to_string(),
            "-",
        ]);
    }
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("failed to invoke ffmpeg on {}: {e}", path.display()));
    out.stdout
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// `CORPUS_BITRATE`: the oracle flags and the matching oxivorbis mode.
#[derive(Clone)]
struct Bitrate {
    flags: Vec<String>,
    mode: oxivorbis::BitrateMode,
}

fn parse_bitrate() -> Option<Bitrate> {
    use oxivorbis::BitrateMode;
    let raw = std::env::var("CORPUS_BITRATE").ok()?;
    let flags: Vec<String> = raw.split_whitespace().map(str::to_string).collect();
    if flags.is_empty() {
        return None;
    }
    let (mut b, mut min, mut max) = (None, None, None);
    let (mut managed, mut q) = (false, false);
    let mut it = flags.iter();
    let kbps = |v: Option<&String>| -> u32 {
        v.and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("CORPUS_BITRATE={raw:?}: expected a kbps value"))
    };
    while let Some(t) = it.next() {
        match t.as_str() {
            "-b" => b = Some(kbps(it.next())),
            "-m" => min = Some(kbps(it.next())),
            "-M" => max = Some(kbps(it.next())),
            "--managed" => managed = true,
            "-q" => q = true,
            other => {
                panic!("CORPUS_BITRATE={raw:?}: unknown flag {other:?} (-b -m -M --managed -q)")
            }
        }
    }
    let mode = match (q, b, managed || min.is_some() || max.is_some()) {
        (true, _, _) => BitrateMode::QualityLimits {
            min_kbps: min,
            max_kbps: max,
        },
        (false, Some(kbps), false) => BitrateMode::Nominal { kbps },
        (false, Some(kbps), true) => BitrateMode::Average {
            kbps,
            min_kbps: min,
            max_kbps: max,
        },
        (false, None, _) => BitrateMode::Limits {
            min_kbps: min,
            max_kbps: max,
        },
    };
    Some(Bitrate { flags, mode })
}

/// Failure-directory suffix for a bitrate mode.
fn bitrate_tag(b: &Option<Bitrate>) -> String {
    match b {
        None => String::new(),
        Some(b) => format!("-br{}", b.flags.join("").replace('-', "_")),
    }
}

fn oracle_encode(raw: &[u8], rate: u32, ch: u16, base_quality: f32) -> Vec<u8> {
    oracle_encode_with(raw, rate, ch, base_quality, &[])
}

fn oracle_encode_with(
    raw: &[u8],
    rate: u32,
    ch: u16,
    base_quality: f32,
    flags: &[String],
) -> Vec<u8> {
    let mut child = Command::new(ORACLE_BIN)
        .args(flags)
        .args([
            &rate.to_string(),
            &ch.to_string(),
            &base_quality.to_string(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn {ORACLE_BIN}: {e}"));
    // Feed stdin from a separate thread so a large input can't deadlock
    // against the oracle filling its stdout pipe.
    let mut stdin = child.stdin.take().unwrap();
    std::thread::scope(|s| {
        s.spawn(move || {
            let _ = stdin.write_all(raw);
        });
        child.wait_with_output().unwrap().stdout
    })
}

/// Fail early (once) with a helpful message instead of once per file.
fn preflight() {
    if let Err(e) = Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        if e.kind() == std::io::ErrorKind::NotFound {
            panic!(
                "ffmpeg not on PATH — install it (e.g. `brew install ffmpeg` on macOS, \
                 `apt install ffmpeg` on Debian/Ubuntu) and re-run"
            );
        }
        panic!("failed to invoke ffmpeg: {e}");
    }
    if !Path::new(ORACLE_BIN).exists() {
        panic!("{ORACLE_BIN} not found — build it first with `./tools/oracle-encoder/build.sh`");
    }
}

struct Divergence {
    why: String,
    /// (input PCM, oracle output, oxivorbis output) when there is something
    /// to save.
    artifacts: Option<(Vec<u8>, Vec<u8>, Vec<u8>)>,
}

fn check_one(
    cfg: SweepConfig,
    samples: &[i16],
    raw: &[u8],
    q: f32,
    bitrate: &Option<Bitrate>,
) -> Result<(), Divergence> {
    if samples.is_empty() {
        return Err(Divergence {
            why: "empty samples".into(),
            artifacts: None,
        });
    }
    let oracle = match bitrate {
        None => oracle_encode(raw, cfg.rate, cfg.ch, q),
        Some(b) => oracle_encode_with(raw, cfg.rate, cfg.ch, q, &b.flags),
    };
    if oracle.len() < 18 {
        return Err(Divergence {
            why: format!("oracle too short: {} bytes", oracle.len()),
            artifacts: None,
        });
    }
    let serial = u32::from_le_bytes(oracle[14..18].try_into().unwrap());
    let vendor: &[u8] = b"Xiph.Org libVorbis I 20200704 (Reducing Environment)";
    let (rate, channels) = cfg.oxivorbis();
    let lw = match catch_unwind(AssertUnwindSafe(|| match bitrate {
        None => oxivorbis::encode_with_base_quality_and_meta(
            samples,
            rate,
            channels,
            q,
            serial,
            Some(vendor),
            Some(b""),
        ),
        Some(b) => oxivorbis::encode_bitrate_with_meta(
            samples,
            rate,
            channels,
            q,
            b.mode,
            oxivorbis::BitrateTuning::default(),
            serial,
            Some(vendor),
        )
        .unwrap_or_else(|e| panic!("oxivorbis rejected {:?}: {e}", b.mode)),
    })) {
        Ok(lw) => lw,
        Err(p) => {
            let msg = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "<non-string panic>".into());
            return Err(Divergence {
                why: format!("oxivorbis panicked: {msg}"),
                artifacts: Some((raw.to_vec(), oracle, Vec::new())),
            });
        }
    };
    if lw == oracle {
        return Ok(());
    }
    let first_diff = lw
        .iter()
        .zip(&oracle)
        .position(|(a, b)| a != b)
        .unwrap_or(lw.len().min(oracle.len()));
    Err(Divergence {
        why: format!(
            "lw={} or={} delta={} first_diff={}",
            lw.len(),
            oracle.len(),
            lw.len() as i64 - oracle.len() as i64,
            first_diff
        ),
        artifacts: Some((raw.to_vec(), oracle, lw)),
    })
}

fn save_failure(cfg: SweepConfig, q: f32, bitrate: &Option<Bitrate>, rel: &Path, d: &Divergence) {
    let br = bitrate_tag(bitrate);
    let mode = bitrate
        .as_ref()
        .map(|b| format!(" --mode \"{}\"", b.flags.join(" ")))
        .unwrap_or_default();
    let Some((pcm, oracle, lw)) = &d.artifacts else {
        return;
    };
    let dir = failures_root()
        .join(format!("{}-q{q}{br}", cfg.tag()))
        .join(rel);
    let res = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("input.s16le"), pcm)?;
        std::fs::write(dir.join("oracle.ogg"), oracle)?;
        std::fs::write(dir.join("oxivorbis.ogg"), lw)?;
        let input = dir.join("input.s16le");
        std::fs::write(
            dir.join("descriptor.txt"),
            format!(
                "source: corpus/{}\nrate: {} channels: {} quality: {q}\nframes: {}\n{}\n\n\
                 replay:\n  cargo run -p parity-fuzz --release -- --pcm {} --rates {} \
                 --channels {} --quality {q}{mode} --no-minimize\n",
                rel.display(),
                cfg.rate,
                cfg.ch,
                pcm.len() / 2 / cfg.ch as usize,
                d.why,
                input.display(),
                cfg.rate,
                cfg.ch,
            ),
        )
    })();
    if let Err(e) = res {
        eprintln!("warning: could not save failure to {}: {e}", dir.display());
    }
}

fn walk_audio_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn recurse(dir: &Path, out: &mut Vec<PathBuf>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                recurse(&path, out);
            } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if AUDIO_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(ext)) {
                    out.push(path);
                }
            }
        }
    }
    recurse(root, &mut out);
    out
}

/// First path component as "category", or "(root)" for files directly
/// under corpus/.
fn category(rel: &Path) -> String {
    if rel.components().count() > 1 {
        rel.components()
            .next()
            .unwrap()
            .as_os_str()
            .to_string_lossy()
            .into_owned()
    } else {
        "(root)".to_string()
    }
}

/// Per-quality outcomes of one (file, config) unit.
type UnitResult = Vec<Result<(), String>>;

#[derive(Default)]
struct ComboResult {
    total: usize,
    passed: usize,
    by_category: BTreeMap<String, (usize, usize)>,
    failures: Vec<(PathBuf, String)>,
}

/// Walk `<repo_root>/corpus/` recursively and assert parity for every audio
/// file found. Skips silently if the directory is absent (typical CI case).
///
/// See the module docs for the `CORPUS_*` environment variables. Failures
/// are reported per (config, quality) and per category, and the test panics
/// at the end with a summary if any case diverged.
/// The sweep has nothing to run. Say so loudly (the test still passes, so a
/// plain `--run-ignored all` works without a corpus), or fail when
/// `CORPUS_REQUIRE=1`.
fn not_run(why: &str) {
    let bar = "=".repeat(72);
    eprintln!(
        "\n{bar}\nWARNING: corpus_parity_sweep did NOT run: {why}.\n\
         NOTHING WAS TESTED. Symlink an audio corpus at <repo_root>/{CORPUS_DIR}\n\
         (see CONTRIBUTING.md). Set CORPUS_REQUIRE=1 to make this an error.\n{bar}\n"
    );
    if std::env::var_os("CORPUS_REQUIRE").is_some_and(|v| v != "0") {
        panic!("corpus_parity_sweep: {why} (CORPUS_REQUIRE is set)");
    }
}

#[test]
#[ignore = "manual sweep over <repo_root>/corpus/ (gitignored, large)"]
fn corpus_parity_sweep() {
    let qualities = parse_qualities();
    let configs = parse_configs();
    let jobs = parse_jobs();
    let bitrate = parse_bitrate();
    let br = bitrate_tag(&bitrate);

    let root = std::env::current_dir().unwrap().join(CORPUS_DIR);
    if !root.exists() {
        not_run(&format!("no {} directory", root.display()));
        return;
    }

    let limit = std::env::var("CORPUS_LIMIT")
        .ok()
        .and_then(|s| s.parse::<usize>().ok());

    let mut files = walk_audio_files(&root);
    files.sort();
    if let Some(n) = limit {
        files.truncate(n);
    }

    if files.is_empty() {
        not_run(&format!("no audio files under {}", root.display()));
        return;
    }

    preflight();

    // Clear stale failures for exactly the combos this run covers.
    for cfg in &configs {
        for &q in &qualities {
            let _ =
                std::fs::remove_dir_all(failures_root().join(format!("{}-q{q}{br}", cfg.tag())));
        }
    }

    // One work unit = one (file, config): a single ffmpeg decode feeds all
    // qualities.
    let units: Vec<(usize, usize)> = (0..files.len())
        .flat_map(|f| (0..configs.len()).map(move |c| (f, c)))
        .collect();
    let jobs = jobs.min(units.len());
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    // results[unit][quality]
    let results: Mutex<Vec<Option<UnitResult>>> =
        Mutex::new((0..units.len()).map(|_| None).collect());
    let start = Instant::now();
    let progress_step = (units.len() / 20).max(1);

    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= units.len() {
                        break;
                    }
                    let (fi, ci) = units[i];
                    let path = &files[fi];
                    let cfg = configs[ci];
                    let rel = path.strip_prefix(&root).unwrap();
                    let partner = &files[(fi + 1) % files.len()];
                    let samples = ffmpeg_decode(path, partner, cfg.rate, cfg.ch);
                    let raw: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
                    let per_q: UnitResult = qualities
                        .iter()
                        .map(|&q| {
                            check_one(cfg, &samples, &raw, q, &bitrate).map_err(|d| {
                                save_failure(cfg, q, &bitrate, rel, &d);
                                d.why
                            })
                        })
                        .collect();
                    results.lock().unwrap()[i] = Some(per_q);
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if n % progress_step == 0 || n == units.len() {
                        eprintln!(
                            "[corpus] {n}/{} (file, config) units, {:.0}s",
                            units.len(),
                            start.elapsed().as_secs_f64()
                        );
                    }
                }
            });
        }
    });
    let results = results.into_inner().unwrap();

    // Aggregate per (config, quality), preserving the requested order.
    let mut combos: Vec<ComboResult> = (0..configs.len() * qualities.len())
        .map(|_| ComboResult::default())
        .collect();
    for (i, per_q) in results.into_iter().enumerate() {
        let (fi, ci) = units[i];
        let rel = files[fi].strip_prefix(&root).unwrap();
        let cat = category(rel);
        let per_q = per_q.expect("worker finished without a result");
        for (qi, r) in per_q.into_iter().enumerate() {
            let c = &mut combos[ci * qualities.len() + qi];
            c.total += 1;
            let entry = c.by_category.entry(cat.clone()).or_default();
            match r {
                Ok(()) => {
                    c.passed += 1;
                    entry.0 += 1;
                }
                Err(why) => {
                    entry.1 += 1;
                    c.failures.push((rel.to_path_buf(), why));
                }
            }
        }
    }

    let mut any_fail = 0usize;
    let mut grand_total = 0usize;
    for (ci, cfg) in configs.iter().enumerate() {
        for (qi, q) in qualities.iter().enumerate() {
            let c = &combos[ci * qualities.len() + qi];
            eprintln!(
                "\n=== corpus sweep ({}, base_quality {q}{}) ===",
                cfg.describe(),
                bitrate
                    .as_ref()
                    .map(|b| format!(", oracle flags {}", b.flags.join(" ")))
                    .unwrap_or_default()
            );
            eprintln!("Overall: {}/{} byte-identical", c.passed, c.total);
            eprintln!("By category:");
            for (cat, &(pass, fail)) in &c.by_category {
                eprintln!("  {:30} {:5} pass, {:5} fail", cat, pass, fail);
            }
            if !c.failures.is_empty() {
                eprintln!("\nFailures (showing up to 50):");
                for (rel, why) in c.failures.iter().take(50) {
                    eprintln!("  {} — {}", rel.display(), why);
                }
                eprintln!(
                    "  (inputs + outputs saved under {}/{}-q{q}/)",
                    failures_root().display(),
                    cfg.tag()
                );
            }
            any_fail += c.failures.len();
            grand_total += c.total;
        }
    }

    let multi = combos.len() > 1;
    if multi {
        eprintln!("\n=== totals per (config, quality) ===");
        for (ci, cfg) in configs.iter().enumerate() {
            for (qi, q) in qualities.iter().enumerate() {
                let c = &combos[ci * qualities.len() + qi];
                eprintln!(
                    "  {:8} q{:<10} {:5} pass, {:5} fail",
                    cfg.tag(),
                    q,
                    c.passed,
                    c.total - c.passed
                );
            }
        }
        eprintln!(
            "  {:19} {:5} pass, {:5} fail",
            "all",
            grand_total - any_fail,
            any_fail
        );
    }
    eprintln!(
        "\n{} files x {} configs x {} qualities on {jobs} threads in {:.1}s",
        files.len(),
        configs.len(),
        qualities.len(),
        start.elapsed().as_secs_f64()
    );

    if any_fail > 0 {
        if multi {
            panic!(
                "{any_fail}/{grand_total} (file, config, quality) cases diverged \
                 (see stderr for details)"
            );
        }
        panic!(
            "{}/{} files diverged (see stderr for details)",
            any_fail, grand_total
        );
    }
}
