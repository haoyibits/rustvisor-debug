//! Colours for the human output: the shared [`probecore::style`] plus the
//! `tracebridge:` prefixes on stderr (`rvdbg:` when rvdbg runs it).
//!
//! Two things stay as they are: the `[tracebridge]` prefix of `ui::info` is
//! always cyan (as in the Python tool), and the log of the `adapter` proxy is
//! always plain, because the IDEs match its lines.

use std::fmt;

use crate::ui;

pub use probecore::style::*;

/// `tracebridge: <message>` on stderr.
pub fn error(message: impl fmt::Display) {
    eprintln!(
        "{} {message}",
        Style::stderr().bad(format!("{}:", ui::program()))
    );
}

/// `tracebridge: warning: <message>` on stderr.
pub fn warning(message: impl fmt::Display) {
    eprintln!(
        "{} {message}",
        Style::stderr().warn(format!("{}: warning:", ui::program()))
    );
}

/// `tracebridge: <message>` on stderr, for what is done without being asked.
pub fn notice(message: impl fmt::Display) {
    eprintln!(
        "{} {message}",
        Style::stderr().warn(format!("{}:", ui::program()))
    );
}
