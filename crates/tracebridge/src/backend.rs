//! Operations required by the rustvisor board runner. Board policy stays in the caller.

use std::path::{Path, PathBuf};
use std::time::Duration;

use t32rcl::{Address, Debugger};

use crate::config::{self, Config};
use crate::debug::{self, Outcome, commands, probe};
use crate::errors::Result;
use crate::{bridge_error, flash, pycompat, style::Style, target};

pub use crate::debug::check::{
    Options as CheckOptions, Report as CheckReport, Status as CheckStatus,
};

/// Version of the backend code linked into a runner, rather than a separate CLI on PATH.
pub const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("TRACEBRIDGE_GIT_HASH"),
    ")"
);

/// The RTT terminal loop, for any memory access that works while the core
/// runs (another backend's).
pub use crate::rtt::{Terminal as RttTerminal, terminal as rtt_terminal};

/// Where clock tree descriptions are looked up by chip:
/// `~/.config/rvdbg/clock` (or under `XDG_CONFIG_HOME`).
pub fn clock_library_dir() -> PathBuf {
    debug::clock::library_dir(&pycompat::process_env())
}

/// Resolved project configuration. Loading it never connects to PowerView.
#[derive(Clone)]
pub struct Project(Config);

impl Project {
    pub fn load(path: &Path) -> Result<Self> {
        let env = pycompat::process_env();
        let cwd = std::env::current_dir().map_err(|e| bridge_error!("{e}"))?;
        let path = config::find_config_file(Some(path), &cwd, &env)?;
        Ok(Self(config::load_config(&path, &env)?))
    }

    /// Resolved configuration and external descriptions used by acceptance evidence.
    /// This validates the selected files without connecting to the target.
    pub fn evidence_inputs(&self) -> Result<(String, Vec<PathBuf>)> {
        let env = pycompat::process_env();
        let choice = target::validate_target_action(&self.0, target::Action::Flash, &env)?
            .ok_or_else(|| bridge_error!("flash script was not selected"))?;
        let chip = flash::chip_name(&self.0).ok_or_else(|| bridge_error!("missing chip name"))?;
        let (clock, _) = debug::clock::find(chip, &debug::clock::library_dir(&env))
            .map_err(|e| bridge_error!("{e}"))?;
        let script = choice.script();
        let script = if let Some(relative) = script.strip_prefix("~~/") {
            self.0.t32_sys.join(relative)
        } else {
            PathBuf::from(script)
        };
        let mut effective = self.0.clone();
        // Evidence directories vary each run; loaded content is keyed separately.
        effective.elf = PathBuf::new();
        effective.extra_elf.clear();
        Ok((
            format!("{effective:?}"),
            vec![self.0.config_file.clone(), clock, script],
        ))
    }

    /// The configuration from a parsed document with the sections of
    /// trace32.toml (`project`, `target`, `flash`, `rtos`, `trace32`, `rtt`,
    /// `clock`), e.g. assembled by a front end from its own file.
    /// `config_file` is that file: relative paths are relative to its
    /// directory, and the IDE files and the PowerView toolbar run
    /// `<exe> --config <config_file> adapter|flash|load`.
    pub fn from_document(document: &toml::Table, config_file: &Path) -> Result<Self> {
        let env = pycompat::process_env();
        Ok(Self(config::config_from_document(
            document,
            config_file,
            &env,
        )?))
    }

    /// `<project>/.tracebridge`: the PowerView log and the toolbar script.
    pub fn run_dir(&self) -> &Path {
        &self.0.run_dir
    }

    /// Start PowerView unless one answers on the RCL port, then flash the
    /// images, load the symbols and run (`tracebridge flash`).
    pub fn flash(&self) -> Result<()> {
        crate::cli::open(
            &self.0,
            Some(target::Action::Flash),
            &pycompat::process_env(),
        )
    }

    /// `flash` with the flash script chosen for `chip`, or with `script`
    /// (`tracebridge flash --chip/--script`).
    pub fn flash_with(&self, chip: Option<String>, script: Option<String>) -> Result<()> {
        let mut config = self.0.clone();
        if let Some(chip) = chip {
            config.flash_script.clear();
            config.flash_chip = chip;
        }
        if let Some(script) = script {
            config.flash_script = script;
        }
        crate::cli::open(
            &config,
            Some(target::Action::Flash),
            &pycompat::process_env(),
        )
    }

    /// The flash scripts for a chip (`tracebridge chips`); without a query,
    /// for the configured chip.
    pub fn chips(&self, query: Option<&str>) -> Result<()> {
        crate::cli::chips(&self.0, query, &pycompat::process_env())
    }

    /// Start PowerView if needed, then load the symbols without flashing or
    /// resetting (`tracebridge load`).
    pub fn load_symbols(&self) -> Result<()> {
        crate::cli::open(
            &self.0,
            Some(target::Action::Load),
            &pycompat::process_env(),
        )
    }

    /// Start PowerView if needed (`tracebridge open`).
    pub fn open(&self) -> Result<()> {
        crate::cli::open(&self.0, None, &pycompat::process_env())
    }

    /// Run the DAP proxy in front of t32debugadapter until the IDE
    /// disconnects or Ctrl-C (`tracebridge adapter`). PowerView must run.
    pub fn run_adapter(&self) -> Result<()> {
        crate::cli::adapter(&self.0)
    }

    /// Merge the TRACE32 attach configuration into `.vscode/` (`tracebridge
    /// vscode`); its task runs `<exe> --config <config_file> adapter`.
    pub fn install_vscode(&self, exe: &Path) -> Result<()> {
        crate::vscode::installer::install(&self.0, exe)
    }

    /// Write `.run/TRACE32 Attach.run.xml` (`tracebridge rustrover`); it runs
    /// `<exe> --config <config_file> adapter`.
    pub fn install_rustrover(&self, exe: &Path) -> Result<PathBuf> {
        crate::rustrover::install(&self.0, exe)
    }

    /// `tracebridge debug <args>` with this configuration: one command (or
    /// the interactive session without one) on the running PowerView. The
    /// result is the exit code (0 ok, 2 usage, 3 a check failed or verify
    /// found a difference); errors that stop the command are `Err`.
    pub fn debug(&self, args: Vec<String>) -> Result<i32> {
        if let Some(code) = debug::main_without_config(&args) {
            return Ok(code);
        }
        let cwd = std::env::current_dir()
            .map_err(|error| bridge_error!("cannot read the current directory: {error}"))?;
        debug::main(&self.0, &cwd, args)
    }

    /// `tracebridge rtt <args>`: the RTT terminal through the running
    /// PowerView until Ctrl-C. Usage errors exit like any clap parser.
    pub fn rtt(&self, args: &[String]) -> Result<()> {
        let args = crate::rtt::parse_args(args);
        if !crate::powerview::port_open(self.0.rcl_port) {
            return Err(bridge_error!(
                "no PowerView on RCL port {}; flash or load first, or start PowerView",
                self.0.rcl_port
            ));
        }
        crate::rtt::run(&self.0, args)
    }

    /// The chip name: `flash.chip`, else `target.cpu`.
    pub fn chip_name(&self) -> Option<String> {
        flash::chip_name(&self.0).map(str::to_string)
    }

    /// The board's clock frequencies (`[clock]`).
    pub fn clock_inputs(&self) -> &[(String, f64)] {
        &self.0.clock
    }

    /// The RTT settings: control-block symbol, fixed control-block address,
    /// poll interval in seconds.
    pub fn rtt_settings(&self) -> (&str, Option<u64>, f64) {
        (
            &self.0.rtt_symbol,
            self.0.rtt_control_block_address,
            self.0.rtt_poll_interval,
        )
    }

    /// Explicit images override project defaults, including an empty extra-image list.
    pub fn images(&mut self, elf: PathBuf, extra: Vec<PathBuf>, program: String) {
        self.0.elf = elf;
        self.0.extra_elf = extra;
        self.0.program = program;
    }
}

/// One serial owner of a PowerView connection. Operations are never retried automatically.
pub struct Session {
    project: Project,
    debugger: Debugger,
    per: probe::PerSnapshot,
}

impl Session {
    /// Connect to an existing PowerView without starting it or resetting the target.
    pub fn connect(project: Project) -> Result<Self> {
        let debugger = debug::connect(&project.0).map_err(|e| bridge_error!("{e}"))?;
        Ok(Self {
            project,
            debugger,
            per: probe::PerSnapshot::default(),
        })
    }

    pub fn images(&mut self, elf: PathBuf, extra: Vec<PathBuf>, program: String) {
        self.project.images(elf, extra, program);
        self.per.invalidate();
    }

    fn context(&mut self) -> commands::Context<'_> {
        commands::Context {
            probe: &mut self.debugger,
            per: &mut self.per,
            config: &self.project.0,
            cwd: &self.project.0.project_dir,
            style: Style::PLAIN,
        }
    }

    pub fn reset(&mut self) -> Result<()> {
        self.per.invalidate();
        commands::reset(&mut self.context()).map_err(|e| bridge_error!("{e}"))?;
        Ok(())
    }

    pub fn flash(&mut self) -> Result<()> {
        let env = pycompat::process_env();
        let choice = target::validate_target_action(&self.project.0, target::Action::Flash, &env)?
            .ok_or_else(|| bridge_error!("flash script was not selected"))?;
        let mut config = self.project.0.clone();
        if let Some(arg) = choice.cpu_argument(&config.flash_args) {
            config.flash_args.push(arg);
        }
        self.per.invalidate();
        target::run_sequence(&config, Some(&choice.script()), &mut self.debugger)
    }

    /// Same symbol loading and resume behavior as `tracebridge load`.
    pub fn load(&mut self) -> Result<()> {
        target::validate_target_action(
            &self.project.0,
            target::Action::Load,
            &pycompat::process_env(),
        )?;
        self.per.invalidate();
        target::run_sequence(&self.project.0, None, &mut self.debugger)
    }

    pub fn verify(&mut self) -> Result<serde_json::Value> {
        let result =
            commands::verify(&mut self.context(), None, false).map_err(|e| bridge_error!("{e}"))?;
        success(result)
    }

    /// Check failures are returned in the report; transport/execution failures are errors.
    pub fn check(&mut self, file: &Path, options: &CheckOptions) -> Result<CheckReport> {
        commands::check_report(&mut self.context(), file, options).map_err(|e| bridge_error!("{e}"))
    }

    pub fn clock(&mut self) -> Result<serde_json::Value> {
        let result = commands::clock(&mut self.context(), &[], None, false)
            .map_err(|e| bridge_error!("{e}"))?;
        success(result)
    }

    pub fn resume(&mut self) -> Result<()> {
        self.per.invalidate();
        if !self
            .debugger
            .state_run()
            .map_err(|e| bridge_error!("{e}"))?
        {
            self.debugger.cmd("Go").map_err(|e| bridge_error!("{e}"))?;
        }
        Ok(())
    }

    pub fn command(&mut self, command: &str) -> Result<()> {
        self.per.invalidate();
        commands::command(&mut self.context(), command, "command executed")
            .map_err(|e| bridge_error!("{e}"))?;
        Ok(())
    }

    pub fn run_script(
        &mut self,
        script: &str,
        arguments: &[String],
        timeout: Duration,
    ) -> Result<()> {
        self.per.invalidate();
        commands::run_script(&mut self.context(), script, arguments, timeout)
            .map_err(|e| bridge_error!("{e}"))?;
        Ok(())
    }

    pub fn run_until(&mut self, location: &str, timeout: Duration) -> Result<()> {
        self.per.invalidate();
        let result = commands::run_until(&mut self.context(), location, timeout)
            .map_err(|e| bridge_error!("{e}"))?;
        success(result)?;
        Ok(())
    }

    pub fn trace(&mut self, last: u32) -> Result<serde_json::Value> {
        let result =
            commands::trace(&mut self.context(), last).map_err(|e| bridge_error!("{e}"))?;
        success(result)
    }

    /// Bulk little-endian words from ordinary RAM via AD access. Never use for MMIO/CP15.
    /// The caller must know that the requested range is readable RAM.
    pub fn read_ram_words(&mut self, address: u64, count: usize) -> Result<Vec<u32>> {
        let length = ram_length(address, count)?;
        let bytes = self
            .debugger
            .memory_read(&Address::new(Some("AD"), address), length)
            .map_err(|e| bridge_error!("read RAM at {address:#x}: {e}"))?;
        Ok(bytes
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect())
    }

    pub fn write_ram_word(&mut self, address: u64, value: u32) -> Result<()> {
        ram_length(address, 1)?;
        self.debugger
            .memory_write_u32(&Address::new(Some("AD"), address), value)
            .map_err(|e| bridge_error!("write RAM at {address:#x}: {e}"))
    }
}

fn ram_length(address: u64, count: usize) -> Result<usize> {
    let length = count
        .checked_mul(4)
        .filter(|n| *n > 0 && *n <= 16 * 1024 * 1024)
        .ok_or_else(|| bridge_error!("RAM read must contain 1..4194304 words"))?;
    if address % 4 != 0 || address.checked_add(length as u64).is_none() {
        return Err(bridge_error!("unaligned or overflowing RAM range"));
    }
    Ok(length)
}

fn success(result: Outcome) -> Result<serde_json::Value> {
    if result.code != 0 {
        return Err(bridge_error!("{}", result.text));
    }
    Ok(result.json)
}

/// Which of the two VS Code files [`merge_vscode_file`] writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VscodeFile {
    /// `tasks.json`: entries are matched by `label`.
    Tasks,
    /// `launch.json`: entries are matched by `name`.
    Launch,
}

/// Merge `template` (a whole `tasks.json` or `launch.json` document) into
/// `path`: entries with the same label or name are replaced, all others are
/// kept, comments and trailing commas in the existing file are accepted,
/// the old file is backed up and the new one written atomically. Prints
/// what it did, as `tracebridge vscode` does.
pub fn merge_vscode_file(
    path: &Path,
    file: VscodeFile,
    template: &serde_json::Value,
) -> Result<Option<PathBuf>> {
    let kind = match file {
        VscodeFile::Tasks => crate::vscode::installer::Kind::Tasks,
        VscodeFile::Launch => crate::vscode::installer::Kind::Launch,
    };
    crate::vscode::installer::merge_into(kind, path, template)
}

/// The same ELF parser used by verify: load addresses and bytes, excluding debug information.
pub fn loadable_content(path: &Path) -> Result<Vec<(u64, Vec<u8>)>> {
    let bytes = std::fs::read(path).map_err(|e| bridge_error!("{}: {e}", path.display()))?;
    let segments = debug::elf::load_segments(&bytes).map_err(|e| bridge_error!("{e}"))?;
    Ok(segments.into_iter().map(|s| (s.paddr, s.data)).collect())
}

/// Validate a check file and enumerate the selected names without a target connection.
/// A misspelled variant must not silently select only the unconditional checks.
pub fn check_names(path: &Path, variant: Option<&str>) -> Result<Vec<String>> {
    let file = debug::check::load(path).map_err(|e| bridge_error!("{e}"))?;
    if let Some(variant) = variant {
        if file.checks.iter().any(|c| c.variants.is_some())
            && !file.checks.iter().any(|c| {
                c.variants
                    .as_ref()
                    .is_some_and(|vs| vs.iter().any(|v| v == variant))
            })
        {
            return Err(bridge_error!(
                "{}: unknown variant {variant}",
                path.display()
            ));
        }
    }
    Ok(file
        .checks
        .into_iter()
        .filter(|c| {
            c.variants
                .as_ref()
                .is_none_or(|vs| variant.is_some_and(|v| vs.iter().any(|x| x == v)))
        })
        .map(|c| c.name)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_ram_ranges_before_access() {
        assert!(ram_length(1, 1).is_err());
        assert!(ram_length(0, 0).is_err());
        assert!(ram_length(u64::MAX - 3, 2).is_err());
        assert!(ram_length(0, usize::MAX).is_err());
        assert_eq!(ram_length(0x60010000, 14076).unwrap(), 56304);
    }
}
