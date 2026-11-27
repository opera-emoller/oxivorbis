//! Encoding one input to one output: open, parse, stream, summarize.

use crate::args::Config;
use crate::codec::{self, Enc};
use crate::msg;
use crate::pcm::{self, SampleFormat};
use crate::wav;
use anyhow::{Context, Result, anyhow, bail};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub enum Input {
    Stdin,
    File(PathBuf),
}

pub enum Output {
    Stdout,
    File(PathBuf),
}

impl Input {
    pub fn name(&self) -> String {
        match self {
            Input::Stdin => "stdin".into(),
            Input::File(p) => p.display().to_string(),
        }
    }
}

impl Output {
    pub fn name(&self) -> String {
        match self {
            Output::Stdout => "stdout".into(),
            Output::File(p) => p.display().to_string(),
        }
    }
}

pub struct Job {
    pub index: usize,
    pub input: Input,
    pub output: Output,
}

/// The reader of stdout went away (`| head`). Ends the job quietly; see
/// [`is_broken_pipe`].
#[derive(Debug)]
struct BrokenPipe;

impl std::fmt::Display for BrokenPipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stdout closed")
    }
}

impl std::error::Error for BrokenPipe {}

/// Whether a job error is a closed stdout, which is not reported.
pub fn is_broken_pipe(e: &anyhow::Error) -> bool {
    e.is::<BrokenPipe>()
}

/// Plan the (input, output) pairs from the parsed arguments.
pub fn plan(cfg: &Config) -> Vec<Job> {
    cfg.inputs
        .iter()
        .enumerate()
        .map(|(index, p)| {
            let input = if p.as_os_str() == "-" {
                Input::Stdin
            } else {
                Input::File(p.clone())
            };
            let output = match (&cfg.output, &input) {
                (Some(o), _) if o.as_os_str() == "-" => Output::Stdout,
                (Some(o), _) => Output::File(o.clone()),
                (None, Input::Stdin) => Output::Stdout,
                (None, Input::File(p)) => Output::File(p.with_extension("ogg")),
            };
            Job {
                index,
                input,
                output,
            }
        })
        .collect()
}

/// Whether two paths name the same existing file (hard links and
/// symlinks included).
fn same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (fs::metadata(a), fs::metadata(b)) {
            (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        match (fs::canonicalize(a), fs::canonicalize(b)) {
            (Ok(x), Ok(y)) => x == y,
            _ => false,
        }
    }
}

/// `r.read(buf)`, retried on EINTR.
fn read_retrying(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        match r.read(buf) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            other => return other,
        }
    }
}

/// Where encoded bytes go. A new or replaced file is written to a
/// temporary file in the same directory and renamed into place by
/// [`Sink::commit`], so a failed encode never leaves a truncated `.ogg`
/// behind or clobbers an existing one (with `-f`).
struct Sink {
    w: BufWriter<Box<dyn Write>>,
    stdout: bool,
    /// (temporary, final) for renamed outputs.
    rename: Option<(PathBuf, PathBuf)>,
    name: String,
    bytes: u64,
}

impl Sink {
    fn open(out: &Output, force: bool) -> Result<Sink> {
        let name = out.name();
        let (w, stdout, rename): (Box<dyn Write>, _, _) = match out {
            Output::Stdout => (Box::new(io::stdout().lock()), true, None),
            Output::File(path) => match fs::metadata(path) {
                Ok(m) if m.is_dir() => bail!("output '{name}' is a directory"),
                // Devices and FIFOs (/dev/null, named pipes): write directly.
                Ok(m) if !m.is_file() => {
                    let f = fs::OpenOptions::new()
                        .write(true)
                        .open(path)
                        .with_context(|| format!("failed to open '{name}'"))?;
                    (Box::new(f), false, None)
                }
                Ok(_) if !force => bail!("'{name}' already exists (use -f to overwrite)"),
                _ => {
                    let dir = match path.parent() {
                        Some(d) if !d.as_os_str().is_empty() => d,
                        _ => Path::new("."),
                    };
                    let file = path.file_name().unwrap_or_default().to_string_lossy();
                    let tmp = dir.join(format!(".{file}.{}.oxivorbis-tmp", std::process::id()));
                    let f = fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&tmp)
                        .with_context(|| format!("failed to create '{name}'"))?;
                    (Box::new(f), false, Some((tmp, path.clone())))
                }
            },
        };
        Ok(Sink {
            w: BufWriter::with_capacity(1 << 16, w),
            stdout,
            rename,
            name,
            bytes: 0,
        })
    }

    fn io_fail(&self, e: io::Error) -> anyhow::Error {
        if self.stdout && e.kind() == io::ErrorKind::BrokenPipe {
            anyhow!(BrokenPipe)
        } else {
            anyhow!(e).context(format!("failed to write '{}'", self.name))
        }
    }

    fn write(&mut self, b: &[u8]) -> Result<()> {
        self.bytes += b.len() as u64;
        self.w.write_all(b).map_err(|e| self.io_fail(e))
    }

    fn commit(mut self) -> Result<u64> {
        self.w.flush().map_err(|e| self.io_fail(e))?;
        if let Some((tmp, dst)) = self.rename.take() {
            if let Err(e) = fs::rename(&tmp, &dst) {
                let _ = fs::remove_file(&tmp);
                return Err(anyhow!(e).context(format!("failed to create '{}'", dst.display())));
            }
        }
        Ok(self.bytes)
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        // Not committed: discard the temporary file.
        if let Some((tmp, _)) = self.rename.take() {
            let _ = fs::remove_file(tmp);
        }
    }
}

/// What a finished job reports.
pub struct Summary {
    pub rate: u32,
    pub channels: u16,
    pub format: SampleFormat,
    pub frames: u64,
    pub bytes: u64,
    pub elapsed: Duration,
}

fn clock(secs: f64) -> String {
    let t = secs.max(0.0);
    let m = (t / 60.0).floor();
    format!("{}:{:05.2}", m as u64, t - m * 60.0)
}

fn size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1024.0 * 1024.0 {
        format!("{:.2} MiB", b / (1024.0 * 1024.0))
    } else if b >= 1024.0 {
        format!("{:.1} KiB", b / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

impl Summary {
    /// `in.wav -> in.ogg: 3:21.50, 44100 Hz stereo 16-bit, q3, 3.10 MiB,
    /// 129.3 kbps, 280.1x realtime`; `setting` is the `q3` part.
    pub fn line(&self, job: &Job, setting: &str) -> String {
        let secs = self.frames as f64 / self.rate as f64;
        let kbps = if secs > 0.0 {
            format!("{:.1} kbps", self.bytes as f64 * 8.0 / secs / 1000.0)
        } else {
            "- kbps".into()
        };
        let took = self.elapsed.as_secs_f64();
        let speed = if took > 0.0 && secs > 0.0 {
            format!(", {:.1}x realtime", secs / took)
        } else {
            String::new()
        };
        let ch = match self.channels {
            1 => "mono".to_string(),
            2 => "stereo".to_string(),
            6 => "5.1".to_string(),
            n => format!("{n}-channel"),
        };
        format!(
            "{} -> {}: {}, {} Hz {ch} {}, {setting}, {}, {kbps}{speed}",
            job.input.name(),
            job.output.name(),
            clock(secs),
            self.rate,
            self.format.describe(),
            size(self.bytes),
        )
    }
}

/// Live progress, only on a terminal stderr.
struct Progress {
    on: bool,
    last: Instant,
    name: String,
    total_frames: Option<u64>,
    rate: u32,
}

impl Progress {
    fn tick(&mut self, frames: u64) {
        if !self.on || self.last.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.last = Instant::now();
        let at = clock(frames as f64 / self.rate as f64);
        let msg = match self.total_frames {
            Some(t) if t > 0 => format!(
                "{}: {:3.0}% ({at})",
                self.name,
                100.0 * frames as f64 / t as f64
            ),
            _ => format!("{}: {at}", self.name),
        };
        msg::raw(format_args!("\r\x1b[2K{msg}"));
    }

    fn clear(&self) {
        if self.on {
            msg::raw("\r\x1b[2K");
        }
    }
}

/// Encode one job. Never panics on bad input.
pub fn run(job: &Job, cfg: &Config) -> Result<Summary> {
    let start = Instant::now();
    let in_name = job.input.name();
    let read_ctx = || format!("failed to read '{in_name}'");
    let mut reader: Box<dyn Read> = match &job.input {
        Input::Stdin => Box::new(io::stdin().lock()),
        Input::File(p) => {
            if matches!(&job.output, Output::File(o) if same_file(p, o)) {
                bail!("refusing to overwrite the input file '{in_name}'");
            }
            let f = File::open(p).with_context(|| format!("failed to open '{in_name}'"))?;
            if f.metadata().is_ok_and(|m| m.is_dir()) {
                bail!("'{in_name}' is a directory");
            }
            Box::new(BufReader::with_capacity(1 << 16, f))
        }
    };

    let (rate, channels, format, data_len) = match cfg.raw {
        Some(r) => (r.rate, r.channels, r.format, None),
        None => {
            let w = wav::read_header(&mut reader)
                .map_err(anyhow::Error::msg)
                .with_context(read_ctx)?;
            (w.rate, w.channels, w.format, w.data_len)
        }
    };
    // WAV stores 3..=8 channels in WAV/SMPTE order (5.1: L R C LFE BL BR);
    // Vorbis wants its own (L C R BL BR LFE). Reorder as oggenc does. Raw
    // input is taken to be in Vorbis order already.
    let reorder = cfg.raw.is_none() && codec::wav_order_differs(channels);
    let settings = codec::Settings {
        quality: cfg.quality,
        rate_args: cfg.rate_args.clone(),
        serial: cfg.serial_for(job.index),
        comments: cfg.comments_for(job.index),
    };
    let mut enc = Enc::new(rate, channels, &settings)
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("cannot encode '{in_name}'"))?;
    // Opened only once the input is known to be encodable, so a bad
    // input never creates or replaces an output file.
    let mut sink = Sink::open(&job.output, cfg.force)?;

    let sample_bytes = format.bytes();
    let frame_bytes = (sample_bytes * channels as usize) as u64;
    let mut progress = Progress {
        on: !cfg.quiet && io::stderr().is_terminal(),
        last: Instant::now(),
        name: in_name.clone(),
        total_frames: data_len.map(|l| l / frame_bytes),
        rate,
    };
    let mut src = reader.take(data_len.unwrap_or(u64::MAX));
    let mut buf = vec![0u8; 1 << 16];
    let mut filled = 0usize;
    let mut total: u64 = 0;
    let mut ibuf: Vec<i16> = Vec::new();
    let mut fbuf: Vec<f32> = Vec::new();
    loop {
        let n = read_retrying(&mut src, &mut buf[filled..]).with_context(read_ctx)?;
        if n == 0 {
            break;
        }
        filled += n;
        total += n as u64;
        // Whole samples; whole frames when reordering channels.
        let unit = if reorder {
            frame_bytes as usize
        } else {
            sample_bytes
        };
        let whole = filled - filled % unit;
        if whole == 0 {
            continue;
        }
        let pages = if format == SampleFormat::S16 {
            pcm::to_i16(&buf[..whole], &mut ibuf);
            if reorder {
                enc.reorder_wav(&mut ibuf);
            }
            enc.write_i16(&ibuf)
        } else {
            pcm::to_f32(format, &buf[..whole], &mut fbuf);
            if reorder {
                enc.reorder_wav(&mut fbuf);
            }
            enc.write_f32(&fbuf)
        };
        sink.write(pages)?;
        buf.copy_within(whole..filled, 0);
        filled -= whole;
        progress.tick(total / frame_bytes);
    }
    progress.clear();
    if let Some(len) = data_len.filter(|&len| total < len) {
        // Dropping the sink discards a temporary output file.
        return Err(anyhow!(
            "file is truncated: the data chunk declares {len} bytes but only {total} are present"
        ))
        .with_context(read_ctx);
    }
    // A trailing partial sample (or, when reordering, frame) is dropped
    // here, and a trailing partial frame by the encoder.
    let tail = enc.finish();
    sink.write(&tail)?;
    let bytes = sink.commit()?;
    Ok(Summary {
        rate,
        channels,
        format,
        frames: total / frame_bytes,
        bytes,
        elapsed: start.elapsed(),
    })
}

/// Report a job error unless it is a closed stdout. Returns whether it
/// counts as a failure.
pub fn report(e: &anyhow::Error) -> bool {
    if is_broken_pipe(e) {
        return false;
    }
    msg::error(e);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting() {
        assert_eq!(clock(0.0), "0:00.00");
        assert_eq!(clock(201.5), "3:21.50");
        assert_eq!(size(10), "10 B");
        assert_eq!(size(2048), "2.0 KiB");
        assert_eq!(size(3 << 20), "3.00 MiB");
    }

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
    fn broken_pipe_marker() {
        let e = anyhow!(BrokenPipe);
        assert!(is_broken_pipe(&e));
        assert!(!is_broken_pipe(&anyhow!("x")));
    }
}
