//! parity-fuzz: differential fuzzer for lewtoff vs the in-tree libvorbis
//! oracle (tools/oracle-encoder). Generates seeded synthetic signals, encodes
//! with both, and on any byte divergence (or lewtoff panic) saves the case
//! and a minimized reproduction under target/parity-fuzz/<id>/.
//!
//! Usage:
//!   cargo run -p parity-fuzz --release -- --iters N --seed S [--quality Q]
//!       [--rates 44100,48000] [--channels 1,2] [--jobs J] [--max-secs T]
//!       [--max-len SECS] [--keep-going] [--no-minimize] [--start I]
//!       [--min-frames N] [--minimize-per-sig N] [--min-evals N]
//!
//!   --keep-going       collect all failures instead of stopping at the first
//!   --min-frames N     skip generated cases shorter than N frames (e.g. to
//!                      look past a known short-stream bug)
//!   --minimize-per-sig only minimize the first N failures of each signature
//!                      (class / divergent-packet position / length class)
//!       [--out DIR] [--oracle PATH]
//!   Replay a saved raw file (one rate / channel count):
//!   cargo run -p parity-fuzz --release -- --pcm input.s16le --rates 44100 --channels 2

mod generate;
mod oggdiff;

use generate::{Case, Gen, Stereo, make_case, mix_seed};
use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// lewtoff adapter: the ONLY place lewtoff is called. `quality` is a libvorbis
// base_quality (the oracle's 3rd argument), -0.1..=1.0.
// ---------------------------------------------------------------------------

fn quality_supported(q: f32) -> bool {
    (-0.1..=1.0).contains(&q)
}

fn lewtoff_encode(
    pcm: &[i16],
    rate: u32,
    channels: u16,
    quality: f32,
    serial: u32,
    vendor: &[u8],
    tag: &[u8],
) -> Vec<u8> {
    debug_assert!(quality_supported(quality));
    let r = match rate {
        44100 => lewtoff::SampleRate::Hz44100,
        48000 => lewtoff::SampleRate::Hz48000,
        _ => unreachable!("rate validated at CLI"),
    };
    let c = match channels {
        1 => lewtoff::Channels::Mono,
        2 => lewtoff::Channels::Stereo,
        _ => unreachable!("channels validated at CLI"),
    };
    lewtoff::encode_with_base_quality_and_meta(pcm, r, c, quality, serial, Some(vendor), Some(tag))
}

// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Config {
    iters: usize,
    start: usize,
    seed: u64,
    quality: f32,
    combos: Vec<(u32, u16)>,
    jobs: usize,
    max_secs: Option<f64>,
    max_len: f64,
    keep_going: bool,
    minimize: bool,
    out: PathBuf,
    oracle: PathBuf,
    pcm: Option<PathBuf>,
    min_evals: usize,
    min_frames: usize,
    min_per_sig: usize,
}

fn usage() -> ! {
    eprintln!(
        "usage: parity-fuzz [--iters N] [--seed S] [--start I] [--quality Q] \
         [--rates 44100,48000] [--channels 1,2] [--jobs J] [--max-secs T] [--max-len SECS] \
         [--keep-going] [--no-minimize] [--min-evals N] [--min-frames N] [--minimize-per-sig N] [--out DIR] [--oracle PATH] [--pcm FILE]"
    );
    std::process::exit(2)
}

fn parse_args() -> Config {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root = root.canonicalize().unwrap_or(root);
    let mut cfg = Config {
        iters: 1000,
        start: 0,
        seed: 1,
        quality: 0.5,
        combos: vec![],
        jobs: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4),
        max_secs: None,
        max_len: 3.0,
        keep_going: false,
        minimize: true,
        out: root.join("target/parity-fuzz"),
        oracle: root.join("tools/oracle-encoder/oracle-encoder"),
        pcm: None,
        min_evals: 300,
        min_frames: 0,
        min_per_sig: 2,
    };
    let mut rates = vec![44100u32, 48000];
    let mut chans = vec![1u16, 2];
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| usage());
        match a.as_str() {
            "--iters" => cfg.iters = val().parse().unwrap_or_else(|_| usage()),
            "--start" => cfg.start = val().parse().unwrap_or_else(|_| usage()),
            "--seed" => cfg.seed = val().parse().unwrap_or_else(|_| usage()),
            "--quality" => cfg.quality = val().parse().unwrap_or_else(|_| usage()),
            "--jobs" => cfg.jobs = val().parse().unwrap_or_else(|_| usage()),
            "--max-secs" => cfg.max_secs = Some(val().parse().unwrap_or_else(|_| usage())),
            "--max-len" => cfg.max_len = val().parse().unwrap_or_else(|_| usage()),
            "--min-evals" => cfg.min_evals = val().parse().unwrap_or_else(|_| usage()),
            "--min-frames" => cfg.min_frames = val().parse().unwrap_or_else(|_| usage()),
            "--minimize-per-sig" => cfg.min_per_sig = val().parse().unwrap_or_else(|_| usage()),
            "--out" => cfg.out = PathBuf::from(val()),
            "--oracle" => cfg.oracle = PathBuf::from(val()),
            "--pcm" => cfg.pcm = Some(PathBuf::from(val())),
            "--keep-going" => cfg.keep_going = true,
            "--no-minimize" => cfg.minimize = false,
            "--rates" => {
                rates = val()
                    .split(',')
                    .map(|s| s.trim().parse().unwrap_or_else(|_| usage()))
                    .collect()
            }
            "--channels" => {
                chans = val()
                    .split(',')
                    .map(|s| s.trim().parse().unwrap_or_else(|_| usage()))
                    .collect()
            }
            "-h" | "--help" => usage(),
            _ => {
                eprintln!("unknown argument {a}");
                usage()
            }
        }
    }
    if !quality_supported(cfg.quality) {
        eprintln!(
            "parity-fuzz: --quality {} outside libvorbis base_quality -0.1..=1.0",
            cfg.quality
        );
        std::process::exit(2);
    }
    for &r in &rates {
        if r != 44100 && r != 48000 {
            eprintln!("parity-fuzz: unsupported rate {r} (44100 or 48000)");
            std::process::exit(2);
        }
    }
    for &c in &chans {
        if c != 1 && c != 2 {
            eprintln!("parity-fuzz: unsupported channel count {c} (1 or 2)");
            std::process::exit(2);
        }
    }
    for &r in &rates {
        for &c in &chans {
            cfg.combos.push((r, c));
        }
    }
    if !cfg.oracle.exists() {
        eprintln!(
            "parity-fuzz: oracle not found at {} (run tools/oracle-encoder/build.sh)",
            cfg.oracle.display()
        );
        std::process::exit(2);
    }
    cfg.jobs = cfg.jobs.max(1);
    cfg
}

// ---------------------------------------------------------------------------
// Running one case
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    Mismatch,
    LewtoffPanic,
    OracleFail,
}

impl Class {
    fn name(self) -> &'static str {
        match self {
            Class::Mismatch => "mismatch",
            Class::LewtoffPanic => "lewtoff-panic",
            Class::OracleFail => "oracle-fail",
        }
    }
}

struct Outcome {
    class: Option<Class>,
    oracle: Vec<u8>,
    lewtoff: Vec<u8>,
    message: String,
    /// Coarse bucket for de-duplicating failures, e.g. "mismatch/final/long".
    signature: String,
}

thread_local! {
    static PANIC_MSG: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let payload = info.payload();
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic>".into());
        let loc = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        PANIC_MSG.with(|m| *m.borrow_mut() = Some(format!("{msg} at {loc}")));
    }));
}

fn run_oracle(oracle: &Path, pcm: &[i16], rate: u32, ch: u16, q: f32) -> Result<Vec<u8>, String> {
    let raw: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut child = Command::new(oracle)
        .arg(rate.to_string())
        .arg(ch.to_string())
        .arg(q.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn oracle: {e}"))?;
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(&raw));
    let out = child
        .wait_with_output()
        .map_err(|e| format!("oracle wait: {e}"))?;
    let _ = writer.join();
    if !out.status.success() {
        return Err(format!(
            "oracle exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

fn run_case(cfg: &Config, pcm: &[i16], rate: u32, ch: u16) -> Outcome {
    let oracle = match run_oracle(&cfg.oracle, pcm, rate, ch, cfg.quality) {
        Ok(o) => o,
        Err(e) => {
            // Still see whether lewtoff survives the same input.
            let lw = std::panic::catch_unwind(|| {
                lewtoff_encode(pcm, rate, ch, cfg.quality, 0, b"", b"")
            });
            let extra = if lw.is_err() {
                format!(
                    "; lewtoff also panicked: {}",
                    PANIC_MSG
                        .with(|m| m.borrow_mut().take())
                        .unwrap_or_default()
                )
            } else {
                String::new()
            };
            return Outcome {
                class: Some(Class::OracleFail),
                oracle: vec![],
                lewtoff: lw.unwrap_or_default(),
                message: format!("{e}{extra}"),
                signature: "oracle-fail".into(),
            };
        }
    };
    let serial = oggdiff::extract_serial(&oracle).unwrap_or(0);
    let (vendor, tag) = oggdiff::extract_comment_strings(&oracle).unwrap_or_default();
    let lw = std::panic::catch_unwind(|| {
        lewtoff_encode(pcm, rate, ch, cfg.quality, serial, &vendor, &tag)
    });
    let frames = pcm.len() / ch as usize;
    // libvorbis' EOS buffer still holds pre-extrapolated samples for streams
    // shorter than ~3072 frames, which is its own distinct regime.
    let len_class = match frames {
        0 => "empty",
        1..=3071 => "short",
        _ => "long",
    };
    match lw {
        Err(_) => {
            let message = PANIC_MSG
                .with(|m| m.borrow_mut().take())
                .unwrap_or_else(|| "<unknown panic>".into());
            Outcome {
                class: Some(Class::LewtoffPanic),
                oracle,
                lewtoff: vec![],
                signature: format!(
                    "panic/{}",
                    message.rsplit(" at ").next().unwrap_or_default()
                ),
                message,
            }
        }
        Ok(lw) if lw != oracle => {
            let rep = oggdiff::analyze(&lw, &oracle);
            Outcome {
                class: Some(Class::Mismatch),
                message: format!(
                    "first divergent byte {} (lewtoff len {}, oracle len {})\n{}\n{}",
                    rep.first_byte, rep.lewtoff_len, rep.oracle_len, rep.location, rep.summary
                ),
                oracle,
                lewtoff: lw,
                signature: format!("mismatch/{}/{len_class}", rep.position),
            }
        }
        Ok(lw) => Outcome {
            class: None,
            oracle,
            lewtoff: lw,
            message: String::new(),
            signature: String::new(),
        },
    }
}

// ---------------------------------------------------------------------------
// Minimization
// ---------------------------------------------------------------------------

struct Minimizer<'a> {
    cfg: &'a Config,
    rate: u32,
    ch: u16,
    class: Class,
    /// Candidates must reproduce the same signature, so minimization doesn't
    /// drift from one bug into another (e.g. trimming a long stream into the
    /// short-stream regime).
    signature: String,
    evals: usize,
    deadline: Instant,
    log: Vec<String>,
}

impl Minimizer<'_> {
    fn budget_left(&self) -> bool {
        self.evals < self.cfg.min_evals && Instant::now() < self.deadline
    }
    fn fails(&mut self, pcm: &[i16]) -> bool {
        self.evals += 1;
        let o = run_case(self.cfg, pcm, self.rate, self.ch);
        o.class == Some(self.class) && o.signature == self.signature
    }

    /// Segment-level simplification: drop segments, replace with silence,
    /// strip gain/dc/stereo decorations.
    fn segments(&mut self, case: &mut Case) {
        let mut i = 0;
        while i < case.segments.len() && self.budget_left() {
            if case.segments.len() > 1 {
                let mut c = case.clone();
                c.segments.remove(i);
                if self.fails(&c.render()) {
                    self.log.push(format!("dropped segment {i}"));
                    *case = c;
                    continue;
                }
            }
            let seg = &case.segments[i];
            let mut variants: Vec<(&str, Case)> = Vec::new();
            if !matches!(seg.generator, Gen::Silence) {
                let mut c = case.clone();
                c.segments[i].generator = Gen::Silence;
                variants.push(("silenced", c));
            }
            if seg.gain != 1.0 {
                let mut c = case.clone();
                c.segments[i].gain = 1.0;
                variants.push(("gain=1", c));
            }
            if seg.dc != 0.0 {
                let mut c = case.clone();
                c.segments[i].dc = 0.0;
                variants.push(("dc=0", c));
            }
            if case.channels == 2 && !matches!(seg.stereo, Stereo::Identical) {
                let mut c = case.clone();
                c.segments[i].stereo = Stereo::Identical;
                variants.push(("stereo=identical", c));
            }
            for (what, c) in variants {
                if !self.budget_left() {
                    break;
                }
                if self.fails(&c.render()) {
                    self.log.push(format!("segment {i}: {what}"));
                    *case = c;
                }
            }
            i += 1;
        }
    }

    /// PCM-level: trim from the end, then from the start, then zero windows.
    /// Returns (pcm, frames trimmed from start).
    fn pcm(&mut self, mut pcm: Vec<i16>) -> (Vec<i16>, usize) {
        let ch = self.ch as usize;
        let mut trimmed_start = 0usize;
        for from_start in [false, true] {
            let mut chunk = pcm.len() / ch / 2;
            while chunk >= 1 && self.budget_left() {
                let frames = pcm.len() / ch;
                if chunk > frames {
                    chunk = frames / 2;
                    continue;
                }
                let cand: Vec<i16> = if from_start {
                    pcm[chunk * ch..].to_vec()
                } else {
                    pcm[..(frames - chunk) * ch].to_vec()
                };
                if self.fails(&cand) {
                    if from_start {
                        trimmed_start += chunk;
                    }
                    pcm = cand;
                } else {
                    chunk /= 2;
                }
            }
            self.log.push(format!(
                "after trim-{}: {} frames",
                if from_start { "start" } else { "end" },
                pcm.len() / ch
            ));
        }
        // Zero out windows (simplify content while preserving length).
        let mut w = (pcm.len() / ch / 2).max(1);
        while w >= 32 && self.budget_left() {
            let frames = pcm.len() / ch;
            let mut s = 0;
            while s < frames && self.budget_left() {
                let e = (s + w).min(frames);
                if pcm[s * ch..e * ch].iter().any(|&x| x != 0) {
                    let mut cand = pcm.clone();
                    cand[s * ch..e * ch].iter_mut().for_each(|x| *x = 0);
                    if self.fails(&cand) {
                        pcm = cand;
                    }
                }
                s += w;
            }
            w /= 2;
        }
        let nz = pcm.iter().filter(|&&x| x != 0).count();
        self.log
            .push(format!("after zeroing: {nz} nonzero samples remain"));
        (pcm, trimmed_start)
    }
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

struct Meta<'a> {
    id: &'a str,
    seed: u64,
    iter: Option<usize>,
    case_seed: Option<u64>,
    rate: u32,
    ch: u16,
    quality: f32,
    case: Option<&'a Case>,
    note: &'a str,
}

fn save(dir: &Path, m: &Meta, pcm: &[i16], o: &Outcome) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let raw: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
    std::fs::write(dir.join("input.s16le"), raw)?;
    std::fs::write(dir.join("oracle.ogg"), &o.oracle)?;
    std::fs::write(dir.join("lewtoff.ogg"), &o.lewtoff)?;
    let frames = pcm.len() / m.ch as usize;
    let case_dbg = m.case.map(|c| format!("{c:#?}")).unwrap_or_default();
    let (fb, fp, blk) = if o.class == Some(Class::Mismatch) {
        let r = oggdiff::analyze(&o.lewtoff, &o.oracle);
        (Some(r.first_byte), r.first_packet, r.block_index)
    } else {
        (None, None, None)
    };
    let opt = |v: Option<usize>| v.map(|x| x.to_string()).unwrap_or("null".into());
    let json = format!(
        "{{\n  \"id\": {},\n  \"class\": {},\n  \"seed\": {},\n  \"iter\": {},\n  \"case_seed\": {},\n  \
         \"rate\": {},\n  \"channels\": {},\n  \"quality\": {},\n  \"frames\": {},\n  \
         \"first_divergent_byte\": {},\n  \"first_divergent_packet\": {},\n  \"block_index\": {},\n  \
         \"note\": {},\n  \"message\": {},\n  \"case\": {},\n  \
         \"replay\": {}\n}}\n",
        esc(m.id),
        esc(o.class.map(|c| c.name()).unwrap_or("none")),
        m.seed,
        opt(m.iter),
        m.case_seed.map(|x| x.to_string()).unwrap_or("null".into()),
        m.rate,
        m.ch,
        m.quality,
        frames,
        opt(fb),
        opt(fp),
        opt(blk),
        esc(m.note),
        esc(&o.message),
        esc(&case_dbg),
        esc(&format!(
            "cargo run -p parity-fuzz --release -- --pcm {} --rates {} --channels {} --no-minimize",
            dir.join("input.s16le").display(),
            m.rate,
            m.ch
        )),
    );
    std::fs::write(dir.join("descriptor.json"), json)?;
    let txt = format!(
        "id: {}\nclass: {}\nrate: {} channels: {} quality: {} frames: {}\nseed: {} iter: {:?} case_seed: {:?}\nnote: {}\n\n{}\n\ncase:\n{}\n",
        m.id,
        o.class.map(|c| c.name()).unwrap_or("none"),
        m.rate,
        m.ch,
        m.quality,
        frames,
        m.seed,
        m.iter,
        m.case_seed,
        m.note,
        o.message,
        case_dbg
    );
    std::fs::write(dir.join("descriptor.txt"), txt)
}

/// Save the original failure, minimize it, save the minimized case.
/// Returns a one-paragraph summary.
#[allow(clippy::too_many_arguments)]
fn handle_failure(
    cfg: &Config,
    id: &str,
    iter: Option<usize>,
    case_seed: Option<u64>,
    case: Option<&Case>,
    pcm: &[i16],
    rate: u32,
    ch: u16,
    o: &Outcome,
    minimize: bool,
) -> String {
    let dir = cfg.out.join(id);
    let meta = Meta {
        id,
        seed: cfg.seed,
        iter,
        case_seed,
        rate,
        ch,
        quality: cfg.quality,
        case,
        note: "original",
    };
    if let Err(e) = save(&dir, &meta, pcm, o) {
        eprintln!("save {}: {e}", dir.display());
    }
    let class = o.class.unwrap();
    let mut summary = format!(
        "[{id}] {} [{}] rate={rate} ch={ch} frames={}\n  {}\n  saved: {}",
        class.name(),
        o.signature,
        pcm.len() / ch as usize,
        o.message.replace('\n', "\n  "),
        dir.display()
    );
    if !minimize || class == Class::OracleFail {
        return summary;
    }
    let mut m = Minimizer {
        cfg,
        rate,
        ch,
        class,
        signature: o.signature.clone(),
        evals: 0,
        deadline: Instant::now() + Duration::from_secs(240),
        log: vec![],
    };
    let mut start_pcm = pcm.to_vec();
    let mut min_case = case.cloned();
    if let Some(c) = min_case.as_mut() {
        m.segments(c);
        start_pcm = c.render();
    }
    let (min_pcm, _trim) = m.pcm(start_pcm);
    let mo = run_case(cfg, &min_pcm, rate, ch);
    if mo.class != Some(class) || mo.signature != o.signature {
        summary += "\n  (minimization lost the failure; this should not happen)";
        return summary;
    }
    let note = format!("minimized ({} evals): {}", m.evals, m.log.join("; "));
    let mdir = dir.join("minimized");
    let mmeta = Meta {
        note: &note,
        case: min_case.as_ref(),
        ..meta
    };
    if let Err(e) = save(&mdir, &mmeta, &min_pcm, &mo) {
        eprintln!("save {}: {e}", mdir.display());
    }
    summary += &format!(
        "\n  minimized: {} frames ({} evals) -> {}\n  {}",
        min_pcm.len() / ch as usize,
        m.evals,
        mdir.display(),
        mo.message.replace('\n', "\n  ")
    );
    summary
}

// ---------------------------------------------------------------------------

fn main() {
    let cfg = parse_args();
    install_panic_hook();

    if let Some(path) = cfg.pcm.clone() {
        let (rate, ch) = cfg.combos[0];
        if cfg.combos.len() != 1 {
            eprintln!("--pcm needs exactly one --rates and one --channels value");
            std::process::exit(2);
        }
        let raw = std::fs::read(&path).unwrap_or_else(|e| {
            eprintln!("read {}: {e}", path.display());
            std::process::exit(2)
        });
        let pcm: Vec<i16> = raw
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        let o = run_case(&cfg, &pcm, rate, ch);
        match o.class {
            None => println!("MATCH ({} bytes)", o.oracle.len()),
            Some(_) => {
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or("pcm".into());
                let id = format!("replay-{stem}-{rate}-{ch}ch");
                println!(
                    "{}",
                    handle_failure(
                        &cfg,
                        &id,
                        None,
                        None,
                        None,
                        &pcm,
                        rate,
                        ch,
                        &o,
                        cfg.minimize
                    )
                );
                std::process::exit(1);
            }
        }
        return;
    }

    let cfg = Arc::new(cfg);
    let next = Arc::new(AtomicUsize::new(cfg.start));
    let done = Arc::new(AtomicUsize::new(0));
    let skipped = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    // Per (rate, ch) combo: (cases run, failures).
    let per_combo: Arc<Vec<(AtomicUsize, AtomicUsize)>> = Arc::new(
        cfg.combos
            .iter()
            .map(|_| (AtomicUsize::new(0), AtomicUsize::new(0)))
            .collect(),
    );
    let frames_total = Arc::new(AtomicUsize::new(0));
    let state = Arc::new(Mutex::new(FailState::default()));
    let t0 = Instant::now();
    let end = cfg.start + cfg.iters;

    println!(
        "parity-fuzz: seed={} iters={} (start {}) quality={} combos={:?} jobs={} oracle={}",
        cfg.seed,
        cfg.iters,
        cfg.start,
        cfg.quality,
        cfg.combos,
        cfg.jobs,
        cfg.oracle.display()
    );

    let mut handles = vec![];
    for _ in 0..cfg.jobs {
        let (cfg, next, done, skipped, stop, per_combo, state, frames_total) = (
            cfg.clone(),
            next.clone(),
            done.clone(),
            skipped.clone(),
            stop.clone(),
            per_combo.clone(),
            state.clone(),
            frames_total.clone(),
        );
        handles.push(std::thread::spawn(move || {
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                if let Some(t) = cfg.max_secs
                    && t0.elapsed().as_secs_f64() > t
                {
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= end {
                    break;
                }
                let ci = i % cfg.combos.len();
                let (rate, ch) = cfg.combos[ci];
                let case_seed = mix_seed(cfg.seed, i as u64);
                let case = make_case(case_seed, rate, ch, cfg.max_len);
                if case.frames() < cfg.min_frames {
                    skipped.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                let pcm = case.render();
                let o = run_case(&cfg, &pcm, rate, ch);
                per_combo[ci].0.fetch_add(1, Ordering::Relaxed);
                frames_total.fetch_add(case.frames(), Ordering::Relaxed);
                if let Some(class) = o.class {
                    per_combo[ci].1.fetch_add(1, Ordering::Relaxed);
                    if !cfg.keep_going {
                        stop.store(true, Ordering::Relaxed);
                    }
                    let id = format!("s{}-i{}-{}-{}ch", cfg.seed, i, rate, ch);
                    let minimize = {
                        let mut st = state.lock().unwrap();
                        let e = st.by_sig.entry(o.signature.clone()).or_default();
                        e.0 += 1;
                        e.1.push(id.clone());
                        cfg.minimize && e.0 <= cfg.min_per_sig
                    };
                    eprintln!(
                        "!! {} [{}] at iter {i} ({} {}Hz {}ch, {} frames){}",
                        class.name(),
                        o.signature,
                        case.kind,
                        rate,
                        ch,
                        case.frames(),
                        if minimize { " — minimizing..." } else { "" }
                    );
                    let s = handle_failure(
                        &cfg,
                        &id,
                        Some(i),
                        Some(case_seed),
                        Some(&case),
                        &pcm,
                        rate,
                        ch,
                        &o,
                        minimize,
                    );
                    let mut st = state.lock().unwrap();
                    if minimize || !cfg.keep_going {
                        eprintln!("{s}\n");
                        st.detailed.push(s);
                    }
                    st.count += 1;
                }
                done.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    let mut last = Instant::now();
    while handles.iter().any(|h| !h.is_finished()) {
        std::thread::sleep(Duration::from_millis(200));
        if last.elapsed() >= Duration::from_secs(10) {
            last = Instant::now();
            let d = done.load(Ordering::Relaxed);
            let el = t0.elapsed().as_secs_f64();
            println!(
                "  [{:>6.0}s] {d}/{} cases ({} skipped), {:.1}/s, {:.0}s audio, {} failures",
                el,
                cfg.iters,
                skipped.load(Ordering::Relaxed),
                d as f64 / el,
                frames_total.load(Ordering::Relaxed) as f64 / 44100.0,
                state.lock().unwrap().count
            );
        }
    }
    for h in handles {
        let _ = h.join();
    }

    let st = state.lock().unwrap();
    println!(
        "\nparity-fuzz done in {:.1}s: {} cases run, {} skipped (< --min-frames), {} failures",
        t0.elapsed().as_secs_f64(),
        done.load(Ordering::Relaxed),
        skipped.load(Ordering::Relaxed),
        st.count
    );
    for (k, &(r, c)) in cfg.combos.iter().enumerate() {
        println!(
            "  {r} Hz {c}ch: {} cases, {} failures",
            per_combo[k].0.load(Ordering::Relaxed),
            per_combo[k].1.load(Ordering::Relaxed)
        );
    }
    if st.count == 0 {
        println!("no divergences");
        return;
    }
    println!("\nfailure signatures:");
    for (sig, (n, ids)) in &st.by_sig {
        let shown: Vec<&str> = ids.iter().take(8).map(|s| s.as_str()).collect();
        println!(
            "  {n:>5}  {sig}  e.g. {}{}",
            shown.join(" "),
            if ids.len() > 8 { " ..." } else { "" }
        );
    }
    println!("\ndetails (first {} per signature):", cfg.min_per_sig);
    for f in &st.detailed {
        println!("{f}\n");
    }
    std::process::exit(1);
}

#[derive(Default)]
struct FailState {
    count: usize,
    /// signature -> (count, case ids)
    by_sig: std::collections::BTreeMap<String, (usize, Vec<String>)>,
    detailed: Vec<String>,
}
