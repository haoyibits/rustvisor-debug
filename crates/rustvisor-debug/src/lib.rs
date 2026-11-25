//! rvdbg's conventions for other tools that link this crate. [`config`] reads
//! a project's `rvdbg.toml` and yields a tracebridge
//! [`Project`](tracebridge::backend::Project) with the same keys, defaults and
//! environment overrides as `rvdbg`. rustvisor-sdk's board runner does not
//! link this crate: it drives an installed rvdbg (0.1.24 or later, or `RVDBG`)
//! as an `rvdbg --json` pipe session.
//! [`fdx`] is the FDX link of rustvisor's images, over any memory access;
//! [`stop`] decodes the hypervisor's stop records.
//! [`swtrace`] decodes and pairs the software trace records (rustvisor-sdk
//! `debug/swtrace/FORMAT.md`) that `rvdbg swtrace` reads; [`timeline`] draws them
//! as a VM state timeline page (`rvdbg swtrace --html`).
//!
//! ```no_run
//! let config = rustvisor_debug::config::load("rvdbg.toml".as_ref())?;
//! let project = config.trace32_project()?;
//! # Ok::<(), String>(())
//! ```
#![forbid(unsafe_code)]

pub mod config;
pub mod fdx;
pub mod stop;
pub mod swtrace;
pub mod timeline;
