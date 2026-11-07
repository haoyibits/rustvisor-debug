//! `rvdbg`: rustvisor-specific debugging front end. It reads `rvdbg.toml`
//! and runs each command on the backend of the connected probe: TRACE32
//! through tracebridge, or StellarLINK through OpenOCD (stellarbridge).
//!
//! The TRACE32 IDE files and the PowerView toolbar run
//! `<exe> --config <file> adapter|flash|load`, the way tracebridge's do, so
//! rvdbg accepts the same command names; `adapter` is always TRACE32.
#![forbid(unsafe_code)]

use rustvisor_debug::config;
mod fdx_terminal;
mod init;
mod inspect;
mod openocd;
mod probe;
mod session;
mod skill;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::{Parser, Subcommand, ValueEnum};
use probecore::style::Style;
use tracebridge::backend::Project;

use config::{Backend, Config};

/// Exit code when `flash` wrote the images but the comparison found
/// differences (the same code as a failed check).
const EXIT_FAILED: u8 = 3;

#[derive(Debug, Parser)]
#[command(
    name = "rvdbg",
    version,
    about = "Flash and debug rustvisor on its board (TRACE32 or StellarLINK)"
)]
struct Cli {
    /// Project configuration (default: the nearest rvdbg.toml)
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Use this backend instead of the one of the connected probe
    #[arg(long, global = true, value_enum)]
    backend: Option<BackendArg>,

    /// Machine-readable JSON output of the target commands (status, reg, mem, fault, check, clock, ...)
    #[arg(long, global = true)]
    json: bool,

    /// Without a command, an interactive session starts
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum BackendArg {
    /// Lauterbach TRACE32 PowerView
    Trace32,
    /// StellarLINK through openocd-stellar
    Openocd,
}

impl From<BackendArg> for Backend {
    fn from(argument: BackendArg) -> Backend {
        match argument {
            BackendArg::Trace32 => Backend::Trace32,
            BackendArg::Openocd => Backend::OpenOcd,
        }
    }
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Write rvdbg.toml with the SR6P6 defaults in the current directory
    Init,
    /// Show the configuration, the connected probes and the backend they select
    Config,
    /// Flash the ELF and the extra images, then run
    Flash {
        /// OpenOCD: do not compare the written segments with flash
        #[arg(long)]
        no_verify: bool,
        /// OpenOCD: leave the core halted after flashing instead of `reset run`
        #[arg(long)]
        no_run: bool,
        /// OpenOCD: write bytes of one-time-programmable areas (openocd.otp)
        /// that differ from the board; this changes the board permanently
        #[arg(long)]
        write_otp: bool,
        /// TRACE32: choose the flash script for this chip (overrides flash.chip and target.cpu)
        #[arg(long, conflicts_with = "script")]
        chip: Option<String>,
        /// TRACE32: use this flash script ("~~/..." is a TRACE32 path)
        #[arg(long, value_name = "PATH")]
        script: Option<String>,
    },
    /// TRACE32: start PowerView for the project, or reuse a running one
    Open,
    /// TRACE32: the flash scripts for a chip (your library and the TRACE32 installation)
    Chips {
        /// Chip name or part of it (default: flash.chip or target.cpu)
        query: Option<String>,
    },
    /// Install the AI agent skill for rvdbg (Claude Code)
    Skill {
        /// Skills directory (default: $CLAUDE_CONFIG_DIR/skills or ~/.claude/skills)
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
    },
    /// TRACE32: load the symbols without flashing or resetting
    Load,
    /// Run the debug server in the foreground for the IDE: TRACE32's DAP
    /// adapter, or OpenOCD's GDB server
    Debug {
        /// OpenOCD: keep serving after GDB detaches (until Ctrl-C)
        #[arg(long)]
        keep: bool,
    },
    /// TRACE32's DAP adapter (what the TRACE32 IDE configurations run)
    #[command(hide = true)]
    Adapter,
    /// Write the debug configurations of both backends into .vscode/
    Vscode,
    /// Run state of the core; PC and CPSR when it is halted
    Status,
    /// Stop the core
    Halt,
    /// Let the core run
    Go,
    /// AArch32 Hyp fault report: vector, HSR decoded, HDFAR/HIFAR, ELR_hyp, SPSR_hyp
    Fault {
        /// TRACE32: also list the newest trace records (default 32)
        #[arg(long, value_name = "RECORDS", num_args = 0..=1, default_missing_value = "32")]
        trace: Option<u32>,
    },
    /// TRACE32: registers by PER name (HSR, HSCTLR.C), full PER path, or raw address
    Reg {
        #[arg(required = true, value_name = "NAME|ADDRESS")]
        names: Vec<String>,
    },
    /// 32-bit words at an address (AD:0x60000000, 0x60000000) or a symbol
    Mem {
        #[arg(value_name = "ADDRESS|SYMBOL")]
        location: String,
        /// Number of 32-bit words
        #[arg(default_value_t = 1)]
        count: u32,
    },
    /// TRACE32: the newest trace records, with cycle counts and symbols
    Trace {
        /// Number of records, newest last
        #[arg(long, default_value_t = 100, value_name = "RECORDS")]
        last: u32,
    },
    /// TRACE32: print the value of a PRACTICE expression
    Eval {
        #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
        expression: Vec<String>,
    },
    /// TRACE32: execute a PRACTICE command (can do anything, including reset and flash)
    Cmd {
        #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// TRACE32: open a PER.Watch window with these registers (names, or a file with one per line)
    Watch {
        #[arg(required = true, value_name = "FILE|NAME")]
        items: Vec<String>,
    },
    /// Connect to the target without resetting it
    Attach,
    /// Reset the chip and halt at the reset vector
    Reset,
    /// TRACE32: SYStem.Down
    Down,
    /// Run a halted core until it reaches a symbol or address (hardware breakpoint)
    RunUntil {
        /// Seconds to wait (default: trace32.operation_timeout, openocd.timeout)
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<u64>,
        /// A symbol, symbol+offset, or an address
        #[arg(value_name = "SYMBOL|ADDRESS")]
        target: String,
    },
    /// TRACE32: run a PRACTICE script (DO) and wait until it ends
    Do {
        /// Seconds to wait (default: trace32.operation_timeout)
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<u64>,
        /// The script: a path relative to the current directory, or "~~/..." in TRACE32
        script: String,
        /// Arguments for the script
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
        arguments: Vec<String>,
    },
    /// Compare target memory with the images at their load addresses
    Verify {
        /// ELF file (default: the ELF and the extra images)
        elf: Option<PathBuf>,
        /// TRACE32: also run TRACE32's own comparison (Data.LOAD.Elf /DIFF)
        #[arg(long)]
        t32: bool,
    },
    /// Run a check file (registers, memory, clocks against expectations)
    Check {
        file: PathBuf,
        /// Run the checks listed for this variant
        #[arg(long)]
        variant: Option<String>,
        /// Stop the core when checks read core or coprocessor registers
        #[arg(long)]
        halt: bool,
        /// Only resolve names and symbols; read nothing
        #[arg(long, conflicts_with = "halt")]
        dry_run: bool,
    },
    /// Clock tree with frequencies, computed from the clock registers
    Clock {
        /// Frequencies only the board knows, e.g. XOSC=40MHz (default: [clock] in rvdbg.toml)
        #[arg(value_name = "CLOCK=FREQUENCY")]
        inputs: Vec<String>,
        /// Clock tree description (default: chosen by chip from ~/.config/rvdbg/clock)
        #[arg(long, value_name = "FILE")]
        tree: Option<PathBuf>,
        /// TRACE32: also write the tree as a diagram to .tracebridge/clock.html
        #[arg(long)]
        html: bool,
    },
    /// SEGGER RTT terminal on channel 0 while the core runs (Ctrl-C to stop)
    Rtt {
        /// Control-block symbol (default: rtt.symbol, _SEGGER_RTT)
        #[arg(long)]
        symbol: Option<String>,
        /// Control-block address; skips the symbol lookup, e.g. 0x60000000
        #[arg(long = "cb", value_parser = parse_address)]
        control_block: Option<u64>,
        /// Poll period in seconds (default: rtt.poll_interval, 0.02)
        #[arg(long)]
        poll: Option<f64>,
        /// Set the up-channel read offset to zero first (show what is buffered)
        #[arg(long)]
        replay: bool,
        /// Do not forward terminal input to the target
        #[arg(long)]
        output_only: bool,
        /// TRACE32: symbol program name (default: project.program)
        #[arg(long)]
        program: Option<String>,
        /// TRACE32: Remote API host
        #[arg(long)]
        node: Option<String>,
        /// TRACE32: Remote API port (default: trace32.rcl_port)
        #[arg(long)]
        port: Option<u16>,
    },
    /// FDX link of an image while the core runs: each line typed is a packet to the target, each packet from it a line (Ctrl-C to stop)
    Fdx(FdxArgs),
    /// Words of ordinary RAM in bulk while the cores run (TRACE32 AD:, the OpenOCD MEM-AP); never MMIO or coprocessor registers, use mem for those
    Ram {
        /// An address, or a symbol of the images
        #[arg(value_name = "ADDRESS|SYMBOL")]
        location: String,
        /// Number of 32-bit words to read
        #[arg(default_value_t = 1, conflicts_with = "write")]
        count: u32,
        /// Write these 32-bit words from the address on, one access each, instead of reading
        #[arg(long, value_name = "VALUE", num_args = 1.., value_parser = parse_word)]
        write: Vec<u32>,
    },
    /// Write the RustRover run configuration for TRACE32 (.run/, LSP4IJ)
    Rustrover,
}

/// The options of `rvdbg fdx`.
#[derive(Debug, clap::Args)]
struct FdxArgs {
    /// The n-th extra image (project.extra_elf, from 0) instead of the ELF
    #[arg(long, value_name = "N")]
    guest: Option<usize>,
    /// Link symbol (default: fdx.symbol, RUSTVISOR_FDX)
    #[arg(long)]
    symbol: Option<String>,
    /// Link address; skips the symbol lookup, e.g. 0x60000CC0
    #[arg(long, value_parser = parse_address)]
    address: Option<u64>,
    /// Send this packet, then exit; repeat it to send several, in order
    #[arg(long, value_name = "TEXT")]
    send: Vec<String>,
    /// Then wait for this packet; repeat it to wait for several, in order (exit 3 when one does not come)
    #[arg(long, value_name = "TEXT")]
    expect: Vec<String>,
    /// With --send/--expect: seconds to wait for the link to open, for room and for each expected packet
    #[arg(long, value_name = "SECONDS", default_value_t = 10.0)]
    timeout: f64,
    /// Mark the link closed and exit (while the board is held in reset: RAM keeps the last run's open link)
    #[arg(long, conflicts_with_all = ["send", "expect"])]
    close: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            eprintln!("{} {message}", Style::stderr().bad("rvdbg:"));
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> Result<u8, String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let flag = cli.backend.map(Backend::from);
    let Some(command) = cli.command else {
        return session::run(cli.config.as_deref(), &cwd, flag, cli.json);
    };
    if let Commands::Init = command {
        return init_command(&cwd);
    }
    if let Commands::Skill { dir } = &command {
        return install_skill(dir.as_deref());
    }
    let path = config::find(cli.config.as_deref(), &cwd)?;
    let config = config::load(&path)?;
    let mut env = Env {
        config: &config,
        flag,
        json: cli.json,
        session: None,
    };
    execute(&mut env, command)
}

fn install_skill(dir: Option<&Path>) -> Result<u8, String> {
    let path = skill::install(dir)?;
    println!("{} {}", Style::stdout().good("installed"), path.display());
    Ok(0)
}

/// Where a command runs: the configuration, the backend choice, and in an
/// interactive session the session's backend and OpenOCD.
pub struct Env<'a> {
    pub config: &'a Config,
    pub flag: Option<Backend>,
    pub json: bool,
    pub session: Option<SessionLink<'a>>,
}

/// What a session lends its commands.
pub struct SessionLink<'a> {
    pub backend: Backend,
    /// The session's OpenOCD (StellarLINK); `None` when it could not start.
    pub server: Option<&'a mut stellarbridge::Server>,
    /// Set by Ctrl-C while a command runs.
    pub interrupted: &'a AtomicBool,
    /// The session's PowerView connection (TRACE32), made on first use and
    /// dropped after a command fails.
    pub powerview: Option<&'a mut Option<tracebridge::backend::Session>>,
}

impl Env<'_> {
    /// The session's backend, else `--backend`, `project.backend` or the
    /// connected probe (said on stderr).
    fn backend(&self) -> Result<Backend, String> {
        match &self.session {
            Some(link) => Ok(link.backend),
            None => select(self.config, self.flag),
        }
    }

    /// Run `f` on the session's PowerView connection, or on one for this
    /// command. A failure drops the session's, so that the next command
    /// connects again; nothing is retried.
    fn with_powerview<T>(
        &mut self,
        f: impl FnOnce(&mut tracebridge::backend::Session) -> Result<T, String>,
    ) -> Result<T, String> {
        let project = trace32(self.config)?;
        let connect = |project| tracebridge::backend::Session::connect(project).map_err(text);
        match self
            .session
            .as_mut()
            .and_then(|link| link.powerview.as_deref_mut())
        {
            Some(slot) => {
                let session = match slot {
                    Some(session) => session,
                    None => slot.insert(connect(project)?),
                };
                let result = f(session);
                if result.is_err() {
                    *slot = None;
                }
                result
            }
            None => f(&mut connect(project)?),
        }
    }

    /// Forget the session's PowerView connection.
    fn drop_powerview(&mut self) {
        if let Some(slot) = self
            .session
            .as_mut()
            .and_then(|link| link.powerview.as_deref_mut())
        {
            *slot = None;
        }
    }

    /// Run `f` on the backend's access to RAM while the cores run (TRACE32
    /// `AD:`, the OpenOCD MEM-AP), with the probe's name.
    fn with_ram<T>(
        &mut self,
        f: impl FnOnce(&mut dyn probecore::Memory, &'static str) -> Result<T, String>,
    ) -> Result<T, String> {
        match self.backend()? {
            Backend::OpenOcd => {
                let memory_ap = self.config.openocd()?.memory_ap.clone().ok_or(
                    "RAM is read and written through openocd.memory_ap; set it in rvdbg.toml",
                )?;
                self.with_server(|server| {
                    f(
                        &mut stellarbridge::core::ApMemory { server, memory_ap },
                        "StellarLINK",
                    )
                })
            }
            Backend::Trace32 => self.with_powerview(|session| f(&mut session.memory(), "TRACE32")),
        }
    }

    /// Run `f` with the session's OpenOCD, or with one for this command: the
    /// running one (e.g. of `rvdbg debug`), else a new one.
    fn with_server<T>(
        &mut self,
        f: impl FnOnce(&mut stellarbridge::Server) -> Result<T, String>,
    ) -> Result<T, String> {
        match &mut self.session {
            Some(link) => match link.server.as_deref_mut() {
                Some(server) => f(server),
                None => Err("OpenOCD is not running in this session; 'reconnect' starts it".into()),
            },
            None => {
                let mut server = inspect::connect(self.config)?;
                f(&mut server)
            }
        }
    }
}

/// One command, outside or inside a session.
fn execute(env: &mut Env, command: Commands) -> Result<u8, String> {
    let config = env.config;
    let flag = env.flag;
    match command {
        Commands::Init | Commands::Skill { .. } => unreachable!("handled before the configuration"),
        Commands::Config => show_config(config, flag),
        Commands::Flash {
            no_verify,
            no_run,
            write_otp,
            chip,
            script,
        } => {
            check_images(config)?;
            match env.backend()? {
                Backend::OpenOcd => {
                    if chip.is_some() || script.is_some() {
                        return Err(
                            "--chip and --script choose a TRACE32 flash script; OpenOCD's flash \
                             driver comes from openocd.config"
                                .into(),
                        );
                    }
                    env.with_server(|server| {
                        openocd::flash(config, server, !no_verify, !no_run, write_otp)
                    })
                }
                Backend::Trace32 => {
                    for (set, option) in [
                        (no_verify, "--no-verify"),
                        (no_run, "--no-run"),
                        (write_otp, "--write-otp"),
                    ] {
                        if set {
                            return Err(format!("{option} applies to the openocd backend only"));
                        }
                    }
                    trace32(config)?.flash_with(chip, script).map_err(text)?;
                    Ok(0)
                }
            }
        }
        Commands::Load => match env.backend()? {
            Backend::Trace32 => {
                trace32(config)?.load_symbols().map_err(text)?;
                Ok(0)
            }
            Backend::OpenOcd => Err(
                "'load' is for TRACE32; with OpenOCD the debugger (GDB) reads the symbols \
                 from the ELF itself"
                    .into(),
            ),
        },
        Commands::Debug { keep } => match env.backend()? {
            Backend::OpenOcd => openocd::debug(config, keep),
            Backend::Trace32 => {
                if keep {
                    return Err("--keep applies to the openocd backend only".into());
                }
                trace32_debug(config)
            }
        },
        Commands::Adapter => {
            if flag == Some(Backend::OpenOcd) {
                return Err("'adapter' is TRACE32's DAP adapter; use 'rvdbg debug'".into());
            }
            trace32_debug(config)
        }
        Commands::Vscode => {
            let exe = current_exe()?;
            // Files the TRACE32 step creates now are not backed up again
            // when the OpenOCD entries are merged into them.
            let directory = config.root.join(".vscode");
            let created: Vec<PathBuf> = ["tasks.json", "launch.json"]
                .iter()
                .map(|name| directory.join(name))
                .filter(|path| !path.exists())
                .collect();
            trace32(config)?.install_vscode(&exe).map_err(text)?;
            if config.openocd.is_some() {
                openocd::vscode(config, &exe, &created)?;
            }
            Ok(0)
        }
        Commands::Rustrover => {
            trace32(config)?
                .install_rustrover(&current_exe()?)
                .map_err(text)?;
            Ok(0)
        }
        Commands::Open => match env.backend()? {
            Backend::Trace32 => {
                trace32(config)?.open().map_err(text)?;
                Ok(0)
            }
            Backend::OpenOcd => Err(
                "with StellarLINK there is nothing to open: each command starts OpenOCD, or uses \
                 the one 'rvdbg debug' runs"
                    .into(),
            ),
        },
        Commands::Chips { query } => {
            trace32(config)?.chips(query.as_deref()).map_err(text)?;
            Ok(0)
        }
        Commands::Rtt {
            symbol,
            control_block,
            poll,
            replay,
            output_only,
            program,
            node,
            port,
        } => match env.backend()? {
            Backend::OpenOcd => {
                for (set, option) in [
                    (program.is_some(), "--program"),
                    (node.is_some(), "--node"),
                    (port.is_some(), "--port"),
                ] {
                    if set {
                        return Err(format!("{option} applies to the trace32 backend only"));
                    }
                }
                let project = trace32(config)?;
                env.with_server(|server| {
                    inspect::rtt(
                        config,
                        &project,
                        server,
                        inspect::RttOptions {
                            symbol,
                            control_block,
                            poll,
                            replay,
                            output_only,
                        },
                    )
                })
            }
            Backend::Trace32 => {
                let mut args = Vec::new();
                for (option, value) in [
                    ("--program", program),
                    ("--node", node),
                    ("--port", port.map(|port| port.to_string())),
                ] {
                    if let Some(value) = value {
                        args.extend([option.to_string(), value]);
                    }
                }
                if let Some(symbol) = symbol {
                    args.extend(["--symbol".to_string(), symbol]);
                }
                if let Some(address) = control_block {
                    args.extend(["--cb".to_string(), format!("0x{address:X}")]);
                }
                if let Some(poll) = poll {
                    args.extend(["--poll".to_string(), poll.to_string()]);
                }
                if replay {
                    args.push("--replay".into());
                }
                if output_only {
                    args.push("--output-only".into());
                }
                trace32(config)?.rtt(&args).map_err(text)?;
                Ok(0)
            }
        },
        Commands::Fdx(args) => fdx(env, args),
        Commands::Ram {
            location,
            count,
            write,
        } => ram(env, location, count, write),
        command => inspect_command(env, command),
    }
}

/// status, halt, go, fault, verify, check, clock: tracebridge's `debug`
/// commands on TRACE32, the same commands through OpenOCD on StellarLINK.
fn inspect_command(env: &mut Env, command: Commands) -> Result<u8, String> {
    let config = env.config;
    let json = env.json;
    let openocd_timeout = config
        .openocd
        .as_ref()
        .map_or(600, |openocd| openocd.timeout);
    let (request, mut args) = plan(command, openocd_timeout);
    let project = trace32(config)?;
    match env.backend()? {
        Backend::Trace32 => {
            if json {
                args.insert(0, "--json".into());
            }
            let code = if env.session.is_some() {
                let code = env.with_powerview(|session| session.debug(args).map_err(text))?;
                if code == 1 {
                    // The error may have been the connection's.
                    env.drop_powerview();
                }
                code
            } else {
                project.debug(args).map_err(text)?
            };
            Ok(u8::try_from(code).unwrap_or(1))
        }
        Backend::OpenOcd => {
            let request = request?;
            let name = request.name();
            let interrupted = env.session.as_ref().map(|link| link.interrupted);
            let cancel = || interrupted.is_some_and(|flag| flag.load(Ordering::SeqCst));
            env.with_server(|server| {
                inspect::emit(
                    name,
                    json,
                    inspect::run(config, &project, server, request, &cancel),
                )
            })
        }
    }
}

/// A command as the OpenOCD request (an error when it is TRACE32 only) and
/// as the arguments of tracebridge's `debug`. `openocd_timeout`: the default
/// seconds of `run-until` with OpenOCD.
fn plan(
    command: Commands,
    openocd_timeout: u64,
) -> (Result<inspect::Request, String>, Vec<String>) {
    match command {
        Commands::Status => (Ok(inspect::Request::Status), vec!["status".to_string()]),
        Commands::Halt => (Ok(inspect::Request::Halt), vec!["break".into()]),
        Commands::Go => (Ok(inspect::Request::Go), vec!["go".into()]),
        Commands::Attach => (Ok(inspect::Request::Attach), vec!["attach".into()]),
        Commands::Reset => (Ok(inspect::Request::Reset), vec!["reset".into()]),
        Commands::Down => (Err(trace32_only("down")), vec!["down".into()]),
        Commands::Fault { trace } => {
            let mut args = vec!["fault".to_string()];
            let request = match trace {
                Some(records) => {
                    args.extend(["--trace".to_string(), records.to_string()]);
                    Err(trace32_only("fault --trace"))
                }
                None => Ok(inspect::Request::Fault),
            };
            (request, args)
        }
        Commands::Reg { names } => {
            let mut args = vec!["reg".to_string()];
            args.extend(names);
            (
                Err(trace32_only(
                    "reg (it reads registers by their PER names; use 'mem C15:0x...' or a \
                     check file with read.addr)",
                )),
                args,
            )
        }
        Commands::Mem { location, count } => {
            let args = vec!["mem".to_string(), location.clone(), count.to_string()];
            (Ok(inspect::Request::Mem { location, count }), args)
        }
        Commands::Trace { last } => (
            Err(trace32_only("trace")),
            vec!["trace".into(), "--last".into(), last.to_string()],
        ),
        Commands::Eval { expression } => {
            let mut args = vec!["eval".to_string()];
            args.extend(expression);
            (Err(trace32_only("eval (PRACTICE)")), args)
        }
        Commands::Cmd { command } => {
            let mut args = vec!["cmd".to_string()];
            args.extend(command);
            (Err(trace32_only("cmd (PRACTICE)")), args)
        }
        Commands::Watch { items } => {
            let mut args = vec!["watch".to_string()];
            args.extend(items);
            (Err(trace32_only("watch (a PowerView window)")), args)
        }
        Commands::Do {
            timeout,
            script,
            arguments,
        } => {
            let mut args = vec!["do".to_string()];
            if let Some(timeout) = timeout {
                args.extend(["--timeout".to_string(), timeout.to_string()]);
            }
            args.push(script);
            args.extend(arguments);
            (Err(trace32_only("do (PRACTICE scripts)")), args)
        }
        Commands::RunUntil { timeout, target } => {
            let mut args = vec!["run-until".to_string()];
            if let Some(timeout) = timeout {
                args.extend(["--timeout".to_string(), timeout.to_string()]);
            }
            args.push(target.clone());
            let seconds = timeout.unwrap_or(openocd_timeout);
            (
                Ok(inspect::Request::RunUntil {
                    target,
                    timeout: std::time::Duration::from_secs(seconds),
                }),
                args,
            )
        }
        Commands::Verify { elf, t32 } => {
            let mut args = vec!["verify".to_string()];
            args.extend(elf.iter().map(|path| path.display().to_string()));
            if t32 {
                args.push("--t32".into());
            }
            let request = if t32 {
                Err(trace32_only("verify --t32"))
            } else {
                Ok(inspect::Request::Verify(elf))
            };
            (request, args)
        }
        Commands::Check {
            file,
            variant,
            halt,
            dry_run,
        } => {
            let mut args = vec!["check".to_string(), file.display().to_string()];
            if let Some(variant) = &variant {
                args.extend(["--variant".to_string(), variant.clone()]);
            }
            if halt {
                args.push("--halt".into());
            }
            if dry_run {
                args.push("--dry-run".into());
            }
            let options = probecore::check::Options {
                variant,
                halt,
                dry_run,
            };
            (Ok(inspect::Request::Check { file, options }), args)
        }
        Commands::Clock { inputs, tree, html } => {
            let mut args = vec!["clock".to_string()];
            args.extend(inputs.iter().cloned());
            if let Some(tree) = &tree {
                args.extend(["--tree".to_string(), tree.display().to_string()]);
            }
            if html {
                args.push("--html".into());
                return (Err(trace32_only("clock --html")), args);
            }
            (Ok(inspect::Request::Clock { inputs, tree }), args)
        }
        _ => unreachable!("handled in run"),
    }
}

fn trace32_only(what: &str) -> String {
    format!("{what} is available with TRACE32 only (--backend trace32)")
}

/// `rvdbg fdx`: the link's address, then the terminal on the backend's
/// memory access.
fn fdx(env: &mut Env, args: FdxArgs) -> Result<u8, String> {
    let FdxArgs {
        guest,
        symbol,
        address,
        send,
        expect,
        timeout,
        close,
    } = args;
    let config = env.config;
    if !(timeout > 0.0 && timeout.is_finite()) {
        return Err("--timeout must be positive".into());
    }
    let image = match guest {
        None => config.elf.clone(),
        Some(n) => config.extra_elf.get(n).cloned().ok_or_else(|| {
            format!(
                "--guest {n}: project.extra_elf has {} image(s)",
                config.extra_elf.len()
            )
        })?,
    };
    let symbol = symbol.unwrap_or_else(|| config.fdx_symbol.clone());
    let address = match address {
        Some(address) => address,
        None => rustvisor_debug::fdx::symbol_address(&image, &symbol)
            .map_err(|error| format!("{error}; build the images, or pass --address 0x<address>"))?,
    };
    let options = fdx_terminal::Options {
        symbol,
        address,
        send,
        expect,
        timeout: std::time::Duration::from_secs_f64(timeout),
        json: env.json,
    };
    // Ctrl-C: the session's flag, else one for this command.
    let own = std::sync::Arc::new(AtomicBool::new(false));
    let interrupted: &AtomicBool = match env.session.as_ref().map(|link| link.interrupted) {
        Some(flag) => flag,
        None => {
            #[cfg(unix)]
            signal_hook::flag::register(signal_hook::consts::SIGINT, own.clone())
                .map_err(|error| format!("cannot handle Ctrl-C: {error}"))?;
            &own
        }
    };
    env.with_ram(|mut memory, probe| {
        if close {
            rustvisor_debug::fdx::Link::close(&mut memory, address).map_err(text)?;
            if !options.json {
                println!("closed {} at 0x{address:08X}", options.symbol);
            }
            return Ok(0);
        }
        fdx_terminal::run(&mut memory, probe, &options, interrupted)
    })
}

/// `rvdbg ram`: words of RAM in bulk while the cores run.
fn ram(env: &mut Env, location: String, count: u32, write: Vec<u32>) -> Result<u8, String> {
    let config = env.config;
    let json = env.json;
    let (address, symbol) = match probecore::text::parse_int_auto(&location)
        .and_then(|value| u64::try_from(value).ok())
    {
        Some(address) => (address, None),
        None => {
            let address = inspect::symbols(config)
                .range(&location)
                .map(|(start, _)| start)
                .ok_or_else(|| {
                    format!("{location} is neither an address nor a symbol of the images")
                })?;
            (address, Some(location))
        }
    };
    if address % 4 != 0 {
        return Err(format!("0x{address:X} is not word-aligned"));
    }
    let result = env.with_ram(|memory, _| {
        if !write.is_empty() {
            for (index, value) in write.iter().enumerate() {
                memory
                    .write_u32(address + 4 * index as u64, *value)
                    .map_err(|error| format!("cannot write 0x{address:08X}: {error}"))?;
            }
            return Ok(inspect::Outcome {
                text: format!("wrote {} word(s) at 0x{address:08X}", write.len()),
                json: serde_json::json!({
                    "symbol": symbol,
                    "address": format!("0x{address:08X}"),
                    "written": write.len(),
                }),
                code: 0,
            });
        }
        if count == 0 || count > RAM_MAX_WORDS {
            return Err(format!("count must be between 1 and {RAM_MAX_WORDS}"));
        }
        let words = memory
            .read_u32s(address, count as usize)
            .map_err(|error| format!("cannot read 0x{address:08X}: {error}"))?;
        let text = words
            .chunks(4)
            .enumerate()
            .map(|(row, chunk)| {
                let values: Vec<String> = chunk.iter().map(|w| format!("{w:08X}")).collect();
                format!("0x{:08X}  {}", address + 16 * row as u64, values.join(" "))
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(inspect::Outcome {
            text,
            json: serde_json::json!({
                "symbol": symbol,
                "address": format!("0x{address:08X}"),
                "words": words,
                "hex": words.iter().map(|w| format!("0x{w:08X}")).collect::<Vec<_>>(),
            }),
            code: 0,
        })
    });
    inspect::emit("ram", json, result)
}

/// The most words one `rvdbg ram` reads: 4 MiB.
const RAM_MAX_WORDS: u32 = 1 << 20;

fn parse_word(text: &str) -> Result<u32, String> {
    probecore::text::parse_int_auto(text)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| format!("invalid 32-bit value: {text}"))
}

fn parse_address(text: &str) -> Result<u64, String> {
    probecore::text::parse_int_auto(text)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| format!("invalid address: {text}"))
}

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// `--backend`, else `project.backend`, with where it came from.
pub(crate) fn fixed(config: &Config, flag: Option<Backend>) -> Option<(Backend, &'static str)> {
    match (flag, config.backend) {
        (Some(backend), _) => Some((backend, "--backend")),
        (None, Some(backend)) => Some((backend, "project.backend in rvdbg.toml")),
        (None, None) => None,
    }
}

/// The backend for a command: `--backend`, else `project.backend`, else
/// the connected probe. Says which on stderr.
fn select(config: &Config, flag: Option<Backend>) -> Result<Backend, String> {
    let fixed = fixed(config, flag);
    let found = match fixed {
        Some(_) => Vec::new(),
        None => probe::detect(config)?,
    };
    let choice = probe::choose(fixed, &found)?;
    eprintln!(
        "{} {}",
        Style::stderr().label("[rvdbg] backend"),
        choice.reason
    );
    Ok(choice.backend)
}

/// PowerView (started if needed), then the DAP adapter.
fn trace32_debug(config: &Config) -> Result<u8, String> {
    let project = trace32(config)?;
    project.open().map_err(text)?;
    project.run_adapter().map_err(text)?;
    Ok(0)
}

fn show_config(config: &Config, flag: Option<Backend>) -> Result<u8, String> {
    let style = Style::stdout();
    println!("{} {}", style.label("config "), config.file.display());
    for image in config.images() {
        let status = if image.is_file() {
            style.good("ok")
        } else {
            style.bad("MISSING")
        };
        println!("{} {status} {}", style.label("image  "), image.display());
    }
    if let Some(openocd) = &config.openocd {
        println!(
            "{} {} tcl {} gdb {} ({})",
            style.label("openocd"),
            openocd.executable.display(),
            openocd.tcl_port,
            openocd.gdb_port,
            openocd.gdb_target.as_deref().unwrap_or("every target")
        );
    }
    println!("{} rcl {}", style.label("trace32"), probe::rcl_port(config));
    let found = probe::detect(config)?;
    if found.is_empty() {
        println!("{} none", style.label("probe  "));
    }
    for item in &found {
        println!("{} {item}", style.label("probe  "));
    }
    let fixed = fixed(config, flag);
    match probe::choose(fixed, &found) {
        Ok(choice) => println!("{} {}", style.label("backend"), style.good(choice.reason)),
        Err(error) => println!("{} {}", style.label("backend"), style.warn(error)),
    }
    Ok(0)
}

fn init_command(cwd: &Path) -> Result<u8, String> {
    let path = init::init(cwd)?;
    let style = Style::stdout();
    println!("{} {}", style.good("wrote"), path.display());
    println!(
        "{}",
        style.dim("  the backend follows the connected probe (StellarLINK or Lauterbach)")
    );
    println!("\nNext:");
    for (command, note) in [
        ("rvdbg config", "shows the probe it found"),
        ("rvdbg flash", "build the images first"),
        ("rvdbg vscode", "then F5 in VS Code"),
        ("rvdbg debug", "or run the debug server yourself"),
    ] {
        println!(
            "  {}  {}",
            style.label(format!("{command:<13}")),
            style.dim(format!("# {note}"))
        );
    }
    Ok(0)
}

/// The TRACE32 backend with rvdbg.toml's settings.
fn trace32(config: &Config) -> Result<Project, String> {
    config.trace32_project()
}

fn check_images(config: &Config) -> Result<(), String> {
    for image in config.images() {
        if !image.is_file() {
            return Err(format!(
                "{} does not exist; build it first",
                image.display()
            ));
        }
    }
    Ok(())
}

/// This program, for the IDE files: a GUI-started IDE may not have the
/// user's PATH. A Homebrew install is named by its `opt` link, which
/// `brew upgrade` keeps, not by the versioned Cellar directory.
fn current_exe() -> Result<PathBuf, String> {
    let exe =
        std::env::current_exe().map_err(|error| format!("cannot find rvdbg itself: {error}"))?;
    let exe = exe.canonicalize().unwrap_or(exe);
    Ok(tracebridge::backend::homebrew_stable_path(&exe)
        .filter(|path| path.is_file())
        .unwrap_or(exe))
}

/// The run directory with a `.gitignore` that ignores everything in it.
pub(crate) fn run_dir(config: &Config) -> Result<PathBuf, String> {
    let dir = config.run_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        std::fs::write(&ignore, "*\n")
            .map_err(|error| format!("cannot write {}: {error}", ignore.display()))?;
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether OpenOCD runs the command, and tracebridge's `debug` arguments.
    fn planned(line: &[&str]) -> (bool, Vec<String>) {
        let cli = Cli::try_parse_from(std::iter::once("rvdbg").chain(line.iter().copied()))
            .unwrap_or_else(|error| panic!("{line:?}: {error}"));
        let (request, args) = plan(cli.command.expect("a command"), 600);
        (request.is_ok(), args)
    }

    #[test]
    fn commands_forward_to_tracebridge_debug() {
        let cases: &[(&[&str], bool, &[&str])] = &[
            (&["halt"], true, &["break"]),
            (&["status"], true, &["status"]),
            (&["reset"], true, &["reset"]),
            (&["attach"], true, &["attach"]),
            (&["down"], false, &["down"]),
            (
                &["mem", "_SEGGER_RTT", "4"],
                true,
                &["mem", "_SEGGER_RTT", "4"],
            ),
            (
                &["reg", "HSR", "HSCTLR.C"],
                false,
                &["reg", "HSR", "HSCTLR.C"],
            ),
            (
                &["eval", "Register(PC)", "+", "4"],
                false,
                &["eval", "Register(PC)", "+", "4"],
            ),
            (&["cmd", "Break.List"], false, &["cmd", "Break.List"]),
            (&["trace", "--last", "8"], false, &["trace", "--last", "8"]),
            (&["fault"], true, &["fault"]),
            (&["fault", "--trace"], false, &["fault", "--trace", "32"]),
            (
                &["verify", "a.elf", "--t32"],
                false,
                &["verify", "a.elf", "--t32"],
            ),
            (&["verify"], true, &["verify"]),
            (
                &["check", "c.toml", "--variant", "s5", "--halt"],
                true,
                &["check", "c.toml", "--variant", "s5", "--halt"],
            ),
            (&["clock", "XOSC=40MHz"], true, &["clock", "XOSC=40MHz"]),
            (&["clock", "--html"], false, &["clock", "--html"]),
            (
                &["run-until", "--timeout", "5", "guest_main"],
                true,
                &["run-until", "--timeout", "5", "guest_main"],
            ),
            (
                &["do", "--timeout", "9", "setup.cmm", "-x", "1"],
                false,
                &["do", "--timeout", "9", "setup.cmm", "-x", "1"],
            ),
        ];
        for (line, openocd, args) in cases {
            let expected: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            assert_eq!(planned(line), (*openocd, expected), "{line:?}");
        }
    }

    #[test]
    fn run_until_waits_for_the_openocd_timeout_by_default() {
        let cli = Cli::try_parse_from(["rvdbg", "run-until", "main"]).unwrap();
        match plan(cli.command.expect("a command"), 42).0 {
            Ok(inspect::Request::RunUntil { timeout, .. }) => {
                assert_eq!(timeout, std::time::Duration::from_secs(42))
            }
            _ => panic!("expected run-until"),
        }
    }
}
