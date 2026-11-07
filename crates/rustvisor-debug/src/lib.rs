//! rvdbg's conventions for other tools. rustvisor-sdk's board runner reads
//! the project's `rvdbg.toml` through this library and drives TRACE32 with
//! the tracebridge [`Project`](tracebridge::backend::Project) it yields, so
//! it uses the same keys, defaults and environment overrides as `rvdbg`.
//! [`fdx`] is the FDX link of rustvisor's images, over any memory access.
//!
//! ```no_run
//! let config = rustvisor_debug::config::load("rvdbg.toml".as_ref())?;
//! let project = config.trace32_project()?;
//! # Ok::<(), String>(())
//! ```
#![forbid(unsafe_code)]

pub mod config;
pub mod fdx;
