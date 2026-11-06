//! StellarLINK backend: starts a patched OpenOCD as a subprocess and
//! controls it through [`oocd`].
//!
//! Generic: board knowledge lives in data files, never in this crate.
#![forbid(unsafe_code)]
