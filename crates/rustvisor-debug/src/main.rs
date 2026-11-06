//! `rvdbg`: rustvisor-specific debugging front end. It picks the backend
//! from the project configuration; so far only `flash` with OpenOCD.
#![forbid(unsafe_code)]

mod config;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use probecore::style::Style;

use config::{Backend, Config};

/// Exit code when `flash` wrote the images but the comparison found
/// differences (the same code as a failed check).
const EXIT_FAILED: u8 = 3;

#[derive(Debug, Parser)]
#[command(
    name = "rvdbg",
    version,
    about = "Debug rustvisor on its board (TRACE32 or StellarLINK)"
)]
struct Cli {
    /// Project configuration (default: the nearest rvdbg.toml)
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Erase and write the ELF and the extra images, compare them with flash, then run
    Flash {
        /// Do not compare the written segments with flash
        #[arg(long)]
        no_verify: bool,
        /// Leave the core halted after flashing instead of `reset run`
        #[arg(long)]
        no_run: bool,
        /// Write bytes of one-time-programmable areas (openocd.otp) that differ
        /// from the board; this changes the board permanently
        #[arg(long)]
        write_otp: bool,
    },
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
    let path = config::find(cli.config.as_deref(), &cwd)?;
    let config = config::load(&path)?;
    match cli.command {
        Commands::Flash {
            no_verify,
            no_run,
            write_otp,
        } => flash(&config, !no_verify, !no_run, write_otp),
    }
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

fn flash(config: &Config, verify: bool, run: bool, write_otp: bool) -> Result<u8, String> {
    let style = Style::stdout();
    let images = config.images();
    for image in &images {
        if !image.is_file() {
            return Err(format!(
                "{} does not exist; build it first",
                image.display()
            ));
        }
    }
    let openocd = match config.backend {
        Backend::Trace32 => {
            return Err(
                "the TRACE32 backend is not wired into rvdbg yet; use 'tracebridge flash'".into(),
            );
        }
        Backend::OpenOcd => config.openocd.as_ref().expect("checked when loading"),
    };
    if openocd.memory_ap.is_none() {
        if !openocd.otp.is_empty() {
            return Err(
                "openocd.otp needs openocd.memory_ap (the MEM-AP target that reads physical \
                 memory) to check the OTP areas before writing"
                    .into(),
            );
        }
        if verify {
            return Err(
                "comparing needs openocd.memory_ap (the MEM-AP target that reads physical \
                 memory); set it or pass --no-verify"
                    .into(),
            );
        }
    }
    let log = run_dir(config)?.join("openocd.log");
    let server_config = stellarbridge::ServerConfig {
        executable: openocd.executable.clone(),
        search: openocd.search.clone(),
        config: openocd.config.clone(),
        commands: openocd.commands.clone(),
        tcl_port: openocd.tcl_port,
        log: log.clone(),
        startup_timeout: Duration::from_secs(30),
    };
    println!(
        "{} {} {}",
        style.label("openocd"),
        openocd.executable.display(),
        server_config.arguments().join(" ")
    );
    let mut server = stellarbridge::Server::start(&server_config).map_err(|e| e.0)?;
    let options = stellarbridge::FlashOptions {
        memory_ap: openocd.memory_ap.clone(),
        verify,
        otp: openocd
            .otp
            .iter()
            .map(|&(start, size)| stellarbridge::OtpRange { start, size })
            .collect(),
        write_otp,
        run,
        timeout: Duration::from_secs(openocd.timeout),
    };
    let started = std::time::Instant::now();
    let report = stellarbridge::flash(&mut server, &images, &options, &mut |step| {
        println!("{} {step}", style.label("step"));
    })
    .map_err(|e| e.0)?;
    server.shutdown();

    for check in &report.otp {
        let status = if check.differing == 0 {
            style.good("same as on the board")
        } else {
            style.warn(format!("{} bytes changed (--write-otp)", check.differing))
        };
        println!(
            "  {}  {}  {:>7} bytes  OTP {status}",
            short(&check.image),
            style.address(format!("0x{:08X}", check.address)),
            check.size
        );
    }
    for (image, segments) in &report.verified {
        for segment in segments {
            let status = if segment.differing == 0 {
                style.good("match")
            } else {
                style.bad(format!(
                    "MISMATCH {} bytes, first at 0x{:08X}",
                    segment.differing,
                    segment.first_difference.unwrap_or_default()
                ))
            };
            println!(
                "  {}  {}  {:>7} bytes  {status}",
                short(image),
                style.address(format!("0x{:08X}", segment.paddr)),
                segment.size
            );
        }
    }
    let seconds = started.elapsed().as_secs_f64();
    if report.differing() > 0 {
        println!(
            "{}",
            style.bad(format!(
                "flash differs from the images; log {}",
                log.display()
            ))
        );
        return Ok(EXIT_FAILED);
    }
    let what = if report.verified.is_empty() {
        "written (not compared)"
    } else {
        "written and compared"
    };
    let after = if run { ", running" } else { ", halted" };
    println!(
        "{}",
        style.good(format!(
            "flashed {} image(s) in {seconds:.1} s: {what}{after}",
            report.images.len()
        ))
    );
    Ok(0)
}

fn short(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}
