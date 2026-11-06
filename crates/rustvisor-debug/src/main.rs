//! `rvdbg`: rustvisor-specific debugging front end. It reads `rvdbg.toml`
//! and runs each command on the backend the project chose: TRACE32 through
//! tracebridge, or StellarLINK through OpenOCD (stellarbridge).
//!
//! The TRACE32 IDE files and the PowerView toolbar run
//! `<exe> --config <file> adapter|flash|load`, the way tracebridge's do, so
//! rvdbg accepts the same command names.
#![forbid(unsafe_code)]

mod config;
mod init;
mod openocd;

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

#[derive(Debug, Subcommand)]
enum Commands {
    /// Write rvdbg.toml in the current directory (values from trace32.toml if there is one)
    Init {
        #[arg(long, value_enum)]
        backend: BackendArg,
    },
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
    #[command(alias = "adapter")]
    Debug {
        /// OpenOCD: keep serving after GDB detaches (until Ctrl-C)
        #[arg(long)]
        keep: bool,
    },
    /// Write the debug configuration into .vscode/ (launch.json, tasks.json)
    Vscode,
    /// TRACE32: write the RustRover run configuration (.run/, LSP4IJ)
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
    if let Commands::Init { backend } = cli.command {
        return init_command(&cwd, backend);
    }
    let path = config::find(cli.config.as_deref(), &cwd)?;
    let config = config::load(&path)?;
    match cli.command {
        Commands::Init { .. } => unreachable!("handled above"),
        Commands::Flash {
            no_verify,
            no_run,
            write_otp,
        } => {
            check_images(&config)?;
            match config.backend {
                Backend::OpenOcd => openocd::flash(&config, !no_verify, !no_run, write_otp),
                Backend::Trace32 => {
                    for (set, flag) in [
                        (no_verify, "--no-verify"),
                        (no_run, "--no-run"),
                        (write_otp, "--write-otp"),
                    ] {
                        if set {
                            return Err(format!("{flag} applies to the openocd backend only"));
                        }
                    }
                    trace32(&config)?.flash().map_err(text)?;
                    Ok(0)
                }
            }
        }
        Commands::Load => match config.backend {
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
        Commands::Debug { keep } => match config.backend {
            Backend::OpenOcd => openocd::debug(&config, keep),
            Backend::Trace32 => {
                if keep {
                    return Err("--keep applies to the openocd backend only".into());
                }
                let project = trace32(&config)?;
                // The adapter needs PowerView; start it like 'flash' would.
                project.open().map_err(text)?;
                project.run_adapter().map_err(text)?;
                Ok(0)
            }
        },
        Commands::Vscode => match config.backend {
            Backend::OpenOcd => openocd::vscode(&config, &current_exe()?),
            Backend::Trace32 => {
                trace32(&config)?
                    .install_vscode(&current_exe()?)
                    .map_err(text)?;
                Ok(0)
            }
        },
        Commands::Rustrover => match config.backend {
            Backend::Trace32 => {
                trace32(&config)?
                    .install_rustrover(&current_exe()?)
                    .map_err(text)?;
                Ok(0)
            }
            Backend::OpenOcd => Err(
                "RustRover is set up for the trace32 backend only so far; with openocd use \
                 'rvdbg vscode', or a GDB Remote Debug configuration on localhost:<gdb_port> \
                 with 'rvdbg debug' running"
                    .into(),
            ),
        },
    }
}

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn init_command(cwd: &Path, backend: BackendArg) -> Result<u8, String> {
    let backend = match backend {
        BackendArg::Trace32 => Backend::Trace32,
        BackendArg::Openocd => Backend::OpenOcd,
    };
    let path = init::init(cwd, backend)?;
    let style = Style::stdout();
    println!("{} {}", style.good("wrote"), path.display());
    if cwd.join("trace32.toml").is_file() {
        println!(
            "{}",
            style.dim("  images and TRACE32 settings taken from trace32.toml")
        );
    }
    println!("\nNext:");
    for (command, note) in [
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
