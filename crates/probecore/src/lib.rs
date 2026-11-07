//! Probe-independent analysis shared by the debug backends.
//!
//! The analysis code (check files, clock trees, ELF compare, fault decoding)
//! and the debugger's end of the RAM channels (RTT, FDX) only need typed
//! target reads and writes. Each backend
//! (`tracebridge` for TRACE32, `stellarbridge` for OpenOCD) implements those
//! reads; this crate knows nothing about a particular probe, board or
//! hypervisor.
#![forbid(unsafe_code)]

pub mod check;
pub mod clock;
pub mod decode;
pub mod elf;
pub mod fault;
pub mod fdx;
pub mod rtt;
pub mod style;
pub mod target;
pub mod text;

pub use target::{Error, Memory, Result, Target, TargetAddress};
