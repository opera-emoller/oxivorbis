//! `oxivorbis`: a command-line Ogg Vorbis encoder on top of the
//! `oxivorbis` library, modelled loosely on `oggenc`.
//!
//! ```text
//! oxivorbis [options] <input.wav|-> ... [-o output.ogg]
//! oxivorbis --raw --rate 44100 --channels 2 [--format s16le] - > out.ogg
//! ```
//!
//! Modules:
//!
//! - `args`: the clap definition (and the help text) and its conversion
//!   to a plain `Config`.
//! - `wav`: streaming RIFF/WAVE/RF64 header parser.
//! - `pcm`: sample formats and their exact conversion to i16/f32.
//! - `codec`: the only code that calls the library API.
//! - `job`: one input to one output, with the overwrite policy.
//! - `msg`: stderr messages (colored only on a terminal, honours NO_COLOR).
//!
//! Exit status: 0 ok, 1 if any input failed (I/O or input error), 2 for
//! usage errors. A closed stdout (`| head`) ends quietly with status 0.

mod args;
mod codec;
mod job;
mod msg;
mod pcm;
mod wav;

use anyhow::Context;
use clap::CommandFactory;
use std::io::{IsTerminal, Write};
use std::process::ExitCode;

/// Print a man page or completion script for `oxivorbis` to stdout.
fn generate(kind: args::Generate) -> anyhow::Result<()> {
    use clap_complete::Shell;
    let mut cmd = args::Cli::command();
    let mut out = Vec::new();
    let shell = match kind {
        args::Generate::Man => {
            clap_mangen::Man::new(cmd.clone())
                .render(&mut out)
                .context("failed to render the man page")?;
            None
        }
        args::Generate::CompleteBash => Some(Shell::Bash),
        args::Generate::CompleteZsh => Some(Shell::Zsh),
        args::Generate::CompleteFish => Some(Shell::Fish),
        args::Generate::CompletePowershell => Some(Shell::PowerShell),
        args::Generate::CompleteElvish => Some(Shell::Elvish),
    };
    if let Some(shell) = shell {
        clap_complete::generate(shell, &mut cmd, "oxivorbis", &mut out);
    }
    match std::io::stdout().write_all(&out) {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => {
            Err(e).context("failed to write stdout")
        }
        _ => Ok(()),
    }
}

fn main() -> ExitCode {
    let cfg = match args::parse(std::env::args_os().skip(1).collect()) {
        Ok(args::Command::Encode(cfg)) => cfg,
        Ok(args::Command::Generate(kind)) => {
            return match generate(kind) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    msg::error(&e);
                    ExitCode::FAILURE
                }
            };
        }
        // Usage errors exit 2; --help and --version print and exit 0.
        Err(e) => e.exit(),
    };
    let jobs = job::plan(&cfg);
    let usage = |m: &str| -> ! {
        args::Cli::command()
            .error(clap::error::ErrorKind::InvalidValue, m)
            .exit()
    };
    if jobs.iter().any(|j| matches!(j.output, job::Output::Stdout))
        && std::io::stdout().is_terminal()
    {
        usage("refusing to write binary Ogg data to a terminal; use -o <FILE> or redirect stdout");
    }
    if jobs.iter().any(|j| matches!(j.input, job::Input::Stdin)) && std::io::stdin().is_terminal() {
        usage("input is stdin, but stdin is a terminal; pipe or redirect audio in");
    }
    let mut failed = false;
    for j in &jobs {
        match job::run(j, &cfg) {
            Ok(summary) => {
                if !cfg.quiet {
                    let setting = codec::describe(cfg.quality, &cfg.rate_args);
                    msg::line(summary.line(j, &setting));
                }
            }
            Err(e) => failed |= job::report(&e),
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
