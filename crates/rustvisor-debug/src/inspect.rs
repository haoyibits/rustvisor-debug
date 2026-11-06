//! Inspecting the target through StellarLINK: status, halt, go, fault,
//! verify, check, clock and RTT, with the same text and JSON as tracebridge's
//! `debug` commands. They use the OpenOCD that `rvdbg debug` runs when there
//! is one (its Tcl port answers), or start their own without resetting the
//! target.

use std::path::{Path, PathBuf};
use std::time::Duration;

use probecore::check;
use probecore::clock;
use probecore::decode;
use probecore::elf::{self, SymbolTable};
use probecore::fault::{self, with_symbol};
use probecore::style::Style;
use probecore::target::{Target, hex32};
use serde_json::{Value, json};
use stellarbridge::core::{ApMemory, CoreTarget, RunState};
use tracebridge::backend::Project;

use crate::config::{Config, OpenOcd};
use crate::{EXIT_FAILED, run_dir};

/// One command's result, as tracebridge's `debug` prints it.
pub struct Outcome {
    pub text: String,
    pub json: Value,
    pub code: u8,
}

impl Outcome {
    fn ok(text: String, json: Value) -> Outcome {
        Outcome {
            text,
            json,
            code: 0,
        }
    }
}

/// Print an outcome (text, or the JSON envelope of `tracebridge debug
/// --json`) and return the exit code. An error is printed by the caller as
/// text; with `json` its envelope goes to stdout as well.
pub fn emit(command: &str, json: bool, result: Result<Outcome, String>) -> Result<u8, String> {
    match result {
        Ok(outcome) => {
            if json {
                println!(
                    "{}",
                    json!({
                        "command": command,
                        "ok": outcome.code == 0,
                        "exit_code": outcome.code,
                        "result": outcome.json,
                    })
                );
            } else if !outcome.text.is_empty() {
                println!("{}", outcome.text);
            }
            Ok(outcome.code)
        }
        Err(error) => {
            if json {
                println!(
                    "{}",
                    json!({"command": command, "ok": false, "exit_code": 1, "error": error})
                );
            }
            Err(error)
        }
    }
}

/// The OpenOCD to talk to: the running one on the Tcl port, or a new one.
pub fn connect(config: &Config) -> Result<stellarbridge::Server, String> {
    let openocd = config.openocd()?;
    if stellarbridge::Server::running(openocd.tcl_port) {
        eprintln!(
            "{} using the OpenOCD on Tcl port {}",
            Style::stderr().label("[rvdbg]"),
            openocd.tcl_port
        );
        return stellarbridge::Server::attach(openocd.tcl_port).map_err(|e| e.0);
    }
    let log = run_dir(config)?.join("openocd.log");
    let server_config = crate::openocd::server_config(openocd, Some(log), false);
    stellarbridge::Server::start(&server_config).map_err(|e| e.0)
}

/// The symbols of the images that exist; OpenOCD has none of its own.
pub fn symbols(config: &Config) -> SymbolTable {
    let mut table = SymbolTable::default();
    for image in config.images() {
        if let Ok(bytes) = std::fs::read(&image) {
            if let Err(error) = table.add_elf(&bytes) {
                eprintln!(
                    "{} {}: {error}",
                    Style::stderr().warn("rvdbg: warning:"),
                    image.display()
                );
            }
        }
    }
    table
}

fn core_target<'a>(
    server: &'a mut stellarbridge::Server,
    openocd: &OpenOcd,
    symbols: SymbolTable,
) -> Result<CoreTarget<'a>, String> {
    Ok(CoreTarget {
        server,
        core: openocd.core()?.to_string(),
        memory_ap: openocd.memory_ap.clone(),
        symbols,
    })
}

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// What `rvdbg` asks of the OpenOCD backend.
pub enum Request {
    Status,
    Halt,
    Go,
    Fault,
    Verify(Option<PathBuf>),
    Check {
        file: PathBuf,
        options: check::Options,
    },
    Clock {
        inputs: Vec<String>,
        tree: Option<PathBuf>,
    },
}

impl Request {
    pub fn name(&self) -> &'static str {
        match self {
            Request::Status => "status",
            Request::Halt => "halt",
            Request::Go => "go",
            Request::Fault => "fault",
            Request::Verify(_) => "verify",
            Request::Check { .. } => "check",
            Request::Clock { .. } => "clock",
        }
    }
}

pub fn run(config: &Config, project: &Project, request: Request) -> Result<Outcome, String> {
    let style = Style::stdout();
    let openocd = config.openocd()?;
    let mut server = connect(config)?;
    let mut target = core_target(&mut server, openocd, symbols(config))?;
    let cwd = std::env::current_dir().map_err(text)?;
    match request {
        Request::Status => status(&mut target, style),
        Request::Halt => {
            target.halt().map_err(text)?;
            let mut outcome = status(&mut target, style)?;
            outcome.text = format!("{}\n{}", style.good("halted"), outcome.text);
            Ok(outcome)
        }
        Request::Go => {
            target.resume().map_err(text)?;
            Ok(Outcome::ok(
                style.good("running"),
                json!({"state": "running"}),
            ))
        }
        Request::Fault => {
            let facts = stellarbridge::core::fault_facts(&mut target).map_err(text)?;
            let (lines, report) = fault::render(&facts, style);
            Ok(Outcome::ok(lines.join("\n"), report))
        }
        Request::Verify(elf_path) => {
            if openocd.memory_ap.is_none() {
                return Err(
                    "verify reads physical memory through openocd.memory_ap; set it".into(),
                );
            }
            let paths = match elf_path {
                Some(path) => vec![cwd.join(path)],
                None => config.images(),
            };
            verify(&mut target, &paths, style)
        }
        Request::Check { file, options } => {
            let path = cwd.join(file);
            let parsed = check::load(&path).map_err(text)?;
            let tree = if parsed.reads_clocks(options.variant.as_deref()) {
                Some(
                    clock_tree(project, None)
                        .map_err(|error| format!("read.clock: {error}"))?
                        .1,
                )
            } else {
                None
            };
            let inputs = project.clock_inputs().to_vec();
            let clocks = tree.as_ref().map(|tree| check::Clocks {
                tree,
                inputs: &inputs,
            });
            let report =
                check::run(&mut target, &parsed, &options, clocks.as_ref()).map_err(text)?;
            Ok(Outcome {
                text: report.human(style),
                json: report.to_json(),
                code: report.exit_code() as u8,
            })
        }
        Request::Clock { inputs, tree } => {
            let tree = tree.map(|path| cwd.join(path));
            let mut given = project.clock_inputs().to_vec();
            given.extend(clock::parse_inputs(&inputs).map_err(text)?);
            let (path, description) = clock_tree(project, tree.as_deref())?;
            let report = clock::evaluate(&mut target, &description, &given).map_err(text)?;
            let mut heading = path.display().to_string();
            if let Some(text) = &description.description {
                heading.push_str(&format!(" ({text})"));
            }
            let mut json = report.to_json();
            json["tree"] = json!(path.display().to_string());
            json["description"] = json!(description.description);
            Ok(Outcome {
                text: format!("{}\n{}", style.dim(heading), report.human(style)),
                json,
                code: report.exit_code() as u8,
            })
        }
    }
}

/// The run state; PC and CPSR when halted.
fn status(target: &mut CoreTarget, style: Style) -> Result<Outcome, String> {
    let state = target.run_state().map_err(text)?;
    let name = match state {
        RunState::Running => "running",
        RunState::Halted => "halted",
        RunState::Other => "other",
    };
    let shown = match state {
        RunState::Running => style.good(name),
        RunState::Halted => style.warn(name),
        RunState::Other => style.bad(name),
    };
    let mut lines = vec![format!(
        "{}     {shown}  {}",
        style.label("core"),
        style.dim(&target.core)
    )];
    let mut report = json!({"core": target.core, "state": name});
    if state == RunState::Halted {
        let pc = target.read_core_register("PC").map_err(text)?;
        let cpsr = target.read_core_register("CPSR").map_err(text)?;
        let psr = decode::decode_psr(cpsr as u32);
        let symbol = target.symbolize(pc);
        lines.push(format!(
            "{}       {}",
            style.label("PC"),
            with_symbol(style, pc, &symbol)
        ));
        lines.push(format!(
            "{}     {}  {}",
            style.label("CPSR"),
            style.value(hex32(cpsr)),
            psr.describe()
        ));
        report["pc"] =
            json!({"value": pc, "hex": hex32(pc), "symbol": fault::symbol_json(&symbol)});
        report["cpsr"] = psr.to_json();
    }
    Ok(Outcome::ok(lines.join("\n"), report))
}

fn verify(target: &mut CoreTarget, paths: &[PathBuf], style: Style) -> Result<Outcome, String> {
    let mut texts = Vec::new();
    let mut images = Vec::new();
    let mut code = 0;
    for path in paths {
        let bytes =
            std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let segments =
            elf::load_segments(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        if segments.is_empty() {
            return Err(format!(
                "{}: no loadable segments with file content",
                path.display()
            ));
        }
        let results = elf::compare(target, &segments).map_err(text)?;
        let report = elf::verify_report(path, &results, style);
        if !report.matched {
            code = EXIT_FAILED;
        }
        texts.push(report.lines.join("\n"));
        images.push(json!({
            "elf": path.display().to_string(),
            "match": report.matched,
            "differing_bytes": report.differing,
            "first_difference": report.first_difference,
            "segments": report.segments,
        }));
    }
    let json = if images.len() == 1 {
        images.remove(0)
    } else {
        json!({"match": code == 0, "images": images})
    };
    Ok(Outcome {
        text: texts.join("\n"),
        json,
        code,
    })
}

/// The clock tree description: the given file, or the one chosen by chip
/// from tracebridge's library (`~/.config/tracebridge/clock`), which both
/// backends share.
fn clock_tree(project: &Project, tree: Option<&Path>) -> Result<(PathBuf, clock::Tree), String> {
    match tree {
        Some(path) => Ok((path.to_path_buf(), clock::load(path).map_err(text)?)),
        None => {
            let Some(chip) = project.chip_name() else {
                return Err(
                    "no chip name: set trace32.target.cpu (or trace32.flash.chip) in rvdbg.toml, \
                     or pass --tree <file>"
                        .into(),
                );
            };
            clock::find(&chip, &tracebridge::backend::clock_library_dir()).map_err(text)
        }
    }
}

/// How `rvdbg rtt` runs.
pub struct RttOptions {
    pub symbol: Option<String>,
    pub control_block: Option<u64>,
    pub poll: Option<f64>,
    pub replay: bool,
    pub output_only: bool,
}

/// The RTT terminal through the MEM-AP, so the core keeps running.
pub fn rtt(config: &Config, project: &Project, options: RttOptions) -> Result<u8, String> {
    let openocd = config.openocd()?;
    let Some(memory_ap) = openocd.memory_ap.clone() else {
        return Err("RTT reads and writes memory through openocd.memory_ap; set it".into());
    };
    let (default_symbol, default_address, default_poll) = project.rtt_settings();
    let symbol = options.symbol.unwrap_or_else(|| default_symbol.to_string());
    let poll = options.poll.unwrap_or(default_poll);
    if !(poll > 0.0 && poll.is_finite()) {
        return Err("--poll must be positive".into());
    }
    let control_block = match options.control_block.or(default_address) {
        Some(address) => address,
        None => symbols(config)
            .range(&symbol)
            .map(|(start, _)| start)
            .ok_or_else(|| {
                format!(
                    "{symbol} is not in the symbol table of {}; build the images, or pass \
                     --cb 0x<address>",
                    config
                        .images()
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?,
    };
    let mut server = connect(config)?;
    let mut memory = ApMemory {
        server: &mut server,
        memory_ap,
    };
    tracebridge::backend::rtt_terminal(
        &mut memory,
        &tracebridge::backend::RttTerminal {
            probe: "StellarLINK",
            symbol: &symbol,
            control_block,
            poll: Duration::from_secs_f64(poll),
            replay: options.replay,
            output_only: options.output_only,
        },
    )
    .map_err(text)?;
    Ok(0)
}
