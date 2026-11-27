//! Messages on stderr. Styling goes through `anstream`, which drops the
//! escape codes when stderr is not a terminal or `NO_COLOR` is set.
//! Write failures are ignored: a closed stderr must not become a panic
//! the way `eprintln!` would.

use anstyle::{AnsiColor, Style};
use std::fmt::Display;
use std::io::Write;

const ERROR: Style = AnsiColor::Red.on_default().bold();

pub fn error(e: &anyhow::Error) {
    // `{:#}` prints the context chain on one line: "failed to read 'x.wav': ...".
    let _ = writeln!(anstream::stderr(), "{ERROR}error:{ERROR:#} {e:#}");
}

pub fn line(m: impl Display) {
    let _ = writeln!(anstream::stderr(), "{m}");
}

/// Raw text without a newline (progress updates).
pub fn raw(m: impl Display) {
    let mut e = anstream::stderr();
    let _ = write!(e, "{m}");
    let _ = e.flush();
}
