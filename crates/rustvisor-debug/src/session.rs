//! `rvdbg` without a command: an interactive session. It finds rvdbg.toml,
//! picks the backend from the connected probe, and then runs one command per
//! line (`status`, `mem _SEGGER_RTT 4`, `flash`), the same commands as on the
//! command line; a leading `rvdbg` is accepted too.
//!
//! With StellarLINK the session keeps one OpenOCD for all commands, with its
//! GDB server open, so GDB or VS Code can attach while the session runs. With
//! TRACE32 each command talks to PowerView as on the command line.
//!
//! Ctrl-C stops a waiting command (`rtt`, `run-until`), not the session;
//! `quit`, `exit` or Ctrl-D end it.

use std::borrow::Cow;
use std::io::{BufRead, IsTerminal};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::{CommandFactory, Parser};
use rustyline::completion::Completer;
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Editor, Helper};

use probecore::style::Style;
use stellarbridge::core::{CoreTarget, RunState};

use crate::config::{self, Backend, Config};
use crate::{Cli, Commands, Env, SessionLink, execute, install_skill, probe};

/// What a command needs to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Need {
    /// Only the configuration.
    Nothing,
    /// A probe of either kind.
    Probe,
    /// TRACE32.
    Trace32,
}

/// The commands of the session, grouped as the help lists them.
const GROUPS: &[(&str, &[(&str, Need)])] = &[
    (
        "inspect",
        &[
            ("status", Need::Probe),
            ("mem", Need::Probe),
            ("fault", Need::Probe),
            ("verify", Need::Probe),
            ("check", Need::Probe),
            ("clock", Need::Probe),
            ("reg", Need::Trace32),
            ("trace", Need::Trace32),
            ("eval", Need::Trace32),
        ],
    ),
    (
        "control",
        &[
            ("halt", Need::Probe),
            ("go", Need::Probe),
            ("reset", Need::Probe),
            ("attach", Need::Probe),
            ("run-until", Need::Probe),
            ("down", Need::Trace32),
            ("do", Need::Trace32),
            ("cmd", Need::Trace32),
        ],
    ),
    (
        "program",
        &[("flash", Need::Probe), ("load", Need::Trace32)],
    ),
    ("terminal", &[("rtt", Need::Probe)]),
    (
        "project",
        &[
            ("config", Need::Nothing),
            ("debug", Need::Probe),
            ("vscode", Need::Nothing),
            ("rustrover", Need::Nothing),
            ("skill", Need::Nothing),
            ("open", Need::Trace32),
            ("chips", Need::Trace32),
        ],
    ),
];

/// The session's own commands, after the groups.
const SESSION_WORDS: &[(&str, &str)] = &[
    ("help", "help [command]"),
    ("backend", "backend trace32|openocd"),
    ("reconnect", "reconnect"),
    ("quit", "quit"),
];

fn available(need: Need, backend: Option<Backend>) -> bool {
    match need {
        Need::Nothing => true,
        Need::Probe => backend.is_some(),
        Need::Trace32 => backend == Some(Backend::Trace32),
    }
}

/// The command list, unavailable commands dimmed, with a legend.
pub fn help_text(style: Style, backend: Option<Backend>) -> String {
    let mut lines = Vec::new();
    let mut dimmed_trace32 = false;
    let mut dimmed_probe = false;
    for (group, commands) in GROUPS {
        let names: Vec<String> = commands
            .iter()
            .map(|(name, need)| {
                if available(*need, backend) {
                    style.value(name)
                } else {
                    match need {
                        Need::Trace32 if backend.is_some() => dimmed_trace32 = true,
                        _ => dimmed_probe = true,
                    }
                    style.dim(name)
                }
            })
            .collect();
        lines.push(format!(
            "  {}  {}",
            style.label(format!("{group:<8}")),
            names.join("  ")
        ));
    }
    let words: Vec<String> = SESSION_WORDS
        .iter()
        .map(|(_, usage)| style.value(usage))
        .collect();
    lines.push(format!(
        "  {}  {}",
        style.label(format!("{:<8}", "session")),
        words.join("  ")
    ));
    if dimmed_trace32 {
        lines.push(style.dim("  dimmed: TRACE32 only"));
    }
    if dimmed_probe {
        lines.push(style.dim(
            "  dimmed: no probe found; plug in a StellarLINK or a Lauterbach probe, then 'reconnect'",
        ));
    }
    lines.push(style.dim("  'help <command>' explains one; '--json' after a command prints JSON"));
    lines.join("\n")
}

/// The help of one command, as `rvdbg <command> --help` shows it.
fn command_help(name: &str, style: Style) -> Result<String, String> {
    if let Some((_, usage)) = SESSION_WORDS.iter().find(|(word, _)| *word == name) {
        let what = match name {
            "help" => "the command list, or the help of one command",
            "backend" => "use this backend from now on, whatever is connected",
            "reconnect" => "look for the probe again and connect to it (OpenOCD is restarted)",
            _ => "leave the session (also 'exit' or Ctrl-D)",
        };
        return Ok(format!("{}\n  {what}", style.value(usage)));
    }
    let mut command = Cli::command();
    let Some(sub) = command.find_subcommand_mut(name) else {
        return Err(format!("no command '{name}'; 'help' lists them"));
    };
    let help = sub.render_long_help();
    Ok(if style.enabled() {
        help.ansi().to_string()
    } else {
        help.to_string()
    })
}

/// Every name the completion offers.
fn command_names() -> Vec<String> {
    GROUPS
        .iter()
        .flat_map(|(_, commands)| commands.iter().map(|(name, _)| name.to_string()))
        .chain(SESSION_WORDS.iter().map(|(word, _)| word.to_string()))
        .chain(["exit".to_string()])
        .collect()
}

/// Split a line like a shell does (quotes, backslashes), except that `eval`
/// and `cmd` keep the rest of the line verbatim (PRACTICE text keeps its
/// quotes). A leading `rvdbg` is dropped.
pub fn split_line(line: &str) -> Result<Vec<String>, String> {
    let mut trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix("rvdbg") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            trimmed = rest.trim_start();
        }
    }
    let (first, rest) = match trimmed.split_once(char::is_whitespace) {
        Some((first, rest)) => (first, rest.trim()),
        None => (trimmed, ""),
    };
    if first == "eval" || first == "cmd" {
        let mut rest = rest.to_string();
        let mut json = false;
        while let Some(stripped) = rest.strip_suffix("--json") {
            if !stripped.ends_with(char::is_whitespace) {
                break;
            }
            rest = stripped.trim_end().to_string();
            json = true;
        }
        let mut args = vec![first.to_string()];
        if !rest.is_empty() {
            args.push(rest);
        }
        if json {
            args.push("--json".into());
        }
        return Ok(args);
    }
    shell_words(trimmed)
}

/// POSIX-shell-like words: whitespace separates, '…' is literal, "…" allows
/// \" and \\, a backslash outside quotes escapes the next character.
fn shell_words(text: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err("no closing quotation".into()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\')) => word.push(c),
                            Some(c) => {
                                word.push('\\');
                                word.push(c);
                            }
                            None => return Err("no closing quotation".into()),
                        },
                        Some(c) => word.push(c),
                        None => return Err("no closing quotation".into()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(c) => word.push(c),
                    None => return Err("no escaped character".into()),
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

/// `rvdbg [openocd halted]> ` with colours: the program, the backend and
/// the state (running green, halted yellow, problems red).
fn color_prompt(style: Style, prompt: &str) -> String {
    let Some((head, rest)) = prompt.split_once('[') else {
        return style.label(prompt);
    };
    let Some((inside, tail)) = rest.split_once(']') else {
        return style.label(prompt);
    };
    let (backend, state) = inside.split_once(' ').unwrap_or((inside, ""));
    let backend_text = if backend == "no" {
        style.bad(inside)
    } else {
        let state_text = match state {
            "" => String::new(),
            "running" => format!(" {}", style.good(state)),
            "halted" => format!(" {}", style.warn(state)),
            _ => format!(" {}", style.bad(state)),
        };
        format!("{}{state_text}", style.address(backend))
    };
    format!(
        "{}{}{backend_text}{}{}",
        style.label(head),
        style.dim("["),
        style.dim("]"),
        tail
    )
}

struct LineHelper {
    names: Vec<String>,
    style: Style,
}

impl Completer for LineHelper {
    type Candidate = String;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<String>)> {
        let before = &line[..pos];
        let (start, word) = match before.rsplit_once(char::is_whitespace) {
            Some((head, word)) if matches!(head.trim(), "help" | "rvdbg") => {
                (pos - word.len(), word)
            }
            Some(_) => return Ok((pos, Vec::new())),
            None => (
                before.len() - before.trim_start().len(),
                before.trim_start(),
            ),
        };
        let mut matches: Vec<String> = self
            .names
            .iter()
            .filter(|name| name.starts_with(word))
            .cloned()
            .collect();
        matches.sort();
        Ok((start, matches))
    }
}

impl Hinter for LineHelper {
    type Hint = String;
}

impl Highlighter for LineHelper {
    /// The prompt stays plain text for the editor, which measures its width
    /// without the colours.
    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        _default: bool,
    ) -> Cow<'b, str> {
        if self.style.enabled() {
            Cow::Owned(color_prompt(self.style, prompt))
        } else {
            Cow::Borrowed(prompt)
        }
    }
}

impl Validator for LineHelper {}

impl Helper for LineHelper {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
}

/// The session's state, independent of the terminal so that tests drive it.
pub struct Session<'a> {
    config: &'a Config,
    /// `--backend` on the command line.
    flag: Option<Backend>,
    json: bool,
    style: Style,
    backend: Option<Backend>,
    /// Why this backend (or why none).
    reason: String,
    server: Option<stellarbridge::Server>,
    interrupted: Arc<AtomicBool>,
    /// Look for probes; tests replace it.
    detect: fn(&Config) -> Result<Vec<probe::Found>, String>,
}

impl<'a> Session<'a> {
    fn new(config: &'a Config, flag: Option<Backend>, json: bool, style: Style) -> Session<'a> {
        Session {
            config,
            flag,
            json,
            style,
            backend: None,
            reason: String::new(),
            server: None,
            interrupted: Arc::new(AtomicBool::new(false)),
            detect: probe::detect,
        }
    }

    fn note(&self, text: &str) {
        eprintln!("{} {text}", self.style.label("[rvdbg]"));
    }

    fn error(&self, text: &str) {
        eprintln!("{} {text}", self.style.bad("error:"));
    }

    /// Choose the backend (`forced`, else `--backend`, `project.backend`,
    /// the connected probe) and, for OpenOCD, start or attach to OpenOCD.
    fn connect(&mut self, forced: Option<Backend>) {
        self.server = None;
        let fixed = match forced {
            Some(backend) => Some((backend, "the 'backend' command")),
            None => crate::fixed(self.config, self.flag),
        };
        let found = match fixed {
            Some(_) => Vec::new(),
            None => match (self.detect)(self.config) {
                Ok(found) => found,
                Err(error) => {
                    self.backend = None;
                    self.reason = error;
                    return;
                }
            },
        };
        match probe::choose(fixed, &found) {
            Ok(choice) => {
                self.backend = Some(choice.backend);
                self.reason = choice.reason;
            }
            Err(error) => {
                self.backend = None;
                self.reason = error;
            }
        }
        if self.backend == Some(Backend::OpenOcd) {
            if let Err(error) = self.start_openocd() {
                self.error(&format!("{error}; 'reconnect' tries again"));
            }
        }
    }

    /// The session's OpenOCD, with the GDB server on the core, in its own
    /// process group so that Ctrl-C leaves it running.
    fn start_openocd(&mut self) -> Result<(), String> {
        let openocd = self.config.openocd()?;
        let target = openocd.gdb_target.as_deref().unwrap_or("the first target");
        if stellarbridge::Server::running(openocd.tcl_port) {
            self.server = Some(stellarbridge::Server::attach(openocd.tcl_port).map_err(|e| e.0)?);
            self.note(&format!(
                "using the OpenOCD already running on Tcl port {}",
                openocd.tcl_port
            ));
            return Ok(());
        }
        let log = crate::run_dir(self.config)?.join("openocd.log");
        let mut server_config = crate::openocd::server_config(openocd, Some(log.clone()), true);
        server_config.own_process_group = true;
        self.note(&format!("starting {}", openocd.executable.display()));
        let mut server = stellarbridge::Server::start(&server_config).map_err(|e| e.0)?;
        for command in crate::openocd::debug_commands(openocd, true) {
            server.eval(&command).map_err(|e| e.0)?;
        }
        self.note(&format!(
            "OpenOCD runs for this session; GDB server on 127.0.0.1:{} ({target}); log {}",
            openocd.gdb_port,
            log.display()
        ));
        self.server = Some(server);
        Ok(())
    }

    /// The first lines: the version, the file, the probe and the commands.
    fn header(&self) -> String {
        let style = self.style;
        let backend = match self.backend {
            Some(backend) => style.good(format!("{} ({})", backend.name(), self.reason)),
            None => style.warn(&self.reason),
        };
        format!(
            "{} {}  {}\n{}  {backend}\n\n{}\n",
            style.label("rvdbg"),
            env!("CARGO_PKG_VERSION"),
            style.dim(self.config.file.display()),
            style.label("backend"),
            help_text(style, self.backend)
        )
    }

    /// `rvdbg [openocd running]> `, `rvdbg [trace32]> `, `rvdbg [no probe]> `.
    pub fn prompt(&mut self) -> String {
        let inside = match self.backend {
            None => "no probe".to_string(),
            Some(Backend::Trace32) => {
                if probe::listening(probe::rcl_port(self.config)) {
                    "trace32".to_string()
                } else {
                    "trace32 no-PowerView".to_string()
                }
            }
            Some(Backend::OpenOcd) => {
                let core = self
                    .config
                    .openocd
                    .as_ref()
                    .and_then(|openocd| openocd.core().ok())
                    .map(str::to_string);
                match (self.server.as_mut(), core) {
                    (Some(server), Some(core)) => {
                        let mut target = CoreTarget {
                            server,
                            core,
                            memory_ap: None,
                            symbols: Default::default(),
                        };
                        match target.run_state() {
                            Ok(RunState::Running) => "openocd running".into(),
                            Ok(RunState::Halted) => "openocd halted".into(),
                            Ok(RunState::Other) => "openocd other".into(),
                            Err(_) => "openocd lost".into(),
                        }
                    }
                    (Some(_), None) => "openocd".into(),
                    (None, _) => "openocd not-running".into(),
                }
            }
        };
        format!("rvdbg [{inside}]> ")
    }

    pub fn handle_line(&mut self, line: &str) -> Flow {
        let args = match split_line(line) {
            Ok(args) => args,
            Err(error) => {
                self.error(&error);
                return Flow::Continue;
            }
        };
        let Some(first) = args.first().cloned() else {
            return Flow::Continue;
        };
        match first.as_str() {
            "quit" | "exit" => return Flow::Quit,
            "help" => {
                match args.get(1) {
                    None => println!("{}", help_text(self.style, self.backend)),
                    Some(name) => match command_help(name, self.style) {
                        Ok(text) => println!("{}", text.trim_end()),
                        Err(error) => self.error(&error),
                    },
                }
                return Flow::Continue;
            }
            "backend" => {
                let backend = match args.get(1).map(String::as_str) {
                    Some("trace32") => Backend::Trace32,
                    Some("openocd") => Backend::OpenOcd,
                    _ => {
                        self.error("backend trace32|openocd");
                        return Flow::Continue;
                    }
                };
                self.connect(Some(backend));
                self.note(&format!("backend {}", self.reason));
                return Flow::Continue;
            }
            "reconnect" => {
                self.connect(None);
                match self.backend {
                    Some(backend) => {
                        self.note(&format!("backend {} ({})", backend.name(), self.reason))
                    }
                    None => self.error(&self.reason.clone()),
                }
                println!("{}", help_text(self.style, self.backend));
                return Flow::Continue;
            }
            _ => {}
        }
        let cli = match Cli::try_parse_from(std::iter::once("rvdbg".to_string()).chain(args)) {
            Ok(cli) => cli,
            Err(error) if error.kind() == clap::error::ErrorKind::InvalidSubcommand => {
                self.error(&format!("no command '{first}'; 'help' lists them"));
                return Flow::Continue;
            }
            Err(error) => {
                let _ = error.print();
                return Flow::Continue;
            }
        };
        if cli.config.is_some() || cli.backend.is_some() {
            self.error("--config and --backend are fixed for the session; use 'backend <name>'");
            return Flow::Continue;
        }
        let Some(command) = cli.command else {
            return Flow::Continue;
        };
        if let Err(error) = self.run_command(command, cli.json || self.json) {
            self.error(&error);
        }
        Flow::Continue
    }

    fn run_command(&mut self, command: Commands, json: bool) -> Result<(), String> {
        let name = command_name(&command);
        if let Some((_, need)) = GROUPS
            .iter()
            .flat_map(|(_, commands)| commands.iter())
            .find(|(candidate, _)| *candidate == name)
        {
            if !available(*need, self.backend) {
                return Err(match (need, self.backend) {
                    (_, None) => {
                        format!("{name} needs a probe; plug one in, then 'reconnect'")
                    }
                    (_, Some(backend)) => {
                        format!("{name} needs TRACE32; this session uses {}", backend.name())
                    }
                });
            }
        }
        match command {
            Commands::Init => Err(format!(
                "{} already exists; this session uses it",
                self.config.file.display()
            )),
            Commands::Skill { dir } => install_skill(dir.as_deref()).map(|_| ()),
            Commands::Debug { .. } | Commands::Adapter => {
                self.note(&self.debug_notice());
                Ok(())
            }
            command => {
                self.interrupted.store(false, Ordering::SeqCst);
                // Commands that need no probe (config, vscode, rustrover) also
                // run without one.
                let mut env = Env {
                    config: self.config,
                    flag: self.backend.or(self.flag),
                    json,
                    session: self.backend.map(|backend| SessionLink {
                        backend,
                        server: self.server.as_mut(),
                        interrupted: &self.interrupted,
                    }),
                };
                let code = execute(&mut env, command)?;
                if code != 0 {
                    eprintln!("{}", self.style.dim(format!("(exit code {code})")));
                }
                Ok(())
            }
        }
    }

    /// Where the IDE's debug server is while the session runs.
    fn debug_notice(&self) -> String {
        match (self.backend, &self.config.openocd) {
            (Some(Backend::OpenOcd), Some(openocd)) => format!(
                "GDB is served on 127.0.0.1:{} ({}) while this session runs: connect GDB \
                 there, or press F5 on 'StellarLINK: Attach' in VS Code",
                openocd.gdb_port,
                openocd.gdb_target.as_deref().unwrap_or("the first target")
            ),
            _ => "VS Code and RustRover start TRACE32's DAP adapter themselves ('vscode', \
                  'rustrover'); PowerView must run ('open')"
                .to_string(),
        }
    }
}

/// The subcommand's name as the help lists it.
fn command_name(command: &Commands) -> String {
    let debug = format!("{command:?}");
    let variant = debug
        .split(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or_default()
        .to_string();
    // RunUntil -> run-until
    let mut name = String::new();
    for (index, c) in variant.chars().enumerate() {
        if c.is_uppercase() {
            if index > 0 {
                name.push('-');
            }
            name.extend(c.to_lowercase());
        } else {
            name.push(c);
        }
    }
    name
}

fn history_path(config: &Config) -> Option<PathBuf> {
    crate::run_dir(config).ok().map(|dir| dir.join("history"))
}

/// The interactive session.
pub fn run(
    explicit: Option<&Path>,
    cwd: &Path,
    flag: Option<Backend>,
    json: bool,
) -> Result<u8, String> {
    let style = Style::stdout();
    let path = match config::find(explicit, cwd) {
        Ok(path) => path,
        Err(error) => {
            println!("{}\n", help_text(style, None));
            return Err(error);
        }
    };
    let config = config::load(&path)?;

    // Ctrl-C sets the flag instead of ending rvdbg; the line editor reads it
    // as a key while it waits for input.
    let interrupted = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    signal_hook::flag::register(signal_hook::consts::SIGINT, interrupted.clone())
        .map_err(|error| format!("cannot handle Ctrl-C: {error}"))?;

    let mut session = Session::new(&config, flag, json, style);
    session.interrupted = interrupted;
    session.connect(None);
    print!("{}", session.header());

    if !std::io::stdin().is_terminal() {
        // Lines from a pipe or a file: no prompt, no line editor.
        for line in std::io::stdin().lock().lines() {
            let line = line.map_err(|error| format!("cannot read the input: {error}"))?;
            if session.handle_line(&line) == Flow::Quit {
                break;
            }
        }
        return Ok(0);
    }

    let mut editor = Editor::<LineHelper, DefaultHistory>::new()
        .map_err(|error| format!("cannot start the line editor: {error}"))?;
    editor.set_helper(Some(LineHelper {
        names: command_names(),
        style,
    }));
    let history = history_path(&config);
    if let Some(path) = &history {
        let _ = editor.load_history(path);
    }
    loop {
        let prompt = session.prompt();
        match editor.readline(&prompt) {
            Ok(line) => {
                if !line.trim().is_empty() {
                    let _ = editor.add_history_entry(line.as_str());
                    if let Some(path) = &history {
                        let _ = editor.append_history(path);
                    }
                }
                if session.handle_line(&line) == Flow::Quit {
                    break;
                }
            }
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(error) => return Err(format!("terminal error: {error}")),
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(dir: &Path) -> Config {
        let text = "[project]\nprogram = \"app\"\nelf = \"app.elf\"\n\
                    [openocd]\nconfig = [\"board/x.cfg\"]\ngdb_target = \"x.core\"\n";
        let path = dir.join("rvdbg.toml");
        std::fs::write(&path, text).unwrap();
        config::load(&path).unwrap()
    }

    fn nothing_found(_: &Config) -> Result<Vec<probe::Found>, String> {
        Ok(Vec::new())
    }

    #[test]
    fn lines_split_like_a_shell_and_drop_rvdbg() {
        assert_eq!(split_line("rvdbg status").unwrap(), ["status"]);
        assert_eq!(
            split_line("  status --json ").unwrap(),
            ["status", "--json"]
        );
        assert_eq!(split_line("rvdbgx").unwrap(), ["rvdbgx"]);
        assert_eq!(
            split_line("check 'my checks.toml' --variant \"s 5\"").unwrap(),
            ["check", "my checks.toml", "--variant", "s 5"]
        );
        assert_eq!(
            split_line("eval Data.Long(D:0x10) + \"x\" --json").unwrap(),
            ["eval", "Data.Long(D:0x10) + \"x\"", "--json"]
        );
        assert_eq!(
            split_line("rvdbg cmd PRINT \"a\"").unwrap(),
            ["cmd", "PRINT \"a\""]
        );
        assert!(split_line("mem 'open").is_err());
        assert!(split_line("").unwrap().is_empty());
    }

    #[test]
    fn help_dims_what_the_backend_cannot_do() {
        let plain = Style::PLAIN;
        let text = help_text(plain, Some(Backend::OpenOcd));
        assert!(text.contains("dimmed: TRACE32 only"), "{text}");
        assert!(!text.contains("no probe found"), "{text}");
        let text = help_text(plain, Some(Backend::Trace32));
        assert!(!text.contains("dimmed"), "{text}");
        let text = help_text(plain, None);
        assert!(text.contains("no probe found"), "{text}");
        // With colours, reg is dimmed for OpenOCD and bold for TRACE32.
        let dim_reg = Style::COLOR.dim("reg");
        assert!(help_text(Style::COLOR, Some(Backend::OpenOcd)).contains(&dim_reg));
        assert!(!help_text(Style::COLOR, Some(Backend::Trace32)).contains(&dim_reg));
    }

    #[test]
    fn every_listed_command_exists() {
        let cli = Cli::command();
        for (_, commands) in GROUPS {
            for (name, _) in *commands {
                assert!(cli.find_subcommand(name).is_some(), "{name}");
            }
        }
        assert!(
            command_help("status", Style::PLAIN)
                .unwrap()
                .contains("Usage")
        );
        assert!(
            command_help("backend", Style::PLAIN)
                .unwrap()
                .contains("trace32|openocd")
        );
        assert!(command_help("nope", Style::PLAIN).is_err());
    }

    #[test]
    fn command_names_match_the_help() {
        let cli = Cli::try_parse_from(["rvdbg", "run-until", "main"]).unwrap();
        assert_eq!(command_name(&cli.command.unwrap()), "run-until");
        let cli = Cli::try_parse_from(["rvdbg", "status"]).unwrap();
        assert_eq!(command_name(&cli.command.unwrap()), "status");
    }

    #[test]
    fn without_a_probe_target_commands_are_refused_and_the_session_goes_on() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let mut session = Session::new(&config, None, false, Style::PLAIN);
        session.detect = nothing_found;
        session.connect(None);
        assert_eq!(session.backend, None);
        assert_eq!(session.prompt(), "rvdbg [no probe]> ");
        let cli = Cli::try_parse_from(["rvdbg", "status"]).unwrap();
        let error = session
            .run_command(cli.command.unwrap(), false)
            .unwrap_err();
        assert_eq!(error, "status needs a probe; plug one in, then 'reconnect'");
        assert_eq!(session.handle_line("rvdbg status"), Flow::Continue);
        assert_eq!(session.handle_line("help mem"), Flow::Continue);
        // Commands that need no probe still run.
        assert_eq!(session.handle_line("vscode"), Flow::Continue);
        assert!(dir.path().join(".vscode/launch.json").exists());
        assert_eq!(session.handle_line("exit"), Flow::Quit);
    }

    #[test]
    fn trace32_only_commands_are_refused_on_openocd() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let mut session = Session::new(&config, None, false, Style::PLAIN);
        // OpenOCD chosen but not started: the check happens before any use.
        session.backend = Some(Backend::OpenOcd);
        let cli = Cli::try_parse_from(["rvdbg", "reg", "HSR"]).unwrap();
        let error = session
            .run_command(cli.command.unwrap(), false)
            .unwrap_err();
        assert_eq!(error, "reg needs TRACE32; this session uses openocd");
        let cli = Cli::try_parse_from(["rvdbg", "status"]).unwrap();
        let error = session
            .run_command(cli.command.unwrap(), false)
            .unwrap_err();
        assert!(error.contains("'reconnect' starts it"), "{error}");
        assert_eq!(session.prompt(), "rvdbg [openocd not-running]> ");
    }

    #[test]
    fn prompts_are_coloured_by_state() {
        let style = Style::COLOR;
        let text = color_prompt(style, "rvdbg [openocd halted]> ");
        assert!(text.contains(&style.warn("halted")), "{text:?}");
        assert!(text.contains(&style.address("openocd")), "{text:?}");
        let text = color_prompt(style, "rvdbg [no probe]> ");
        assert!(text.contains(&style.bad("no probe")), "{text:?}");
        assert_eq!(
            color_prompt(Style::PLAIN, "rvdbg [trace32]> "),
            "rvdbg [trace32]> "
        );
    }
}
