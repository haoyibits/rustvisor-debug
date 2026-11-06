//! `rvdbg`: rustvisor-specific debugging front end. It reads `rvdbg.toml`
//! and runs each command on the backend of the connected probe: TRACE32
//! through tracebridge, or StellarLINK through OpenOCD (stellarbridge).
//!
//! The TRACE32 IDE files and the PowerView toolbar run
//! `<exe> --config <file> adapter|flash|load`, the way tracebridge's do, so
//! rvdbg accepts the same command names; `adapter` is always TRACE32.
#![forbid(unsafe_code)]

mod config;
mod init;
mod openocd;
mod probe;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

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

    #[command(subcommand)]
    command: Commands,
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
    /// Write rvdbg.toml in the current directory (values from trace32.toml if there is one)
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
    /// Write the RustRover run configuration for TRACE32 (.run/, LSP4IJ)
    Rustrover,
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
    if let Commands::Init = cli.command {
        return init_command(&cwd);
    }
    let path = config::find(cli.config.as_deref(), &cwd)?;
    let config = config::load(&path)?;
    let flag = cli.backend.map(Backend::from);
    match cli.command {
        Commands::Init => unreachable!("handled above"),
        Commands::Config => show_config(&config, flag),
        Commands::Flash {
            no_verify,
            no_run,
            write_otp,
        } => {
            check_images(&config)?;
            match select(&config, flag)? {
                Backend::OpenOcd => openocd::flash(&config, !no_verify, !no_run, write_otp),
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
                    trace32(&config)?.flash().map_err(text)?;
                    Ok(0)
                }
            }
        }
        Commands::Load => match select(&config, flag)? {
            Backend::Trace32 => {
                trace32(&config)?.load_symbols().map_err(text)?;
                Ok(0)
            }
            Backend::OpenOcd => Err(
                "'load' is for TRACE32; with OpenOCD the debugger (GDB) reads the symbols \
                 from the ELF itself"
                    .into(),
            ),
        },
        Commands::Debug { keep } => match select(&config, flag)? {
            Backend::OpenOcd => openocd::debug(&config, keep),
            Backend::Trace32 => {
                if keep {
                    return Err("--keep applies to the openocd backend only".into());
                }
                trace32_debug(&config)
            }
        },
        Commands::Adapter => {
            if flag == Some(Backend::OpenOcd) {
                return Err("'adapter' is TRACE32's DAP adapter; use 'rvdbg debug'".into());
            }
            trace32_debug(&config)
        }
        Commands::Vscode => {
            let exe = current_exe()?;
            trace32(&config)?.install_vscode(&exe).map_err(text)?;
            if config.openocd.is_some() {
                openocd::vscode(&config, &exe)?;
            }
            Ok(0)
        }
        Commands::Rustrover => {
            trace32(&config)?
                .install_rustrover(&current_exe()?)
                .map_err(text)?;
            Ok(0)
        }
    }
}

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// `--backend`, else `project.backend`, with where it came from.
fn fixed(config: &Config, flag: Option<Backend>) -> Option<(Backend, &'static str)> {
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
    if cwd.join("trace32.toml").is_file() {
        println!(
            "{}",
            style.dim("  images and TRACE32 settings taken from trace32.toml")
        );
    }
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
    Project::from_document(&config.trace32, &config.file).map_err(text)
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
/// user's PATH.
fn current_exe() -> Result<PathBuf, String> {
    let exe =
        std::env::current_exe().map_err(|error| format!("cannot find rvdbg itself: {error}"))?;
    Ok(exe.canonicalize().unwrap_or(exe))
}

/// The run directory with a `.gitignore` that ignores everything in it.
fn run_dir(config: &Config) -> Result<PathBuf, String> {
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
