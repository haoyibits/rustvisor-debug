//! Client for the OpenOCD Tcl RPC server (the `tcl_port`, commands
//! terminated by `0x1a`).
//!
//! OpenOCD runs as a separate process; this crate only talks to it over TCP
//! and does not link or embed any OpenOCD code.
#![forbid(unsafe_code)]
