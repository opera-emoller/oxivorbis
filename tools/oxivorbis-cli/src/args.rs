//! Command-line interface (clap derive) and its conversion to [`Config`].

use crate::codec;
use crate::pcm::SampleFormat;
use clap::{ArgAction, CommandFactory, FromArgMatches, Parser, ValueEnum};
use std::ffi::OsString;
use std::path::PathBuf;

/// oggenc's default quality, so `oxivorbis x.wav` is a drop-in for
/// `oggenc x.wav` in size and sound.
pub const DEFAULT_QUALITY: f32 = 3.0;

const LONG_ABOUT: &str = "\
Encode WAV or raw PCM to Ogg Vorbis. The audio is byte-identical to what
libvorbis 1.3.7 produces for the same samples.

Each input is encoded to its own .ogg next to it (extension replaced).
`-` reads stdin, and stdin input goes to stdout unless -o is given.
Existing output files are not overwritten unless -f is given, and the
input file is never overwritten.";

const AFTER_LONG_HELP: &str = "\
Input formats:
  WAV (RIFF, RF64/BW64, WAVE_FORMAT_EXTENSIBLE), mono or stereo:
  8-bit unsigned, 16/24/32-bit signed integer, 32/64-bit float.
  16-bit samples go to the encoder unchanged as i16 (bit-exact with the
  library's `encode`). Other formats are converted exactly to f32:
  integers as x/2^(bits-1), floats passed through (f64 rounded once).

Exit status:
  0 ok, 1 encode or I/O error (any input), 2 usage error.
  A closed stdout (e.g. `| head`) stops quietly with status 0.

Examples:
  oxivorbis song.wav                      # writes song.ogg at q3
  oxivorbis -q 6 -a Artist -l Album *.wav # one .ogg per input
  ffmpeg -i in.flac -f wav - | oxivorbis - > out.ogg
  oxivorbis --raw -R 48000 -C 2 -F f32le - < pcm.f32 > out.ogg";

/// Encode WAV or raw PCM to Ogg Vorbis (byte-identical to libvorbis 1.3.7).
#[derive(Parser, Debug)]
#[command(
    name = "oxivorbis",
    version,

    long_about = LONG_ABOUT,
    after_long_help = AFTER_LONG_HELP,
    max_term_width = 100
)]
pub struct Cli {
    /// Input files (WAV, or raw PCM with --raw); `-` reads stdin.
    #[arg(value_name = "INPUT", required_unless_present = "generate")]
    pub inputs: Vec<PathBuf>,

    /// Output file; `-` writes to stdout. Only with a single input.
    #[arg(short, long, value_name = "FILE", help_heading = "Output")]
    pub output: Option<PathBuf>,

    /// Overwrite existing output files.
    #[arg(short, long, help_heading = "Output")]
    pub force: bool,

    /// No progress or summary on stderr.
    #[arg(short = 'Q', long, help_heading = "Output")]
    pub quiet: bool,

    /// Quality, -1 to 10, fractional values allowed (oggenc's default, 3).
    #[arg(
        short,
        long,
        value_name = "Q",
        default_value_t = DEFAULT_QUALITY,
        value_parser = parse_quality,
        allow_negative_numbers = true,
        help_heading = "Encoding"
    )]
    pub quality: f32,

    /// Ogg stream serial, decimal or 0x hex; the n-th input gets SERIAL+n.
    /// Default: oxivorbis's fixed serial, so output is deterministic.
    #[arg(short, long, value_parser = parse_serial, help_heading = "Encoding")]
    pub serial: Option<u32>,

    /// Accepted for compatibility; streaming is always on.
    #[arg(long, hide = true)]
    pub stream: bool,

    /// Add the comment TAG=VALUE to every output (repeatable).
    #[arg(
        short,
        long = "comment",
        value_name = "TAG=VALUE",
        action = ArgAction::Append,
        value_parser = parse_comment,
        help_heading = "Comments"
    )]
    pub comments: Vec<(String, String)>,

    /// TITLE. Given once it tags every input; repeated, the n-th value
    /// tags the n-th input (as in oggenc). Same for the tags below.
    #[arg(short, long, action = ArgAction::Append, help_heading = "Comments")]
    pub title: Vec<String>,

    /// ARTIST.
    #[arg(short, long, action = ArgAction::Append, help_heading = "Comments")]
    pub artist: Vec<String>,

    /// ALBUM.
    #[arg(short = 'l', long, action = ArgAction::Append, help_heading = "Comments")]
    pub album: Vec<String>,

    /// TRACKNUMBER.
    #[arg(short = 'N', long, action = ArgAction::Append, help_heading = "Comments")]
    pub tracknum: Vec<String>,

    /// GENRE.
    #[arg(short = 'G', long, action = ArgAction::Append, help_heading = "Comments")]
    pub genre: Vec<String>,

    /// DATE.
    #[arg(short, long, action = ArgAction::Append, help_heading = "Comments")]
    pub date: Vec<String>,

    /// Treat inputs as headerless interleaved little-endian PCM.
    #[arg(short, long, requires_all = ["rate", "channels"], help_heading = "Raw input")]
    pub raw: bool,

    /// Raw input sample rate in Hz.
    #[arg(
        short = 'R',
        long,
        alias = "raw-rate",
        requires = "raw",
        value_parser = clap::value_parser!(u32).range(1..),
        help_heading = "Raw input"
    )]
    pub rate: Option<u32>,

    /// Raw input channels: 1, 2, mono or stereo.
    #[arg(
        short = 'C',
        long,
        alias = "raw-chan",
        requires = "raw",
        value_parser = parse_channels,
        help_heading = "Raw input"
    )]
    pub channels: Option<u16>,

    /// Raw input sample format.
    #[arg(
        short = 'F',
        long,
        value_enum,
        default_value_t = RawFormat::S16le,
        requires = "raw",
        help_heading = "Raw input"
    )]
    pub format: RawFormat,

    /// Print a man page or shell completion script to stdout and exit.
    #[arg(long, value_enum, value_name = "KIND", exclusive = true)]
    pub generate: Option<Generate>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum RawFormat {
    U8,
    S16le,
    S24le,
    S32le,
    F32le,
    F64le,
}

impl From<RawFormat> for SampleFormat {
    fn from(f: RawFormat) -> SampleFormat {
        match f {
            RawFormat::U8 => SampleFormat::U8,
            RawFormat::S16le => SampleFormat::S16,
            RawFormat::S24le => SampleFormat::S24,
            RawFormat::S32le => SampleFormat::S32,
            RawFormat::F32le => SampleFormat::F32,
            RawFormat::F64le => SampleFormat::F64,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Generate {
    /// roff man page (oxivorbis.1).
    Man,
    CompleteBash,
    CompleteZsh,
    CompleteFish,
    CompletePowershell,
    CompleteElvish,
}

fn parse_quality(s: &str) -> Result<f32, String> {
    s.parse::<f32>()
        .ok()
        .filter(|&q| codec::quality_ok(q))
        .ok_or_else(|| "want a number from -1 to 10".into())
}

fn parse_serial(s: &str) -> Result<u32, String> {
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
    .ok_or_else(|| "want a 32-bit unsigned number".into())
}

fn parse_channels(s: &str) -> Result<u16, String> {
    match s {
        "mono" => Ok(1),
        "stereo" => Ok(2),
        _ => s
            .parse::<u16>()
            .ok()
            .filter(|&n| n > 0)
            .ok_or_else(|| "want 1, 2, mono or stereo".into()),
    }
}

fn parse_comment(s: &str) -> Result<(String, String), String> {
    let (k, v) = s.split_once('=').ok_or("want TAG=VALUE")?;
    if k.is_empty() || !k.bytes().all(|b| (0x20..=0x7D).contains(&b)) {
        return Err("tags are ASCII 0x20..0x7D without '='".into());
    }
    Ok((k.to_string(), v.to_string()))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawSpec {
    pub rate: u32,
    pub channels: u16,
    pub format: SampleFormat,
}

/// Tag conveniences: (Vorbis key, values in the order given).
pub type Tags = Vec<(&'static str, Vec<String>)>;

/// What the encoder needs from the command line.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Input paths; `-` is stdin.
    pub inputs: Vec<PathBuf>,
    /// `-o`; `-` is stdout.
    pub output: Option<PathBuf>,
    pub quality: f32,
    pub serial: Option<u32>,
    /// `-c` comments, applied to every file.
    pub comments: Vec<(String, String)>,
    pub tags: Tags,
    pub quiet: bool,
    pub force: bool,
    pub raw: Option<RawSpec>,
}

impl Config {
    /// Comments for the `i`-th input: tag conveniences first, then `-c`.
    pub fn comments_for(&self, i: usize) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (key, vals) in &self.tags {
            let v = if vals.len() == 1 {
                vals.first()
            } else {
                vals.get(i)
            };
            if let Some(v) = v {
                out.push((key.to_string(), v.clone()));
            }
        }
        out.extend(self.comments.iter().cloned());
        out
    }

    /// Serial for the `i`-th input.
    pub fn serial_for(&self, i: usize) -> Option<u32> {
        self.serial.map(|s| s.wrapping_add(i as u32))
    }
}

pub enum Command {
    Encode(Config),
    Generate(Generate),
}

/// The pre-clap benchmark form `oxivorbis [--stream] <rate> <channels>
/// [quality]` (raw s16le stdin to stdout, q5, quiet), still used by the
/// repository's `just bench`. Recognised only when the first argument is
/// a number that isn't an existing file.
fn legacy(args: &[OsString]) -> Option<Config> {
    let mut a: Vec<&str> = args.iter().map(|s| s.to_str()).collect::<Option<_>>()?;
    if a.first() == Some(&"--stream") {
        a.remove(0);
    }
    if !(2..=3).contains(&a.len()) || std::path::Path::new(a[0]).exists() {
        return None;
    }
    let rate: u32 = a[0].parse().ok()?;
    let channels = parse_channels(a[1]).ok()?;
    let quality = match a.get(2) {
        Some(q) => parse_quality(q).ok()?,
        None => 5.0,
    };
    Some(Config {
        inputs: vec!["-".into()],
        output: Some("-".into()),
        quality,
        serial: None,
        comments: Vec::new(),
        tags: Vec::new(),
        quiet: true,
        force: false,
        raw: Some(RawSpec {
            rate,
            channels,
            format: SampleFormat::S16,
        }),
    })
}

/// Parse the arguments after the program name. Usage errors (and
/// `--help`/`--version`) come back as clap errors; `.exit()` on them
/// prints and exits with status 2 (0 for help/version).
pub fn parse(args: Vec<OsString>) -> Result<Command, clap::Error> {
    if let Some(cfg) = legacy(&args) {
        return Ok(Command::Encode(cfg));
    }
    // `--version` also names the library version (not a const, so set here).
    let long_version: &'static str = Box::leak(
        format!(
            "{} (library: {})",
            env!("CARGO_PKG_VERSION"),
            codec::library_version()
        )
        .into_boxed_str(),
    );
    let matches = Cli::command()
        .long_version(long_version)
        .try_get_matches_from(std::iter::once(OsString::from("oxivorbis")).chain(args))?;
    let cli = Cli::from_arg_matches(&matches)?;
    if let Some(g) = cli.generate {
        return Ok(Command::Generate(g));
    }
    let conflict = |msg: &str| Cli::command().error(clap::error::ErrorKind::ArgumentConflict, msg);
    if cli.output.is_some() && cli.inputs.len() > 1 {
        return Err(conflict("--output can only be used with a single input"));
    }
    if cli.inputs.iter().filter(|p| p.as_os_str() == "-").count() > 1 {
        return Err(conflict("stdin (`-`) can only be given once"));
    }
    let raw = cli.raw.then(|| RawSpec {
        // `requires_all` guarantees both are present with --raw.
        rate: cli.rate.unwrap_or_default(),
        channels: cli.channels.unwrap_or_default(),
        format: cli.format.into(),
    });
    let tags: Tags = [
        ("TITLE", cli.title),
        ("ARTIST", cli.artist),
        ("ALBUM", cli.album),
        ("TRACKNUMBER", cli.tracknum),
        ("GENRE", cli.genre),
        ("DATE", cli.date),
    ]
    .into_iter()
    .filter(|(_, v)| !v.is_empty())
    .collect();
    Ok(Command::Encode(Config {
        inputs: cli.inputs,
        output: cli.output,
        quality: cli.quality,
        serial: cli.serial,
        comments: cli.comments,
        tags,
        quiet: cli.quiet,
        force: cli.force,
        raw,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(a: &[&str]) -> Result<Command, clap::Error> {
        parse(a.iter().map(OsString::from).collect())
    }

    fn cfg(a: &[&str]) -> Config {
        match p(a) {
            Ok(Command::Encode(c)) => c,
            Ok(Command::Generate(g)) => panic!("{g:?}"),
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn clap_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn defaults() {
        let c = cfg(&["a.wav"]);
        assert_eq!(c.inputs, [PathBuf::from("a.wav")]);
        assert_eq!(c.quality, 3.0);
        assert_eq!(c.output, None);
        assert!(!c.quiet && !c.force && c.raw.is_none());
        assert!(c.comments_for(0).is_empty());
    }

    #[test]
    fn value_forms() {
        for args in [
            &["-q", "6.5", "x"][..],
            &["-q6.5", "x"],
            &["--quality", "6.5", "x"],
            &["--quality=6.5", "x"],
        ] {
            assert_eq!(cfg(args).quality, 6.5, "{args:?}");
        }
        assert_eq!(cfg(&["-q", "-1", "x"]).quality, -1.0);
        assert_eq!(cfg(&["-q-0.5", "x"]).quality, -0.5);
        let c = cfg(&["-fQo", "out.ogg", "x"]);
        assert!(c.force && c.quiet);
        assert_eq!(c.output, Some("out.ogg".into()));
        assert_eq!(cfg(&["-s", "0x10", "x"]).serial, Some(16));
        assert_eq!(cfg(&["--", "-q"]).inputs, [PathBuf::from("-q")]);
        assert_eq!(cfg(&["--stream", "-"]).inputs, [PathBuf::from("-")]);
    }

    #[test]
    fn comments() {
        let c = cfg(&[
            "-t", "One", "-t", "Two", "-a", "Band", "-c", "X=a=b", "a.wav", "b.wav",
        ]);
        let s = |v: &[(&str, &str)]| -> Vec<(String, String)> {
            v.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        assert_eq!(
            c.comments_for(0),
            s(&[("TITLE", "One"), ("ARTIST", "Band"), ("X", "a=b")])
        );
        assert_eq!(
            c.comments_for(1),
            s(&[("TITLE", "Two"), ("ARTIST", "Band"), ("X", "a=b")])
        );
        assert_eq!(c.comments_for(2), s(&[("ARTIST", "Band"), ("X", "a=b")]));
        assert_eq!(cfg(&["-s", "7", "a", "b"]).serial_for(1), Some(8));
    }

    #[test]
    fn raw() {
        let c = cfg(&["--raw", "-R", "48000", "-C", "stereo", "-F", "f32le", "-"]);
        assert_eq!(
            c.raw,
            Some(RawSpec {
                rate: 48000,
                channels: 2,
                format: SampleFormat::F32
            })
        );
        assert_eq!(
            cfg(&["-r", "--raw-rate", "8000", "--raw-chan", "1", "-"])
                .raw
                .map(|r| r.format),
            Some(SampleFormat::S16)
        );
        assert!(p(&["--raw", "-R", "48000", "-"]).is_err());
        assert!(p(&["-R", "48000", "-C", "2", "-"]).is_err());
        assert!(p(&["-F", "u8", "-"]).is_err());
    }

    #[test]
    fn legacy_form() {
        let c = cfg(&["--stream", "44100", "stereo", "7"]);
        assert_eq!(c.quality, 7.0);
        assert!(c.quiet);
        assert_eq!(c.output, Some("-".into()));
        assert_eq!(
            c.raw,
            Some(RawSpec {
                rate: 44100,
                channels: 2,
                format: SampleFormat::S16
            })
        );
        assert_eq!(cfg(&["48000", "1"]).quality, 5.0);
    }

    #[test]
    fn usage_errors() {
        for args in [
            &[][..],
            &["--bogus", "x"],
            &["-z", "x"],
            &["-q", "11", "x"],
            &["-q", "abc", "x"],
            &["-q"],
            &["-o", "o.ogg", "a", "b"],
            &["-", "-"],
            &["-c", "novalue", "x"],
            &["-c", "=v", "x"],
            &["--force=1", "x"],
            &["-s", "-1", "x"],
            &["-F", "s16be", "--raw", "-R", "1", "-C", "1", "x"],
            &["--generate", "man", "x"],
        ] {
            let e = p(args).err().unwrap_or_else(|| panic!("{args:?}"));
            assert_eq!(e.exit_code(), 2, "{args:?}");
        }
        for (args, kind) in [
            (["x", "--help"], clap::error::ErrorKind::DisplayHelp),
            (["x", "-V"], clap::error::ErrorKind::DisplayVersion),
        ] {
            assert_eq!(p(&args).err().map(|e| e.kind()), Some(kind));
        }
        assert!(matches!(
            p(&["--generate", "complete-zsh"]),
            Ok(Command::Generate(Generate::CompleteZsh))
        ));
    }
}
