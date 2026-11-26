//! `rvdbg`: rustvisor-specific debugging front end. It reads `rvdbg.toml`
//! and runs each command on the backend of the connected probe: TRACE32
//! through tracebridge, or StellarLINK through OpenOCD (stellarbridge).
//!
//! The TRACE32 IDE files and the PowerView toolbar run
//! `<exe> --config <file> adapter|flash|load`, the way tracebridge's do, so
//! rvdbg accepts the same command names; `adapter` is always TRACE32.
#![forbid(unsafe_code)]

use probecore::output::{eprintln, println};

use rustvisor_debug::config;
mod fdx_terminal;
mod init;
mod inspect;
mod openocd;
mod probe;
mod session;
mod skill;
mod tui;

use std::io::IsTerminal;
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

#[derive(Debug, Clone, Copy, ValueEnum)]
enum TuiModeArg {
    /// Flicker-free alternate screen (default)
    #[value(alias = "alt", alias = "altscreen")]
    Fullscreen,
    /// Dynamic-height inline surface in terminal scrollback
    Inline,
}

impl From<TuiModeArg> for tui::ScreenMode {
    fn from(arg: TuiModeArg) -> Self {
        match arg {
            TuiModeArg::Fullscreen => tui::ScreenMode::Fullscreen,
            TuiModeArg::Inline => tui::ScreenMode::Inline,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "rvdbg",
    version,
    about = "Flash and debug rustvisor on its board (TRACE32 or StellarLINK)"
)]
struct Cli {
    /// Start the interactive terminal UI (`--tui`, `--tui=fullscreen`, or `--tui=inline`)
    #[arg(
        long,
        value_name = "MODE",
        num_args = 0..=1,
        default_missing_value = "fullscreen",
        require_equals = true,
        conflicts_with = "json"
    )]
    tui: Option<TuiModeArg>,

    /// Start the interactive session in inline scrollback mode instead of fullscreen
    #[arg(long, conflicts_with_all = ["json", "tui"])]
    inline: bool,

    /// Project configuration (default: the nearest rvdbg.toml)
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Use this backend instead of the one of the connected probe
    #[arg(long, global = true, value_enum)]
    backend: Option<BackendArg>,

    /// Machine-readable JSON output of the target commands (status, reg, mem, fault, check, clock, ...)
    #[arg(long, global = true)]
    json: bool,

    /// Use the images in this directory instead of rvdbg.toml's and ELF/EXTRA_ELF: `hypervisor` (or its only other ELF) and guest0.elf, guest1.elf, ... (a board-tests case directory)
    #[arg(long, global = true, value_name = "DIR")]
    images: Option<PathBuf>,

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
    /// Reset the chip and halt at the reset vector, then mark every image's FDX link closed
    Reset {
        /// Leave the FDX links as they are (RAM keeps the last run's open links)
        #[arg(long)]
        keep_links: bool,
        /// Start running immediately after resetting and closing links (same as `restart`)
        #[arg(long)]
        go: bool,
    },
    /// Reset the chip, close FDX links, and start running (shortcut for `reset --go`)
    Restart {
        /// Leave the FDX links as they are (RAM keeps the last run's open links)
        #[arg(long)]
        keep_links: bool,
    },
    /// The hypervisor's stop records and the images' FDX links, read while the cores run
    Cores,
    /// The images in use, where they come from, and their FDX links (in a session, 'images <dir>' switches to a directory's images)
    Images,
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
    /// FDX link of an image while the cores run: each line typed is a packet to the target, each packet from it a line; --all watches every image's link (Ctrl-C or :q to stop)
    Fdx(FdxArgs),
    /// Software trace records (rustvisor-swtrace) of the images while the cores run: drain each image's RUSTVISOR_TRACE channels, pair IPC sends with their receives, one-way latency per direction
    Swtrace(SwtraceArgs),
    /// Words of ordinary RAM in bulk while the cores run (TRACE32 AD:, the OpenOCD MEM-AP); never MMIO or coprocessor registers, use mem for those
    Ram {
        /// An address, `[image:]symbol[+offset]` (`0..2:MAILBOX+0x3c`), or `location=value`
        #[arg(value_name = "ADDRESS|SYMBOL")]
        location: String,
        /// Number of 32-bit words to read (default 1), or `= VALUE...` to write
        #[arg(value_name = "COUNT|= VALUE", num_args = 0..)]
        args: Vec<String>,
        /// Write these 32-bit words from the address on, one access each, instead of reading
        #[arg(long, value_name = "VALUE", num_args = 1.., value_parser = parse_word)]
        write: Vec<u32>,
    },
    /// Write the RustRover run configuration for TRACE32 (.run/, LSP4IJ)
    Rustrover,
}

/// The options of `rvdbg fdx`.
#[derive(Clone, Debug, clap::Args)]
struct FdxArgs {
    /// The image whose link to use: el2 (the ELF, the default), guest<n> or <n> (the n-th extra image)
    #[arg(long, value_name = "NAME|N")]
    guest: Option<String>,
    /// Every image's link at once (interactive), or with --close: close them all
    #[arg(long, conflicts_with_all = ["guest", "address", "send", "expect"])]
    all: bool,
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

/// The options of `rvdbg swtrace`.
impl FdxArgs {
    fn interactive(&self) -> bool {
        !self.close && self.send.is_empty() && self.expect.is_empty()
    }
}

#[derive(Debug, clap::Args)]
struct SwtraceArgs {
    /// Only these images: el2, guest<n>, <n>, or a range like 0..2; repeat for several (default: every image that has the symbol)
    #[arg(long, value_name = "NAME|N")]
    guest: Vec<String>,
    /// Channel symbol; SYMBOL_<suffix> are channels too (default: RUSTVISOR_TRACE)
    #[arg(long)]
    symbol: Option<String>,
    /// Also write every record, merged by time, to FILE as CSV
    #[arg(long, value_name = "FILE", conflicts_with = "from")]
    csv: Option<PathBuf>,
    /// Also draw the records as a VM state timeline page in FILE (self-contained HTML; defaults to .rvdbg/swtrace/latest.html)
    #[arg(long, value_name = "FILE", num_args = 0..=1, default_missing_value = "latest")]
    html: Option<PathBuf>,
    /// Generate the HTML timeline and open it in the default browser
    #[arg(long)]
    open: bool,
    /// Read the records from a CSV that --csv wrote (or 'latest' for the last board backup) instead of the board (no probe needed)
    #[arg(long, value_name = "CSV", conflicts_with = "guest")]
    from: Option<PathBuf>,
    /// For --html: KEY=VM[:CORE], the VM a core's EL2 channel is (C0C0=VM1) and the VM and core of an image's channel (guest0=VM1:C0C0); repeatable, over swtrace.map
    #[arg(long, value_name = "KEY=VM[:CORE]")]
    map: Vec<String>,
}

fn main() -> ExitCode {
    quiet_on_closed_stdout(|| {
        let cli = Cli::parse();
        match run(cli) {
            Ok(code) => ExitCode::from(code),
            Err(message) => {
                eprintln!("{} {message}", Style::stderr().bad("rvdbg:"));
                ExitCode::from(1)
            }
        }
    })
}

/// A closed stdout (`rvdbg config | head -1`) ends rvdbg quietly with 141,
/// the status a shell shows for a program that SIGPIPE ended. Rust ignores
/// SIGPIPE, so `println!` panics on EPIPE instead; that panic prints nothing
/// here and still unwinds, so an OpenOCD that rvdbg started is stopped as
/// on any other exit (ending at the signal would leave it running).
fn quiet_on_closed_stdout(run: impl FnOnce() -> ExitCode + std::panic::UnwindSafe) -> ExitCode {
    let report = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !is_closed_stdout(info.payload()) {
            report(info);
        }
    }));
    match std::panic::catch_unwind(run) {
        Ok(code) => code,
        Err(payload) if is_closed_stdout(payload.as_ref()) => ExitCode::from(141),
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

/// The panic of `print!`/`println!` on a closed stdout: std's
/// "failed printing to stdout: Broken pipe (os error 32)" (EPIPE is 32 on
/// Linux and macOS).
fn is_closed_stdout(payload: &(dyn std::any::Any + Send)) -> bool {
    let text = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied());
    text.is_some_and(|text| {
        text.starts_with("failed printing to stdout: ") && text.ends_with("(os error 32)")
    })
}

fn run(cli: Cli) -> Result<u8, String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let flag = cli.backend.map(Backend::from);
    let explicit_tui = if cli.inline {
        Some((tui::ScreenMode::Inline, "--inline"))
    } else {
        cli.tui.map(|mode| (mode.into(), "--tui"))
    };
    if let Some((mode, flag_name)) = explicit_tui {
        if cli.command.is_some() {
            return Err(format!(
                "{flag_name} starts a session and takes no subcommand"
            ));
        }
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            return Err(format!(
                "{flag_name} requires a terminal on stdin and stdout"
            ));
        }
        return tui::run(
            session::load_config(
                &config::find(cli.config.as_deref(), &cwd)?,
                &cwd,
                cli.images.as_deref(),
            )?,
            flag,
            mode,
        );
    }
    let Some(command) = cli.command else {
        let interactive_tty = !cli.json
            && std::io::stdin().is_terminal()
            && std::io::stdout().is_terminal()
            && std::env::var("TERM").as_deref() != Ok("dumb");
        if interactive_tty {
            return tui::run(
                session::load_config(
                    &config::find(cli.config.as_deref(), &cwd)?,
                    &cwd,
                    cli.images.as_deref(),
                )?,
                flag,
                tui::ScreenMode::Fullscreen,
            );
        }
        return session::run(
            cli.config.as_deref(),
            &cwd,
            flag,
            cli.json,
            cli.images.as_deref(),
        );
    };
    if let Commands::Init = command {
        return init_command(&cwd);
    }
    if let Commands::Skill { dir } = &command {
        return install_skill(dir.as_deref());
    }
    if let Commands::Swtrace(args) = &command
        && args.from.is_some()
    {
        // From a file: no probe, and rvdbg.toml only for swtrace.map.
        let config = match config::find(cli.config.as_deref(), &cwd) {
            Ok(path) => Some(config::load(&path)?),
            Err(_) if cli.config.is_none() => None,
            Err(error) => return Err(error),
        };
        let Commands::Swtrace(args) = command else {
            unreachable!()
        };
        return swtrace_from(config.as_ref(), cli.json, args);
    }
    let path = config::find(cli.config.as_deref(), &cwd)?;
    let mut config = config::load(&path)?;
    if let Some(dir) = &cli.images {
        let resolved = config::resolve_images_dir(dir, &cwd, &config.root)?;
        config = config.with_images(&resolved)?;
    }
    let mut env = Env {
        config: &config,
        flag,
        selected: std::cell::Cell::new(None),
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
    pub selected: std::cell::Cell<Option<Backend>>,
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
    /// What the session received on each FDX link.
    pub history: Option<&'a mut fdx_terminal::History>,
    pub(crate) ui: Option<&'a tui::CommandUi>,
    pub(crate) fdx_targets: &'a [fdx_terminal::Target],
}

impl Env<'_> {
    /// The session's backend, else `--backend`, `project.backend` or the
    /// connected probe (said on stderr).
    fn backend(&self) -> Result<Backend, String> {
        match &self.session {
            Some(link) => Ok(link.backend),
            None => {
                if let Some(backend) = self.selected.get() {
                    return Ok(backend);
                }
                let backend = select(self.config, self.flag)?;
                self.selected.set(Some(backend));
                Ok(backend)
            }
        }
    }

    /// Run `f` on the session's PowerView connection, or on one for this
    /// command. A failure drops the session's, so that the next command
    /// connects again; nothing is retried.
    fn with_powerview<T>(
        &mut self,
        f: impl FnOnce(&mut tracebridge::backend::Session) -> Result<T, String>,
    ) -> Result<T, String> {
        let connect =
            || tracebridge::backend::Session::connect(trace32(self.config)?).map_err(text);
        match self
            .session
            .as_mut()
            .and_then(|link| link.powerview.as_deref_mut())
        {
            Some(slot) => {
                let session = match slot {
                    Some(session) => session,
                    None => slot.insert(connect()?),
                };
                let result = f(session);
                if result.is_err() {
                    *slot = None;
                }
                result
            }
            None => f(&mut connect()?),
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
        } => {
            let backend = env.backend()?;
            let ui = env.session.as_ref().and_then(|link| link.ui);
            let input = if output_only {
                None
            } else {
                ui.map(tui::CommandUi::rtt).transpose()?
            };
            match backend {
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
                    let interrupted = env.session.as_ref().map(|link| link.interrupted);
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
                            interrupted,
                            input.as_ref(),
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
                    let interrupted = env.session.as_ref().map(|link| link.interrupted);
                    trace32(config)?
                        .rtt_with_input(&args, interrupted, input.as_ref())
                        .map_err(text)?;
                    Ok(0)
                }
            }
        }
        Commands::Fdx(args) => fdx(env, args),
        Commands::Reset { keep_links, go } => reset(env, keep_links, go),
        Commands::Restart { keep_links } => reset(env, keep_links, true),
        Commands::Cores => cores(env),
        Commands::Images => images(env),
        Commands::Swtrace(args) => swtrace(env, args),
        Commands::Ram {
            location,
            args,
            write,
        } => ram(env, location, args, write),
        Commands::Mem { location, count } => {
            let location = resolve_mem_location(env.config, location)?;
            inspect_command(env, Commands::Mem { location, count })
        }
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
                let interrupted = env.session.as_ref().map(|link| link.interrupted);
                let cancel = || interrupted.is_some_and(|flag| flag.load(Ordering::SeqCst));
                let code = env.with_powerview(|session| {
                    session.debug_cancellable(args, &cancel).map_err(text)
                })?;
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
        Commands::Reset { .. } | Commands::Restart { .. } => {
            (Ok(inspect::Request::Reset), vec!["reset".into()])
        }
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

/// Every image's FDX link: the images with `symbol`, as `el2`, `guest0`, …
/// An image that is missing or has no such symbol has no link.
pub(crate) fn link_targets(config: &Config, symbol: &str) -> Vec<fdx_terminal::Target> {
    std::iter::once(None)
        .chain((0..config.extra_elf.len()).map(Some))
        .filter_map(|extra| {
            let path = match extra {
                None => &config.elf,
                Some(n) => &config.extra_elf[n],
            };
            let address = rustvisor_debug::fdx::symbol_address(path, symbol).ok()?;
            Some(fdx_terminal::Target {
                name: rustvisor_debug::fdx::image_name(extra),
                symbol: symbol.to_string(),
                address,
            })
        })
        .collect()
}

fn fdx_options(
    config: &Config,
    args: FdxArgs,
    json: bool,
) -> Result<fdx_terminal::Options, String> {
    let FdxArgs {
        guest,
        all,
        symbol,
        address,
        send,
        expect,
        timeout,
        close,
    } = args;
    if !(timeout > 0.0 && timeout.is_finite()) {
        return Err("--timeout must be positive".into());
    }
    let symbol = symbol.unwrap_or_else(|| config.fdx_symbol.clone());
    let every = link_targets(config, &symbol);
    let targets = if all {
        if every.is_empty() {
            return Err(format!("no image has {symbol}; build the images"));
        }
        every.clone()
    } else {
        let image = match &guest {
            None => None,
            Some(name) => rustvisor_debug::fdx::parse_image(name, config.extra_elf.len())
                .map_err(|error| format!("--guest {error}"))?,
        };
        let path = match image {
            None => &config.elf,
            Some(n) => &config.extra_elf[n],
        };
        let address = match address {
            Some(address) => address,
            None => rustvisor_debug::fdx::symbol_address(path, &symbol).map_err(|error| {
                format!("{error}; build the images, or pass --address 0x<address>")
            })?,
        };
        vec![fdx_terminal::Target {
            name: rustvisor_debug::fdx::image_name(image),
            symbol: symbol.clone(),
            address,
        }]
    };
    let peers = if all {
        every
    } else {
        every
            .into_iter()
            .filter(|peer| peer.address != targets[0].address)
            .collect()
    };
    Ok(fdx_terminal::Options {
        targets,
        send,
        expect,
        timeout: std::time::Duration::from_secs_f64(timeout),
        json,
        diagnosis: fdx_terminal::Diagnosis {
            peers,
            stops: rustvisor_debug::stop::slots(&config.elf).ok().flatten(),
            symbols: if close {
                Default::default()
            } else {
                inspect::symbols(config)
            },
        },
        history_file: run_dir(config).ok().map(|dir| dir.join("fdx-history")),
    })
}

/// Resolve the links, then run the standalone or scripted FDX command.
fn fdx(env: &mut Env, args: FdxArgs) -> Result<u8, String> {
    let close = args.close;
    let options = fdx_options(env.config, args, env.json)?;
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
    let mut local = fdx_terminal::History::default();
    let history = match env.session.as_mut().and_then(|link| link.history.take()) {
        Some(history) => history,
        None => &mut local,
    };
    env.with_ram(|mut memory, probe| {
        if close {
            for target in &options.targets {
                rustvisor_debug::fdx::Link::close(&mut memory, target.address).map_err(text)?;
                history.forget(target.address);
                if !options.json {
                    println!("closed {} at 0x{:08X}", target.symbol, target.address);
                }
            }
            return Ok(0);
        }
        fdx_terminal::run(&mut memory, probe, &options, history, interrupted)
    })
}

/// `rvdbg reset` (and `rvdbg restart` / `reset --go`): reset, mark every
/// image's FDX link closed while the chip is held, and optionally `go`.
fn reset(env: &mut Env, keep_links: bool, go: bool) -> Result<u8, String> {
    let code = if go && env.json {
        // Keep `--json` strictly one JSON document per command.
        probecore::output::with_sink(
            |line| {
                if let probecore::output::Line::Error(error) = line {
                    std::eprintln!("{error}");
                }
            },
            || {
                let mut quiet = Env {
                    config: env.config,
                    flag: env.flag,
                    selected: std::cell::Cell::new(env.selected.get()),
                    json: false,
                    session: env.session.as_mut().map(|link| SessionLink {
                        backend: link.backend,
                        server: link.server.as_deref_mut(),
                        interrupted: link.interrupted,
                        powerview: link.powerview.as_deref_mut(),
                        history: link.history.as_deref_mut(),
                        ui: link.ui,
                        fdx_targets: link.fdx_targets,
                    }),
                };
                inspect_command(
                    &mut quiet,
                    Commands::Reset {
                        keep_links,
                        go: false,
                    },
                )
            },
        )?
    } else {
        inspect_command(
            env,
            Commands::Reset {
                keep_links,
                go: false,
            },
        )?
    };
    if code != 0 {
        return Ok(code);
    }
    if !keep_links {
        let mut targets = link_targets(env.config, &env.config.fdx_symbol);
        if let Some(link) = &env.session {
            for target in link.fdx_targets {
                if !targets.iter().any(|known| known.address == target.address) {
                    targets.push(target.clone());
                }
            }
        }
        let mut local = fdx_terminal::History::default();
        let history = match env.session.as_mut().and_then(|link| link.history.take()) {
            Some(history) => history,
            None => &mut local,
        };
        history.clear();
        if !targets.is_empty() {
            let style = Style::stderr();
            // Each link on its own: RAM a Guest has not set up since power-on may
            // refuse the write (bus error) while the chip is held.
            let outcome = env.with_ram(|mut memory, _| {
                Ok(targets
                    .iter()
                    .map(|target| {
                        rustvisor_debug::fdx::Link::close(&mut memory, target.address)
                            .map_err(|error| error.to_string())
                    })
                    .collect::<Vec<_>>())
            });
            match outcome {
                Ok(results) => {
                    let mut closed = Vec::new();
                    let mut failed = Vec::new();
                    for (target, result) in targets.iter().zip(&results) {
                        match result {
                            Ok(()) => closed.push(target.name.as_str()),
                            Err(error) => failed.push(format!("{} ({error})", target.name)),
                        }
                    }
                    let mut line = format!(
                        "{} closed {} FDX link(s): {}",
                        style.label("[rvdbg]"),
                        closed.len(),
                        closed.join(" ")
                    );
                    if !failed.is_empty() {
                        line.push_str(&format!(
                            "; not closed: {} (RAM its image has not set up since power-on)",
                            failed.join(", ")
                        ));
                    }
                    eprintln!("{line}");
                }
                Err(error) => eprintln!(
                    "{} FDX links not closed ({error}); 'fdx --close --all' tries again",
                    style.warn("[rvdbg]")
                ),
            }
        }
    }
    if go {
        return inspect_command(env, Commands::Go);
    }
    Ok(code)
}

/// `rvdbg cores`: the stop records and the FDX links, read while the cores
/// run.
fn cores(env: &mut Env) -> Result<u8, String> {
    use rustvisor_debug::stop;
    let config = env.config;
    let json = env.json;
    let slots = stop::slots(&config.elf)?.ok_or_else(|| {
        format!(
            "{} has no {}: cores reads the hypervisor's stop records",
            config.elf.display(),
            stop::SYMBOL
        )
    })?;
    let diagnosis = fdx_terminal::Diagnosis {
        symbols: inspect::symbols(config),
        ..Default::default()
    };
    let targets = link_targets(config, &config.fdx_symbol);
    let mut local = fdx_terminal::History::default();
    let history = match env.session.as_mut().and_then(|link| link.history.take()) {
        Some(history) => history,
        None => &mut local,
    };
    let style = Style::stdout();
    let result = env.with_ram(|mut memory, _| {
        let now = std::time::Instant::now();
        let records = stop::read(&mut memory, slots)
            .map_err(|error| format!("cannot read {}: {error}", stop::SYMBOL))?;
        let mut lines = vec![style.label(format!(
            "stop records  {} @ 0x{:08X}, {} slot(s)",
            stop::SYMBOL,
            slots.address,
            slots.count
        ))];
        let mut stops = Vec::new();
        for (slot, record) in records.iter().enumerate() {
            let (line, value) = match record {
                stop::Record::Empty => {
                    (style.good("no stop"), serde_json::json!({"state": "empty"}))
                }
                stop::Record::Incomplete => (
                    style.warn("a record is being written"),
                    serde_json::json!({"state": "incomplete"}),
                ),
                stop::Record::Invalid(magic) => (
                    style.dim(format!(
                        "not a record (0x{magic:08X}); not initialised yet?"
                    )),
                    serde_json::json!({"state": "invalid", "magic": format!("0x{magic:08X}")}),
                ),
                stop::Record::Complete(stop) => {
                    let mut value = stop.fields();
                    value["state"] = "complete".into();
                    (
                        format!(
                            "{} {}",
                            style.bad(format!("{} stopped:", stop.core)),
                            stop.summary(&|address| diagnosis.symbol(address))
                        ),
                        value,
                    )
                }
            };
            lines.push(format!("  slot {slot}  {line}"));
            stops.push(value);
        }
        lines.push(style.label(format!("FDX links  {}", config.fdx_symbol)));
        let mut links = Vec::new();
        for target in &targets {
            // RAM a Guest has not set up yet reads as anything.
            let (open, state) =
                match rustvisor_debug::fdx::Link::locate(&mut memory, target.address) {
                    Ok(Some(_)) => (Some(true), style.good("open")),
                    Ok(None) => (Some(false), style.dim("closed")),
                    Err(error) => (None, style.dim(format!("not a link yet ({error})"))),
                };
            let last = history.last(target.address, 1).pop();
            let mut line = format!("  {:<8} 0x{:08X}  {state}", target.name, target.address);
            if let Some((at, packet)) = &last {
                line.push_str(&format!(
                    ", last packet {packet:?} ({} s ago)",
                    now.saturating_duration_since(*at).as_secs()
                ));
            }
            lines.push(line);
            links.push(serde_json::json!({
                "name": target.name,
                "address": format!("0x{:08X}", target.address),
                "open": open,
                "last": last.map(|(_, packet)| packet),
            }));
        }
        Ok(inspect::Outcome {
            text: lines.join("\n"),
            json: serde_json::json!({"stops": stops, "links": links}),
            code: 0,
        })
    });
    inspect::emit("cores", json, result)
}

/// `rvdbg images`: the images, where they come from, and their FDX links
/// (open or closed when a probe answers).
fn images(env: &mut Env) -> Result<u8, String> {
    let config = env.config;
    let json = env.json;
    let style = Style::stdout();
    let targets = link_targets(config, &config.fdx_symbol);
    let open: Option<Vec<bool>> = env
        .with_ram(|mut memory, _| {
            Ok(targets
                .iter()
                .map(|target| {
                    matches!(
                        rustvisor_debug::fdx::Link::locate(&mut memory, target.address),
                        Ok(Some(_))
                    )
                })
                .collect())
        })
        .ok();
    let mut lines = vec![format!(
        "{} {}",
        style.label("images from"),
        config.images_from
    )];
    let mut list = Vec::new();
    for (index, path) in config.images().iter().enumerate() {
        let extra = index.checked_sub(1);
        let name = rustvisor_debug::fdx::image_name(extra);
        let target = targets.iter().position(|target| target.name == name);
        let link = match target {
            Some(i) => {
                let state = match &open {
                    Some(open) if open[i] => style.good("open"),
                    Some(_) => style.dim("closed"),
                    None => style.dim("-"),
                };
                format!("{} 0x{:08X} {state}", config.fdx_symbol, targets[i].address)
            }
            None => style.dim(format!("no {}", config.fdx_symbol)),
        };
        let exists = if path.is_file() {
            String::new()
        } else {
            format!(" {}", style.bad("MISSING"))
        };
        lines.push(format!("  {name:<8} {}{exists}  {link}", path.display()));
        list.push(serde_json::json!({
            "name": name,
            "path": path.display().to_string(),
            "exists": path.is_file(),
            "link": target.map(|i| format!("0x{:08X}", targets[i].address)),
            "open": match (&open, target) {
                (Some(open), Some(i)) => Some(open[i]),
                _ => None,
            },
        }));
    }
    inspect::emit(
        "images",
        json,
        Ok(inspect::Outcome {
            text: lines.join("\n"),
            json: serde_json::json!({"from": config.images_from.to_string(), "images": list}),
            code: 0,
        }),
    )
}

/// `rvdbg swtrace`: every trace channel of the chosen images, drained in
/// one go (reading consumes), then the report.
fn swtrace(env: &mut Env, args: SwtraceArgs) -> Result<u8, String> {
    use rustvisor_debug::swtrace;
    let config = env.config;
    let json = env.json;
    if args.from.is_some() {
        return swtrace_from(Some(config), json, args);
    }
    let map = swtrace_map(Some(config), &args.map)?;
    let symbol = args.symbol.unwrap_or_else(|| swtrace::SYMBOL.to_string());
    let explicit = !args.guest.is_empty();
    let images: Vec<(String, PathBuf)> = if explicit {
        let mut all_names = vec!["el2".to_string()];
        all_names.extend((0..config.extra_elf.len()).map(|n| format!("guest{n}")));
        let mut chosen_indices = Vec::new();
        for spec in &args.guest {
            let resolved = rustvisor_debug::fdx::resolve_targets(spec, &all_names)
                .map_err(|error| format!("--guest {error}"))?;
            for idx in resolved {
                if !chosen_indices.contains(&idx) {
                    chosen_indices.push(idx);
                }
            }
        }
        chosen_indices
            .into_iter()
            .map(|idx| {
                if idx == 0 {
                    ("elf".to_string(), config.elf.clone())
                } else {
                    let n = idx - 1;
                    (format!("guest{n}"), config.extra_elf[n].clone())
                }
            })
            .collect()
    } else {
        std::iter::once(("elf".to_string(), config.elf.clone()))
            .chain(
                config
                    .extra_elf
                    .iter()
                    .enumerate()
                    .map(|(n, path)| (format!("guest{n}"), path.clone())),
            )
            .collect()
    };
    let mut channels = Vec::new();
    for (image, path) in images {
        let found = swtrace::channels(&path, &symbol)?;
        if found.is_empty() && explicit {
            return Err(format!(
                "{symbol} is not in the symbol table of {}",
                path.display()
            ));
        }
        channels.extend(
            found
                .into_iter()
                .map(|(name, address)| (image.clone(), name, address)),
        );
    }
    if channels.is_empty() {
        return Err(format!(
            "no image has {symbol}: build the images with the trace channel"
        ));
    }
    let result = env.with_ram(|mut memory, _| {
        let mut found = Vec::new();
        for (image, name, address) in channels {
            let drained = swtrace::read(&mut memory, address)
                .map_err(|error| format!("{image} {name} at 0x{address:08X}: {error}"))?;
            found.push(swtrace::Found {
                image,
                symbol: name,
                address,
                drained,
            });
        }
        let (mut text, mut json) = swtrace::report(&found, &symbol);
        let has_records = found.iter().any(|f| !f.drained.records.is_empty());
        if let Some(path) = &args.csv {
            write_swtrace_csv(path, &found, &symbol)?;
            text.push_str(&format!("\nrecords written to {}", path.display()));
            json["csv"] = serde_json::Value::String(path.display().to_string());
        } else if has_records && let Ok(dir) = run_dir(config) {
            let backup_dir = dir.join("swtrace");
            let backup_path = backup_dir.join("latest.csv");
            if std::fs::create_dir_all(&backup_dir).is_ok()
                && write_swtrace_csv(&backup_path, &found, &symbol).is_ok()
            {
                text.push_str(&format!(
                    "\nsaved backup to {} ('swtrace --from latest' redraws)",
                    backup_path.display()
                ));
                json["saved_csv"] = serde_json::Value::String(backup_path.display().to_string());
            }
        }
        // The records are consumed: a page that cannot be drawn must not
        // lose the report (or the CSV written above).
        let mut code = 0;
        if let Some(html_path) = resolve_swtrace_html(Some(config), args.html.as_deref(), args.open)
        {
            let source = format!("{} channel(s) read from the board", found.len());
            if let Err(error) = swtrace_html(
                &found, &symbol, &map, &html_path, "swtrace", &source, &mut text, &mut json,
            ) {
                text.push_str(&format!("\ntimeline not written: {error}"));
                json["html_error"] = serde_json::Value::String(error);
                code = 1;
            } else if args.open {
                open_in_browser(&html_path, &mut text);
            }
        }
        Ok(inspect::Outcome { text, json, code })
    });
    inspect::emit("swtrace", json, result)
}

fn write_swtrace_csv(
    path: &Path,
    found: &[rustvisor_debug::swtrace::Found],
    symbol: &str,
) -> Result<(), String> {
    let names: Vec<String> = found.iter().map(|f| f.label(symbol)).collect();
    let records: Vec<_> = found
        .iter()
        .enumerate()
        .flat_map(|(i, f)| f.drained.records.iter().map(move |&r| (i, r)))
        .collect();
    std::fs::write(path, rustvisor_debug::swtrace::csv(&records, &names))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn resolve_swtrace_html(
    config: Option<&config::Config>,
    explicit: Option<&Path>,
    open: bool,
) -> Option<PathBuf> {
    if explicit.is_none() && !open {
        return None;
    }
    match explicit {
        Some(path) if path != Path::new("latest") && !path.as_os_str().is_empty() => {
            Some(path.to_path_buf())
        }
        _ => match config {
            Some(c) => {
                let dir = c.run_dir().join("swtrace");
                let _ = std::fs::create_dir_all(&dir);
                Some(dir.join("latest.html"))
            }
            None => Some(PathBuf::from("swtrace.html")),
        },
    }
}

fn open_in_browser(path: &Path, text: &mut String) {
    if std::env::var_os("RVDBG_NO_BROWSER").as_deref() == Some(std::ffi::OsStr::new("1")) {
        return;
    }
    #[cfg(target_os = "macos")]
    let launcher = "open";
    #[cfg(not(target_os = "macos"))]
    let launcher = "xdg-open";
    match std::process::Command::new(launcher)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => text.push_str(&format!("\nopened {} with {launcher}", path.display())),
        Err(error) => text.push_str(&format!("\ncannot open browser with {launcher}: {error}")),
    }
}

/// `rvdbg swtrace --from`: the report (and `--html`) of records a
/// `--csv` file holds, without a probe.
fn swtrace_from(
    config: Option<&config::Config>,
    json: bool,
    args: SwtraceArgs,
) -> Result<u8, String> {
    use rustvisor_debug::swtrace;
    let raw_path = args.from.as_ref().expect("--from");
    let path = if raw_path == Path::new("latest") && !raw_path.is_file() {
        let Some(c) = config else {
            return Err("cannot find 'latest': pass a CSV path or run inside a project".into());
        };
        c.run_dir().join("swtrace/latest.csv")
    } else {
        raw_path.clone()
    };
    let symbol = args.symbol.unwrap_or_else(|| swtrace::SYMBOL.to_string());
    let map = swtrace_map(config, &args.map)?;
    let result = (|| {
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let found = swtrace::from_csv(&text, &symbol)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let (mut text, mut json) = swtrace::report(&found, &symbol);
        text = format!(
            "from {} (no channel addresses or FDX drop markers in a CSV)\n{text}",
            path.display()
        );
        json["from"] = serde_json::Value::String(path.display().to_string());
        if let Some(html_path) = resolve_swtrace_html(config, args.html.as_deref(), args.open) {
            let name = path
                .file_stem()
                .map_or("swtrace".into(), |stem| stem.to_string_lossy().into_owned());
            let source = path
                .file_name()
                .map_or(String::new(), |name| name.to_string_lossy().into_owned());
            swtrace_html(
                &found, &symbol, &map, &html_path, &name, &source, &mut text, &mut json,
            )?;
            if args.open {
                open_in_browser(&html_path, &mut text);
            }
        }
        Ok(inspect::Outcome {
            text,
            json,
            code: 0,
        })
    })();
    inspect::emit("swtrace", json, result)
}

/// `swtrace.map` with the `--map` items on top.
fn swtrace_map(
    config: Option<&config::Config>,
    items: &[String],
) -> Result<rustvisor_debug::timeline::Map, String> {
    let extra = rustvisor_debug::timeline::Map::parse(items.iter().map(String::as_str))
        .map_err(|error| format!("--map {error}"))?;
    Ok(config
        .map(|config| config.swtrace_map.clone())
        .unwrap_or_default()
        .merged(&extra))
}

/// Writes the timeline page of `found` to `path` and says so.
#[allow(clippy::too_many_arguments)]
fn swtrace_html(
    found: &[rustvisor_debug::swtrace::Found],
    symbol: &str,
    map: &rustvisor_debug::timeline::Map,
    path: &Path,
    name: &str,
    source: &str,
    text: &mut String,
    json: &mut serde_json::Value,
) -> Result<(), String> {
    use rustvisor_debug::timeline;
    let texts = timeline::Texts {
        name: name.to_string(),
        source: source.to_string(),
    };
    let doc =
        timeline::data(found, symbol, map, &texts).map_err(|error| format!("--html: {error}"))?;
    std::fs::write(path, timeline::page(&doc))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    text.push_str(&format!("\ntimeline written to {}", path.display()));
    if map.is_empty() {
        text.push_str(" (no swtrace.map: one panel per channel)");
    }
    json["html"] = serde_json::Value::String(path.display().to_string());
    Ok(())
}

fn configured_images(config: &Config) -> Vec<(String, &Path)> {
    std::iter::once(("el2".to_string(), config.elf.as_path()))
        .chain(
            config
                .extra_elf
                .iter()
                .enumerate()
                .map(|(n, path)| (format!("guest{n}"), path.as_path())),
        )
        .collect()
}

fn parse_symbol_offset(expr: &str) -> Result<(&str, u64), String> {
    match expr.split_once('+') {
        Some((sym, off)) => {
            let sym = sym.trim();
            if sym.is_empty() {
                return Err(format!("missing symbol before '+' in {expr:?}"));
            }
            let offset = probecore::text::parse_int_auto(off.trim())
                .and_then(|value| u64::try_from(value).ok())
                .ok_or_else(|| format!("invalid offset in {expr:?}"))?;
            Ok((sym, offset))
        }
        None => Ok((expr.trim(), 0)),
    }
}

fn find_named_symbol(path: &Path, symbol: &str) -> Result<Option<(u64, bool)>, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let list = probecore::elf::named_symbols(&bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(list
        .into_iter()
        .find(|(name, _, _)| name == symbol)
        .map(|(_, value, is_abs)| (value, is_abs)))
}

fn resolve_mem_location(config: &Config, location: String) -> Result<String, String> {
    if probecore::target::TargetAddress::parse(&location).is_some() {
        return Ok(location);
    }
    if let Some(addr) = probecore::text::parse_int_auto(&location) {
        return Ok(format!("AD:0x{addr:08X}"));
    }
    if location.contains(':') {
        let targets = resolve_ram_targets(config, &location)?;
        if targets.len() != 1 {
            return Err(format!(
                "{location} matches multiple images; use a single image for 'mem', or use 'ram' \
                 for multi-image reads"
            ));
        }
        return Ok(format!("AD:0x{:08X}", targets[0].1));
    }
    // Let normal ELF symbols keep their name for TRACE32/OpenOCD display,
    // while resolving `SHN_ABS` symbols (which TRACE32/OpenOCD's section table omits).
    let (sym, offset) = parse_symbol_offset(&location)?;
    for (_, path) in configured_images(config) {
        if let Ok(Some((base, is_abs))) = find_named_symbol(path, sym)
            && is_abs
        {
            return Ok(format!("AD:0x{:08X}", base + offset));
        }
    }
    Ok(location)
}

/// Resolve an address, `[image:]symbol[+offset]`, or multi-image selector (`0..2:MAILBOX+0x3c`)
/// into `(optional label, physical address)` pairs.
fn resolve_ram_targets(config: &Config, spec: &str) -> Result<Vec<(Option<String>, u64)>, String> {
    if let Some(address) = probecore::target::TargetAddress::parse(spec) {
        if address.is_coprocessor() {
            return Err(format!(
                "{spec} is a coprocessor register; use 'mem {spec}' instead of 'ram'"
            ));
        }
        return Ok(vec![(None, address.value)]);
    }
    if let Some(address) =
        probecore::text::parse_int_auto(spec).and_then(|value| u64::try_from(value).ok())
    {
        return Ok(vec![(None, address)]);
    }
    let images = configured_images(config);
    if let Some((scope, expr)) = spec.split_once(':') {
        let names: Vec<String> = images.iter().map(|(name, _)| name.clone()).collect();
        let indices = rustvisor_debug::fdx::resolve_targets(scope, &names)?;
        let (sym, offset) = parse_symbol_offset(expr)?;
        if sym.is_empty() {
            return Err(format!("missing symbol after ':' in {spec:?}"));
        }
        let mut out = Vec::with_capacity(indices.len());
        for idx in indices {
            let (image_name, path) = &images[idx];
            let (base, _) = find_named_symbol(path, sym)?
                .ok_or_else(|| format!("{sym} is not in the symbol table of {}", path.display()))?;
            let label = if offset == 0 {
                format!("{image_name}:{sym}")
            } else {
                format!("{image_name}:{sym}+0x{offset:X}")
            };
            out.push((Some(label), base + offset));
        }
        return Ok(out);
    }
    let (sym, offset) = parse_symbol_offset(spec)?;
    if sym.is_empty() {
        return Err("location cannot be empty".into());
    }
    let mut matches = Vec::new();
    for (image_name, path) in images {
        if let Ok(Some((base, _))) = find_named_symbol(path, sym) {
            matches.push((image_name, base + offset));
        }
    }
    if matches.is_empty() {
        return Err(format!(
            "{spec} is neither an address nor a symbol of the images"
        ));
    }
    let mut distinct = Vec::new();
    for (_, addr) in &matches {
        if !distinct.contains(addr) {
            distinct.push(*addr);
        }
    }
    if distinct.len() > 1 {
        let list: Vec<String> = matches
            .iter()
            .map(|(name, addr)| format!("{name}: 0x{addr:08X}"))
            .collect();
        return Err(format!(
            "{sym} has different addresses across images ({}); qualify it as guest0:{spec} or \
             0..2:{spec}",
            list.join(", ")
        ));
    }
    Ok(vec![(Some(spec.to_string()), distinct[0])])
}

enum RamMode {
    Read(u32),
    Write(Vec<u32>),
}

fn parse_ram_args(
    location: String,
    args: Vec<String>,
    write: Vec<u32>,
) -> Result<(String, RamMode), String> {
    if let Some((loc, rhs)) = location.split_once('=') {
        if !write.is_empty() {
            return Err("cannot combine '=' and --write".into());
        }
        let loc = loc.trim().to_string();
        let mut raw_values = Vec::new();
        if !rhs.trim().is_empty() {
            raw_values.push(rhs.trim().to_string());
        }
        raw_values.extend(args);
        if raw_values.is_empty() {
            return Err("missing value after '='".into());
        }
        let words = raw_values
            .iter()
            .map(|v| parse_word(v))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok((loc, RamMode::Write(words)));
    }
    if let Some(first) = args.first()
        && first.starts_with('=')
    {
        if !write.is_empty() {
            return Err("cannot combine '=' and --write".into());
        }
        let mut raw_values = Vec::new();
        let tail = first.trim_start_matches('=').trim();
        if !tail.is_empty() {
            raw_values.push(tail.to_string());
        }
        raw_values.extend(args.into_iter().skip(1));
        if raw_values.is_empty() {
            return Err("missing value after '='".into());
        }
        let words = raw_values
            .iter()
            .map(|v| parse_word(v))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok((location, RamMode::Write(words)));
    }
    if !write.is_empty() {
        if !args.is_empty() {
            return Err("count cannot be combined with --write".into());
        }
        return Ok((location, RamMode::Write(write)));
    }
    match args.as_slice() {
        [] => Ok((location, RamMode::Read(1))),
        [count_str] => {
            let count = probecore::text::parse_int_auto(count_str)
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| {
                    format!(
                        "invalid word count {count_str:?} (for writes, use '= VALUE...' or \
                         '--write VALUE...')"
                    )
                })?;
            Ok((location, RamMode::Read(count)))
        }
        [_, extra, ..] => Err(format!(
            "unexpected argument {extra:?} (for writes, use '= VALUE...' or '--write VALUE...')"
        )),
    }
}

fn ascii_tag(words: &[u32]) -> Option<String> {
    let mut has_tag = false;
    let mut out = String::with_capacity(words.len() * 4);
    for &w in words {
        let bytes = w.to_be_bytes();
        if bytes.iter().all(|&b| (0x20..=0x7E).contains(&b))
            && bytes.iter().any(|&b| (0x21..=0x7E).contains(&b))
        {
            has_tag = true;
            for b in bytes {
                out.push(b as char);
            }
        } else {
            out.push_str("....");
        }
    }
    has_tag.then_some(out)
}

/// `rvdbg ram`: words of RAM in bulk while the cores run.
fn ram(env: &mut Env, location: String, args: Vec<String>, write: Vec<u32>) -> Result<u8, String> {
    let config = env.config;
    let json = env.json;
    let (location, mode) = parse_ram_args(location, args, write)?;
    let targets = resolve_ram_targets(config, &location)?;
    for (_, address) in &targets {
        if address % 4 != 0 {
            return Err(format!("0x{address:X} is not word-aligned"));
        }
    }
    let result = env.with_ram(|memory, _| match &mode {
        RamMode::Write(words) => {
            for (_, address) in &targets {
                for (index, value) in words.iter().enumerate() {
                    memory
                        .write_u32(address + 4 * index as u64, *value)
                        .map_err(|error| format!("cannot write 0x{address:08X}: {error}"))?;
                }
            }
            if targets.len() == 1 {
                let (symbol, address) = &targets[0];
                return Ok(inspect::Outcome {
                    text: format!("wrote {} word(s) at 0x{address:08X}", words.len()),
                    json: serde_json::json!({
                        "symbol": symbol,
                        "address": format!("0x{address:08X}"),
                        "written": words.len(),
                    }),
                    code: 0,
                });
            }
            let summary: Vec<String> = targets
                .iter()
                .map(|(label, addr)| {
                    format!("{} (0x{addr:08X})", label.as_deref().unwrap_or("addr"))
                })
                .collect();
            let json_targets: Vec<_> = targets
                .iter()
                .map(|(symbol, address)| {
                    serde_json::json!({
                        "symbol": symbol,
                        "address": format!("0x{address:08X}"),
                        "written": words.len(),
                    })
                })
                .collect();
            Ok(inspect::Outcome {
                text: format!(
                    "wrote {} word(s) to {} target(s): {}",
                    words.len(),
                    targets.len(),
                    summary.join(", ")
                ),
                json: serde_json::json!({ "targets": json_targets }),
                code: 0,
            })
        }
        RamMode::Read(count) => {
            if *count == 0 || *count > RAM_MAX_WORDS {
                return Err(format!("count must be between 1 and {RAM_MAX_WORDS}"));
            }
            if targets.len() == 1 {
                let (symbol, address) = &targets[0];
                let words = memory
                    .read_u32s(*address, *count as usize)
                    .map_err(|error| format!("cannot read 0x{address:08X}: {error}"))?;
                let text = format_ram_lines(*address, &words, None);
                return Ok(inspect::Outcome {
                    text,
                    json: serde_json::json!({
                        "symbol": symbol,
                        "address": format!("0x{address:08X}"),
                        "words": words,
                        "hex": words.iter().map(|w| format!("0x{w:08X}")).collect::<Vec<_>>(),
                    }),
                    code: 0,
                });
            }
            let mut all_lines = Vec::new();
            let mut json_targets = Vec::new();
            for (symbol, address) in &targets {
                let words = memory
                    .read_u32s(*address, *count as usize)
                    .map_err(|error| format!("cannot read 0x{address:08X}: {error}"))?;
                all_lines.push(format_ram_lines(*address, &words, symbol.as_deref()));
                json_targets.push(serde_json::json!({
                    "symbol": symbol,
                    "address": format!("0x{address:08X}"),
                    "words": words,
                    "hex": words.iter().map(|w| format!("0x{w:08X}")).collect::<Vec<_>>(),
                }));
            }
            Ok(inspect::Outcome {
                text: all_lines.join("\n"),
                json: serde_json::json!({ "targets": json_targets }),
                code: 0,
            })
        }
    });
    inspect::emit("ram", json, result)
}

fn format_ram_lines(address: u64, words: &[u32], prefix: Option<&str>) -> String {
    words
        .chunks(4)
        .enumerate()
        .map(|(row, chunk)| {
            let values: Vec<String> = chunk.iter().map(|w| format!("{w:08X}")).collect();
            let ascii = ascii_tag(chunk)
                .map(|tag| format!("  |{tag}|"))
                .unwrap_or_default();
            let head = prefix
                .map(|label| format!("{label:<22} "))
                .unwrap_or_default();
            format!(
                "{head}0x{:08X}  {}{ascii}",
                address + 16 * row as u64,
                values.join(" ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The most words one `rvdbg ram` reads: 4 MiB.
const RAM_MAX_WORDS: u32 = 1 << 20;

fn parse_word(text: &str) -> Result<u32, String> {
    if let Some(value) =
        probecore::text::parse_int_auto(text).and_then(|value| u32::try_from(value).ok())
    {
        return Ok(value);
    }
    let ascii = text
        .strip_prefix("ascii:")
        .or_else(|| text.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')));
    let bytes = match ascii {
        Some(tag) => Some(tag.as_bytes()),
        None if !text.as_bytes().first().is_some_and(u8::is_ascii_digit) => Some(text.as_bytes()),
        None => None,
    };
    if let Some(bytes) = bytes
        && bytes.len() == 4
        && bytes.iter().all(|&b| (0x20..=0x7E).contains(&b))
    {
        return Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
    }
    Err(format!(
        "invalid 32-bit value {text:?}: pass a 32-bit integer (e.g. 0x474F474F) or a \
         4-character ASCII tag (e.g. GOGO, DONE)"
    ))
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

    #[test]
    fn ram_accepts_ascii_tags_equals_syntax_and_formats_ascii() {
        assert_eq!(parse_word("GOGO").unwrap(), 0x474F_474F);
        assert_eq!(parse_word("DONE").unwrap(), 0x444F_4E45);
        assert_eq!(parse_word("ascii:1234").unwrap(), 0x3132_3334);
        assert_eq!(parse_word("'ABCD'").unwrap(), 0x4142_4344);
        assert_eq!(parse_word("1234").unwrap(), 1234);
        assert!(parse_word("GO").is_err());

        let (loc, mode) = parse_ram_args(
            "0..2:MAILBOX+0x3c".into(),
            vec!["=".into(), "GOGO".into()],
            Vec::new(),
        )
        .unwrap();
        assert_eq!(loc, "0..2:MAILBOX+0x3c");
        match mode {
            RamMode::Write(words) => assert_eq!(words, [0x474F_474F]),
            RamMode::Read(_) => panic!("expected write"),
        }

        let (loc, mode) = parse_ram_args("0x6001003C=DONE".into(), Vec::new(), Vec::new()).unwrap();
        assert_eq!(loc, "0x6001003C");
        match mode {
            RamMode::Write(words) => assert_eq!(words, [0x444F_4E45]),
            RamMode::Read(_) => panic!("expected write"),
        }

        assert_eq!(
            format_ram_lines(0x6001_003C, &[0x444F_4E45], None),
            "0x6001003C  444F4E45  |DONE|"
        );
        assert_eq!(
            format_ram_lines(0x6001_0000, &[0x0000_0000], None),
            "0x60010000  00000000"
        );
    }

    #[test]
    fn swtrace_accepts_named_guests_and_optional_html_path() {
        let cli = Cli::try_parse_from(["rvdbg", "swtrace", "--guest", "guest0", "--open"]).unwrap();
        let Commands::Swtrace(args) = cli.command.unwrap() else {
            panic!("expected swtrace");
        };
        assert_eq!(args.guest, ["guest0"]);
        assert!(args.open);
        assert_eq!(args.html, None);

        let cli = Cli::try_parse_from(["rvdbg", "swtrace", "--html", "--json"]).unwrap();
        assert!(cli.json);
        let Commands::Swtrace(args) = cli.command.unwrap() else {
            panic!("expected swtrace");
        };
        assert_eq!(args.html, Some(PathBuf::from("latest")));
    }

    #[test]
    fn resolve_mem_location_prefixes_ad_for_resolved_addresses() {
        let config = config::Config {
            file: PathBuf::from("rvdbg.toml"),
            root: PathBuf::from("."),
            program: String::new(),
            elf: PathBuf::from("nonexistent-el2.elf"),
            extra_elf: Vec::new(),
            images_from: config::ImagesFrom::Config,
            backend: None,
            fdx_symbol: "RUSTVISOR_FDX".into(),
            swtrace_map: rustvisor_debug::timeline::Map::default(),
            trace32: toml::Table::new(),
            openocd: None,
        };
        assert_eq!(
            resolve_mem_location(&config, "0x6001003c".into()).unwrap(),
            "AD:0x6001003C"
        );
        assert_eq!(
            resolve_mem_location(&config, "AD:0x6001003C".into()).unwrap(),
            "AD:0x6001003C"
        );
        assert_eq!(
            resolve_mem_location(&config, "C15:0x100".into()).unwrap(),
            "C15:0x100"
        );
    }
}
