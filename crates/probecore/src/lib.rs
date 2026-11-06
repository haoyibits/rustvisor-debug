//! Probe-independent analysis shared by the debug backends.
//!
//! The analysis code (check files, clock trees, ELF compare, the RTT ring
//! buffer and fault decoding) only needs typed target reads. Each backend
//! (`tracebridge` for TRACE32, `stellarbridge` for OpenOCD) implements those
//! reads; this crate knows nothing about a particular probe, board or
//! hypervisor.
#![forbid(unsafe_code)]
