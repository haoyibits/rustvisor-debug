//! Interactive SEGGER RTT terminal through a running PowerView (rtt.py).
//!
//! TRACE32 owns the debug probe. This process connects to TRACE32's Remote API,
//! drains RTT up-channel 0 to stdout and forwards stdin to down-channel 0, so
//! it carries both printf output and an interactive console. All memory access
//! uses the `E:` (run-time) access class.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::Parser;
use probecore::Memory;
use probecore::rtt::{DESC_RD_OFF, RttChannel, UP_DESCRIPTOR};
use t32rcl::{Address, Debugger};

use crate::config::Config;
use crate::errors::Result;
use crate::style::Style;
use crate::{bail, bridge_error};

/// RCL run-time memory access (`E:`), so the core keeps running.
struct RunTime<'a>(&'a mut Debugger);

fn run_time(address: u64) -> Address {
    Address::new(Some("E"), address)
}

fn lift<T>(result: t32rcl::Result<T>) -> probecore::Result<T> {
    result.map_err(|error| probecore::Error::new(error.to_string()))
}

impl Memory for RunTime<'_> {
    fn read(&mut self, address: u64, length: usize) -> probecore::Result<Vec<u8>> {
        lift(self.0.memory_read(&run_time(address), length))
    }
    fn write(&mut self, address: u64, data: &[u8]) -> probecore::Result<()> {
        lift(self.0.memory_write(&run_time(address), data))
    }
    fn write_u32(&mut self, address: u64, value: u32) -> probecore::Result<()> {
        lift(self.0.memory_write_u32(&run_time(address), value))
    }
}

fn parse_address(value: &str) -> std::result::Result<u64, String> {
    crate::pycompat::parse_int_auto(value)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| format!("invalid address: {value}"))
}

/// Options of `tracebridge rtt` (`add_arguments`); defaults come from trace32.toml.
#[derive(Debug, Parser)]
#[command(
    name = "tracebridge rtt",
    about = "Interactive SEGGER RTT terminal through PowerView"
)]
pub struct RttArgs {
    /// TRACE32 symbol program name (default: project.program)
    #[arg(long)]
    pub program: Option<String>,
    /// RTT control-block symbol (default: rtt.symbol, _SEGGER_RTT)
    #[arg(long)]
    pub symbol: Option<String>,
    /// Control-block address; bypasses the symbol lookup, e.g. 0x20000000
    #[arg(long = "cb", value_parser = parse_address)]
    pub control_block: Option<u64>,
    /// Remote API host
    #[arg(long, default_value = "localhost")]
    pub node: String,
    /// Remote API port (default: trace32.rcl_port)
    #[arg(long)]
    pub port: Option<u16>,
    /// Poll period in seconds (default: rtt.poll_interval, 0.02)
    #[arg(long)]
    pub poll: Option<f64>,
    /// Set the up-channel RdOff to zero before polling
    #[arg(long)]
    pub replay: bool,
    /// Do not forward terminal input to the RTT down-channel
    #[arg(long)]
    pub output_only: bool,
}

/// Parse the arguments after `rtt`; usage errors exit like any clap parser.
pub fn parse_args(forwarded: &[String]) -> RttArgs {
    let argv = std::iter::once("tracebridge rtt".to_string()).chain(forwarded.iter().cloned());
    RttArgs::parse_from(argv)
}

/// Character-at-a-time input without echo; Ctrl-C still raises SIGINT.
/// The previous settings are restored on drop, including during a panic.
struct TerminalGuard {
    #[cfg(unix)]
    original: Option<nix::sys::termios::Termios>,
}

impl TerminalGuard {
    fn enable(enabled: bool) -> TerminalGuard {
        #[cfg(unix)]
        {
            use nix::sys::termios::{
                LocalFlags, SetArg, SpecialCharacterIndices, tcgetattr, tcsetattr,
            };
            let stdin = std::io::stdin();
            if !enabled || !nix::unistd::isatty(&stdin).unwrap_or(false) {
                return TerminalGuard { original: None };
            }
            let Ok(original) = tcgetattr(&stdin) else {
                return TerminalGuard { original: None };
            };
            let mut modified = original.clone();
            modified
                .local_flags
                .remove(LocalFlags::ICANON | LocalFlags::ECHO);
            modified.control_chars[SpecialCharacterIndices::VMIN as usize] = 1;
            modified.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;
            if tcsetattr(&stdin, SetArg::TCSADRAIN, &modified).is_err() {
                return TerminalGuard { original: None };
            }
            TerminalGuard {
                original: Some(original),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = enabled;
            TerminalGuard {}
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(original) = &self.original {
            let _ = nix::sys::termios::tcsetattr(
                std::io::stdin(),
                nix::sys::termios::SetArg::TCSADRAIN,
                original,
            );
        }
    }
}

/// `read_terminal_input`: whatever is available right now, without blocking.
/// Embedded consoles usually treat BS as erase; most terminals send DEL.
fn read_terminal_input() -> Vec<u8> {
    #[cfg(unix)]
    {
        use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
        use std::os::fd::AsFd;
        let stdin = std::io::stdin();
        let mut fds = [PollFd::new(stdin.as_fd(), PollFlags::POLLIN)];
        if !matches!(poll(&mut fds, PollTimeout::ZERO), Ok(count) if count > 0) {
            return Vec::new();
        }
        let mut buffer = [0u8; 256];
        match nix::unistd::read(stdin.as_fd(), &mut buffer) {
            Ok(count) => buffer[..count]
                .iter()
                .map(|&b| if b == 0x7f { 0x08 } else { b })
                .collect(),
            Err(_) => Vec::new(),
        }
    }
    #[cfg(not(unix))]
    {
        Vec::new()
    }
}

/// `run`.
pub fn run(config: &Config, args: RttArgs) -> Result<()> {
    let port = args.port.unwrap_or(config.rcl_port);
    let program = args.program.unwrap_or_else(|| config.program.clone());
    let symbol = args.symbol.unwrap_or_else(|| config.rtt_symbol.clone());
    let control_block = args.control_block.or(config.rtt_control_block_address);
    let poll = args.poll.unwrap_or(config.rtt_poll_interval);
    if !(poll > 0.0 && poll.is_finite()) {
        bail!("--poll must be positive");
    }
    let poll = Duration::from_secs_f64(poll);

    let mut debugger = Debugger::connect(&args.node, port, Duration::from_secs(5))
        .and_then(|mut debugger| {
            debugger.print(&format!(
                "RTT terminal connected (pid {})",
                std::process::id()
            ))?;
            Ok(debugger)
        })
        .map_err(|error| {
            bridge_error!(
                "cannot connect to TRACE32 at {}:{port} ({error})\n\
                 Check the RCL=NETTCP / PORT= section of your config.t32.",
                args.node
            )
        })?;

    let control_block = match control_block {
        Some(address) => address,
        None if program.is_empty() => bail!("project.program is empty and no --cb was given"),
        None => debugger
            .symbol_address(&format!("\\\\{program}\\Global\\{symbol}"))
            .map_err(|error| {
                bridge_error!(
                    "cannot resolve {symbol} in '{program}' ({error})\n\
                     Run 'tracebridge load' first, or pass --cb 0x<address>."
                )
            })?,
    };

    let mut memory = RunTime(&mut debugger);
    terminal(
        &mut memory,
        &Terminal {
            probe: "TRACE32",
            symbol: &symbol,
            control_block,
            poll,
            replay: args.replay,
            output_only: args.output_only,
        },
    )
}

/// How [`terminal`] runs.
pub struct Terminal<'a> {
    /// The probe's name in the status lines (`TRACE32`, `StellarLINK`).
    pub probe: &'a str,
    /// The control block's symbol, for the status line.
    pub symbol: &'a str,
    pub control_block: u64,
    pub poll: Duration,
    /// Set the up-channel RdOff to zero before polling.
    pub replay: bool,
    /// Do not forward terminal input to the down-channel.
    pub output_only: bool,
}

/// The interactive RTT terminal on channel 0 until Ctrl-C, through any
/// memory access that works while the core runs.
pub fn terminal(memory: &mut impl Memory, options: &Terminal) -> Result<()> {
    let Terminal {
        probe,
        symbol,
        control_block,
        poll,
        ..
    } = *options;
    let mut channel = RttChannel::new(control_block);
    // The status lines on stderr; what the target prints is never touched.
    let style = Style::stderr();
    eprintln!(
        "{} {} @ {}; Ctrl-C to stop",
        style.label(format!("{probe} RTT:")),
        style.symbol(symbol),
        style.address(format!("0x{control_block:08X}"))
    );
    if !channel.refresh_state(memory).unwrap_or(false) {
        eprintln!(
            "{} waiting for the target to initialize SEGGER RTT; \
             if debugging is paused before RTT initialization, press Continue",
            style.warn("[rtt]")
        );
    }
    if options.replay {
        memory
            .write_u32(channel.cb(UP_DESCRIPTOR + DESC_RD_OFF), 0)
            .map_err(|error| bridge_error!("cannot reset the RTT read offset: {error}"))?;
    }

    let interrupted = std::sync::Arc::new(AtomicBool::new(false));
    crate::signals::flag_on_interrupt(&interrupted);
    let guard = TerminalGuard::enable(!options.output_only);
    let mut pending_input: Vec<u8> = Vec::new();
    let mut consecutive_errors = 0u64;
    let mut stdout = std::io::stdout();
    while !interrupted.load(Ordering::SeqCst) {
        let step = (|| -> probecore::Result<()> {
            let output = channel.read_up(memory)?;
            if !output.is_empty() {
                // Passed through byte for byte: escape sequences are the target's business.
                let _ = stdout.write_all(&output);
                let _ = stdout.flush();
            }
            if !options.output_only {
                pending_input.extend(read_terminal_input());
                if !pending_input.is_empty() {
                    let sent = channel.write_down(memory, &pending_input)?;
                    pending_input.drain(..sent);
                }
            }
            Ok(())
        })();
        match step {
            Ok(()) => {
                consecutive_errors = 0;
                std::thread::sleep(poll);
            }
            Err(error) if error.lost => {
                drop(guard);
                bail!("RTT: the connection to the probe is gone: {error}");
            }
            Err(error) => {
                consecutive_errors += 1;
                channel.initialized = false;
                if matches!(consecutive_errors, 5 | 50) || consecutive_errors % 300 == 0 {
                    eprintln!(
                        "\n{} run-time memory access failed x{consecutive_errors}: {error}",
                        style.warn("[rtt]")
                    );
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
    drop(guard);
    eprintln!("\n{}", style.label(format!("{probe} RTT terminal stopped")));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_default_to_the_configuration() {
        let args = parse_args(&["--cb".into(), "0x20000400".into(), "--replay".into()]);
        assert_eq!(args.control_block, Some(0x2000_0400));
        assert!(args.replay);
        assert_eq!(args.node, "localhost");
        assert_eq!(args.port, None);
        let error = RttArgs::try_parse_from(["tracebridge rtt", "--cb", "zz"]).unwrap_err();
        assert!(error.to_string().contains("invalid address: zz"));
        assert!(RttArgs::try_parse_from(["tracebridge rtt", "--protocol", "UDP"]).is_err());
    }
}
