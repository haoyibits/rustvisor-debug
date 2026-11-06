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
use probecore::target::{Target, TargetAddress, hex32};
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
    Mem {
        location: String,
        count: u32,
    },
    Attach,
    Reset,
    RunUntil {
        target: String,
        timeout: Duration,
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
            Request::Mem { .. } => "mem",
            Request::Attach => "attach",
            Request::Reset => "reset",
            Request::RunUntil { .. } => "run-until",
        }
    }
}

/// Run `request` through `server`. `cancel` says when Ctrl-C asked to stop
/// waiting (`run-until`).
pub fn run(
    config: &Config,
    project: &Project,
    server: &mut stellarbridge::Server,
    request: Request,
    cancel: &dyn Fn() -> bool,
) -> Result<Outcome, String> {
    let style = Style::stdout();
    let openocd = config.openocd()?;
    let mut target = core_target(server, openocd, symbols(config))?;
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
        Request::Mem { location, count } => mem(&mut target, &location, count, style),
        Request::Attach => {
            // OpenOCD's init examines the target without resetting it.
            let mut outcome = status(&mut target, style)?;
            outcome.text = format!("{}\n{}", style.good("attached without reset"), outcome.text);
            Ok(outcome)
        }
        Request::Reset => {
            target.reset_halt().map_err(text)?;
            let mut outcome = status(&mut target, style)?;
            outcome.text = format!(
                "{}\n{}",
                style.good("reset, halted at the reset vector"),
                outcome.text
            );
            Ok(outcome)
        }
        Request::RunUntil {
            target: location,
            timeout,
        } => run_until(&mut target, &location, timeout, cancel, style),
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

/// Most words `mem` reads at once, as in tracebridge.
const MEM_MAX_WORDS: u32 = 4096;

/// An address in the data-file notation (`AD:0x60000000`, `C15:0x4025`), a
/// bare hexadecimal address, or a symbol of the ELF files; and the symbol.
fn location(target: &CoreTarget, text: &str) -> Result<(TargetAddress, Option<String>), String> {
    if let Some(address) = TargetAddress::parse(text) {
        return Ok((address, None));
    }
    if let Some(value) = probecore::text::parse_int_auto(text).and_then(|v| u64::try_from(v).ok()) {
        return Ok((
            TargetAddress {
                class: "AD".into(),
                value,
            },
            None,
        ));
    }
    let (name, offset) = match text.split_once('+') {
        Some((name, offset)) => (
            name.trim(),
            probecore::text::parse_int_auto(offset.trim())
                .and_then(|v| u64::try_from(v).ok())
                .ok_or_else(|| format!("invalid offset in {text}"))?,
        ),
        None => (text, 0),
    };
    let (start, _) = target
        .symbols
        .range(name)
        .ok_or_else(|| format!("{name} is neither an address nor a symbol of the ELF files"))?;
    Ok((
        TargetAddress {
            class: "AD".into(),
            value: start + offset,
        },
        Some(text.to_string()),
    ))
}

/// `count` 32-bit words at an address or a symbol, as tracebridge's `mem`.
fn mem(target: &mut CoreTarget, text: &str, count: u32, style: Style) -> Result<Outcome, String> {
    if count == 0 || count > MEM_MAX_WORDS {
        return Err(format!("count must be between 1 and {MEM_MAX_WORDS}"));
    }
    let (base, symbol) = location(target, text)?;
    let words: Vec<u64> = if base.is_coprocessor() {
        if count > 1 {
            return Err(format!("{base} is a coprocessor register; read one word"));
        }
        vec![u64::from(target.read_u32(&base).map_err(text_of)?)]
    } else {
        let bytes = target
            .read_memory(base.value, count as usize * 4)
            .map_err(|error| format!("cannot read {base}: {error}"))?;
        bytes
            .chunks(4)
            .map(|word| u64::from(u32::from_le_bytes(word.try_into().expect("4 bytes"))))
            .collect()
    };
    let mut lines = Vec::new();
    if let Some(symbol) = &symbol {
        lines.push(format!(
            "{} = {}",
            style.symbol(symbol),
            style.address(&base)
        ));
    }
    for (row, chunk) in words.chunks(4).enumerate() {
        let offset = row as u64 * 16;
        let values: Vec<String> = chunk
            .iter()
            .map(|w| match w {
                0 => style.dim(format!("{w:08X}")),
                _ => format!("{w:08X}"),
            })
            .collect();
        lines.push(format!(
            "{}  {}  {}",
            style.address(base.offset(offset)),
            style.dim(format!("+0x{offset:03X}")),
            values.join(" ")
        ));
    }
    Ok(Outcome::ok(
        lines.join("\n"),
        json!({
            "symbol": symbol,
            "address": base.to_string(),
            "words": words,
            "hex": words.iter().map(|w| format!("0x{w:08X}")).collect::<Vec<_>>(),
        }),
    ))
}

fn text_of(error: probecore::target::Error) -> String {
    error.message
}

/// Run a halted core to a symbol or address with a hardware breakpoint.
fn run_until(
    target: &mut CoreTarget,
    text: &str,
    timeout: Duration,
    cancel: &dyn Fn() -> bool,
    style: Style,
) -> Result<Outcome, String> {
    let (address, _) = location(target, text)?;
    if address.is_coprocessor() {
        return Err(format!("{text} is not a code address"));
    }
    let address = address.value;
    let result = target
        .run_until(address, timeout, cancel)
        .map_err(text_of)?;
    let target_text = format!("{} ({})", style.symbol(text), hex32(address));
    let Some(pc) = result.pc else {
        let breakpoint = if result.breakpoint_removed {
            "the breakpoint was removed"
        } else {
            "the breakpoint stays set until the core stops ('rvdbg halt' stops it)"
        };
        let why = if result.cancelled {
            "interrupted before reaching it".to_string()
        } else {
            format!("not reached within {}s", timeout.as_secs())
        };
        return Err(format!(
            "{text}: {why}; the core keeps running and {breakpoint}"
        ));
    };
    let pc_symbol = target.symbolize(pc);
    let reached = pc == address;
    let message = if reached {
        format!(
            "{} {target_text} after {} ms",
            style.good("reached"),
            result.waited.as_millis()
        )
    } else {
        format!(
            "{} at {}, not at {target_text}; the breakpoint is gone",
            style.bad("stopped"),
            with_symbol(style, pc, &pc_symbol)
        )
    };
    Ok(Outcome {
        text: message,
        json: json!({
            "target": text,
            "address": address,
            "hex": hex32(address),
            "reached": reached,
            "waited_ms": result.waited.as_millis() as u64,
            "command": format!("bp 0x{address:X} 4 hw; resume"),
            "pc": {"value": pc, "hex": hex32(pc), "symbol": fault::symbol_json(&pc_symbol)},
        }),
        code: if reached { 0 } else { EXIT_FAILED },
    })
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
/// from the library (`~/.config/rvdbg/clock`), which both
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
pub fn rtt(
    config: &Config,
    project: &Project,
    server: &mut stellarbridge::Server,
    options: RttOptions,
) -> Result<u8, String> {
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
    let mut memory = ApMemory { server, memory_ap };
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
