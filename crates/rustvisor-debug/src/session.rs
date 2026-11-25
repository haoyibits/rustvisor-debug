//! `rvdbg` without a command: an interactive session. It finds rvdbg.toml,
//! picks the backend from the connected probe, and then runs one command per
//! line (`status`, `mem _SEGGER_RTT 4`, `flash`), the same commands as on the
//! command line; a leading `rvdbg` is accepted too.
//!
//! With StellarLINK the session keeps one OpenOCD for all commands, with its
//! GDB server open, so GDB or VS Code can attach while the session runs. With
//! TRACE32 the inspect commands, `ram` and `fdx` share one PowerView
//! connection, made on first use and dropped after a failure; the others
//! connect as on the command line.
//!
//! `rvdbg --json` with commands on a pipe is for programs: a `ready` JSON
//! line first, then after each command's own output (one JSON line for the
//! inspect commands) an `end` JSON line with its exit code and error.
//!
//! Several lines at once (a paste) run one after the other, each shown
//! after the prompt, and stop at the first that fails; a line ending in `\`
//! continues on the next. `source <file>` runs a file's lines the same way
//! (`#` starts a comment). `images <dir>` switches the session to the images
//! of a directory.
//!
//! Ctrl-C stops a waiting command (`rtt`, `fdx`, `run-until`), not the session;
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
use crate::fdx_terminal::History;
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
            ("ram", Need::Probe),
            ("fault", Need::Probe),
            ("cores", Need::Probe),
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
    (
        "terminal",
        &[
            ("rtt", Need::Probe),
            ("fdx", Need::Probe),
            ("swtrace", Need::Probe),
        ],
    ),
    (
        "project",
        &[
            ("config", Need::Nothing),
            ("images", Need::Nothing),
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
    ("source", "source <file>"),
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
            "source" => {
                "run the file's lines as if typed, each shown after the prompt; stops at the \
                 first that fails; '#' starts a comment, a line ending in '\\' continues"
            }
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

impl Validator for LineHelper {
    /// A line ending in `\` continues on the next.
    fn validate(
        &self,
        ctx: &mut rustyline::validate::ValidationContext<'_>,
    ) -> rustyline::Result<rustyline::validate::ValidationResult> {
        Ok(if ctx.input().trim_end().ends_with('\\') {
            rustyline::validate::ValidationResult::Incomplete
        } else {
            rustyline::validate::ValidationResult::Valid(None)
        })
    }
}

/// The commands of an input that may hold several lines, with their line
/// numbers: a line ending in `\` continues on the next; blank lines and
/// lines starting with `#` are skipped.
pub fn logical_lines(input: &str) -> Vec<(usize, String)> {
    let mut lines = Vec::new();
    let mut pending: Option<(usize, String)> = None;
    for (index, raw) in input.split('\n').enumerate() {
        let line = raw.trim_end_matches('\r');
        let (number, mut text) = pending.take().unwrap_or_else(|| (index + 1, String::new()));
        // A continued line joins the one before with one space.
        let line = if text.is_empty() {
            line
        } else {
            text.push(' ');
            line.trim_start()
        };
        if let Some(head) = line.trim_end().strip_suffix('\\') {
            text.push_str(head.trim_end());
            pending = Some((number, text));
            continue;
        }
        text.push_str(line);
        let command = text.trim();
        if !command.is_empty() && !command.starts_with('#') {
            lines.push((number, command.to_string()));
        }
    }
    if let Some((number, text)) = pending {
        let command = text.trim();
        if !command.is_empty() && !command.starts_with('#') {
            lines.push((number, command.to_string()));
        }
    }
    lines
}

impl Helper for LineHelper {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
}

/// The session's state, independent of the terminal so that tests drive it.
pub struct Session {
    config: Config,
    /// `--backend` on the command line.
    flag: Option<Backend>,
    json: bool,
    style: Style,
    backend: Option<Backend>,
    /// Why this backend (or why none).
    reason: String,
    server: Option<stellarbridge::Server>,
    /// The PowerView connection the TRACE32 commands share.
    powerview: Option<tracebridge::backend::Session>,
    interrupted: Arc<AtomicBool>,
    /// The last line's exit code and error, for the `end` line.
    code: u8,
    failure: Option<String>,
    /// Look for probes; tests replace it.
    detect: fn(&Config) -> Result<Vec<probe::Found>, String>,
    /// What each FDX link sent in this session.
    history: History,
    /// `source` files being run, innermost last.
    sourcing: Vec<PathBuf>,
}

/// How deep `source` files may nest.
const SOURCE_DEPTH: usize = 8;

impl Session {
    fn new(config: Config, flag: Option<Backend>, json: bool, style: Style) -> Session {
        Session {
            config,
            flag,
            json,
            style,
            backend: None,
            reason: String::new(),
            server: None,
            powerview: None,
            interrupted: Arc::new(AtomicBool::new(false)),
            code: 0,
            failure: None,
            detect: probe::detect,
            history: History::default(),
            sourcing: Vec::new(),
        }
    }

    fn note(&self, text: &str) {
        eprintln!("{} {text}", self.style.label("[rvdbg]"));
    }

    fn error(&mut self, text: &str) {
        eprintln!("{} {text}", self.style.bad("error:"));
        self.failure = Some(text.to_string());
        if self.code == 0 {
            self.code = 1;
        }
    }

    /// Choose the backend (`forced`, else `--backend`, `project.backend`,
    /// the connected probe) and, for OpenOCD, start or attach to OpenOCD.
    fn connect(&mut self, forced: Option<Backend>) {
        self.server = None;
        self.powerview = None;
        let fixed = match forced {
            Some(backend) => Some((backend, "the 'backend' command")),
            None => crate::fixed(&self.config, self.flag),
        };
        let found = match fixed {
            Some(_) => Vec::new(),
            None => match (self.detect)(&self.config) {
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
        let log = crate::run_dir(&self.config)?.join("openocd.log");
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
            "{} {}  {}\n{}  {backend}\n{}   {}\n\n{}\n",
            style.label("rvdbg"),
            env!("CARGO_PKG_VERSION"),
            style.dim(self.config.file.display()),
            style.label("backend"),
            style.label("images"),
            self.images_line(),
            help_text(style, self.backend)
        )
    }

    /// Where the images come from, and the ELF's directory when it is not
    /// rvdbg.toml's.
    fn images_line(&self) -> String {
        match &self.config.images_from {
            config::ImagesFrom::Environment => format!(
                "ELF/EXTRA_ELF ({})",
                self.config
                    .elf
                    .parent()
                    .map_or_else(String::new, |dir| dir.display().to_string())
            ),
            from => from.to_string(),
        }
    }

    /// `rvdbg [openocd running]> `, `rvdbg [trace32]> `, `rvdbg [no probe]> `.
    pub fn prompt(&mut self) -> String {
        let inside = match self.backend {
            None => "no probe".to_string(),
            Some(Backend::Trace32) => {
                if !probe::listening(probe::rcl_port(&self.config)) {
                    "trace32 no-PowerView".to_string()
                } else {
                    // Only on the connection the session already has.
                    match self.powerview.as_mut().map(|session| session.running()) {
                        Some(Ok(true)) => "trace32 running".to_string(),
                        Some(Ok(false)) => "trace32 halted".to_string(),
                        Some(Err(_)) => {
                            self.powerview = None;
                            "trace32".to_string()
                        }
                        None => "trace32".to_string(),
                    }
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

    /// What was typed or pasted: one line, or several run in order until
    /// one fails ([`logical_lines`]).
    pub fn handle_input(&mut self, input: &str) -> Flow {
        let lines = logical_lines(input);
        match lines.as_slice() {
            [] => {
                self.code = 0;
                self.failure = None;
                Flow::Continue
            }
            [(_, line)] if !input.contains('\n') => self.handle_line(line),
            _ => self.run_lines(&lines, None),
        }
    }

    /// Lines one after the other, each shown after the prompt; the first that
    /// fails (or Ctrl-C) stops the rest. `file` names them in the message.
    fn run_lines(&mut self, lines: &[(usize, String)], file: Option<&Path>) -> Flow {
        self.interrupted.store(false, Ordering::SeqCst);
        for (index, (number, line)) in lines.iter().enumerate() {
            if !self.json {
                let prompt = self.prompt();
                println!("{}{line}", color_prompt(self.style, &prompt));
            }
            if self.handle_line(line) == Flow::Quit {
                return Flow::Quit;
            }
            let stopped = self.interrupted.load(Ordering::SeqCst);
            if self.code != 0 || stopped {
                let left = lines.len() - index - 1;
                let at = match file {
                    Some(file) => format!("{}:{number}", file.display()),
                    None => format!("line {}", index + 1),
                };
                let why = if self.code != 0 {
                    "failed"
                } else {
                    "was interrupted"
                };
                if left > 0 {
                    eprintln!(
                        "{} {at} {why}; {left} line(s) not run",
                        self.style.bad("error:")
                    );
                }
                if self.code == 0 {
                    self.code = 1;
                    self.failure = Some(format!("{at} {why}"));
                }
                return Flow::Continue;
            }
        }
        Flow::Continue
    }

    /// `source <file>`: its lines as if typed.
    fn source(&mut self, file: &str) -> Flow {
        let path = std::env::current_dir()
            .map(|cwd| cwd.join(file))
            .unwrap_or_else(|_| PathBuf::from(file));
        if self.sourcing.len() >= SOURCE_DEPTH {
            self.error(&format!("source files nest more than {SOURCE_DEPTH} deep"));
            return Flow::Continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                self.error(&format!("cannot read {}: {error}", path.display()));
                return Flow::Continue;
            }
        };
        self.sourcing.push(path.clone());
        let flow = self.run_lines(&logical_lines(&text), Some(&path));
        self.sourcing.pop();
        flow
    }

    /// `images <dir>`: the session's images from now on.
    fn switch_images(&mut self, dir: &str) {
        let path = std::env::current_dir()
            .map(|cwd| cwd.join(dir))
            .unwrap_or_else(|_| PathBuf::from(dir));
        match self.config.clone().with_images(&path) {
            Ok(config) => {
                self.config = config;
                // The connection and the links belong to the old images.
                self.powerview = None;
                self.history.clear();
                self.note(&format!(
                    "images from {}: {} and {} extra image(s)",
                    path.display(),
                    self.config.elf.display(),
                    self.config.extra_elf.len()
                ));
            }
            Err(error) => self.error(&error),
        }
    }

    pub fn handle_line(&mut self, line: &str) -> Flow {
        self.code = 0;
        self.failure = None;
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
        if first.starts_with('-') {
            self.code = 2;
            self.error(&format!(
                "a line starts with a command, options come after it (e.g. 'fdx {first}'); \
                 'help' lists the commands"
            ));
            return Flow::Continue;
        }
        match first.as_str() {
            "quit" | "exit" => return Flow::Quit,
            "source" => {
                return match args.as_slice() {
                    [_, file] => self.source(file),
                    _ => {
                        self.code = 2;
                        self.error("source <file>");
                        Flow::Continue
                    }
                };
            }
            "images" if args.len() == 2 && !args[1].starts_with('-') => {
                self.switch_images(&args[1]);
                return Flow::Continue;
            }
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
                    None => {
                        let reason = self.reason.clone();
                        self.error(&reason);
                    }
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
                self.code = u8::try_from(error.exit_code()).unwrap_or(2);
                print_clap_error(&error, &first, self.style);
                if self.code != 0 {
                    self.failure = Some(error.kind().to_string());
                }
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
                if !SHARES_POWERVIEW.contains(&name.as_str()) {
                    // flash, load, open and the like manage PowerView
                    // themselves.
                    self.powerview = None;
                }
                // Commands that need no probe (config, vscode, rustrover) also
                // run without one.
                if name == "flash" {
                    // The images start again.
                    self.history.clear();
                }
                let mut env = Env {
                    config: &self.config,
                    flag: self.backend.or(self.flag),
                    json,
                    session: self.backend.map(|backend| SessionLink {
                        backend,
                        server: self.server.as_mut(),
                        interrupted: &self.interrupted,
                        powerview: Some(&mut self.powerview),
                        history: Some(&mut self.history),
                    }),
                };
                let code = execute(&mut env, command)?;
                self.code = code;
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

/// The commands that use the session's PowerView connection.
const SHARES_POWERVIEW: &[&str] = &[
    "status",
    "halt",
    "go",
    "attach",
    "reset",
    "down",
    "fault",
    "reg",
    "mem",
    "verify",
    "check",
    "clock",
    "run-until",
    "trace",
    "eval",
    "cmd",
    "watch",
    "do",
    "ram",
    "fdx",
    "swtrace",
];

/// A usage error the way a session shows it: clap's message, with the
/// session's `help <command>` instead of `--help`.
fn print_clap_error(error: &clap::Error, command: &str, style: Style) {
    use clap::error::ErrorKind;
    if matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        let _ = error.print();
        return;
    }
    let rendered = error.render();
    let text = if style.enabled() {
        rendered.ansi().to_string()
    } else {
        rendered.to_string()
    };
    for line in text.trim_end().lines() {
        if line.contains("For more information, try") {
            eprintln!("'help {command}' explains the command");
        } else {
            eprintln!("{line}");
        }
    }
}

/// The `end` line after a command in `--json` pipe mode.
fn end_line(line: &str, code: u8, error: Option<&str>) -> String {
    serde_json::json!({
        "end": line.trim(),
        "ok": code == 0,
        "exit_code": code,
        "error": error,
    })
    .to_string()
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
    images: Option<&Path>,
) -> Result<u8, String> {
    let style = Style::stdout();
    let path = match config::find(explicit, cwd) {
        Ok(path) => path,
        Err(error) => {
            println!("{}\n", help_text(style, None));
            return Err(error);
        }
    };
    let mut config = config::load(&path)?;
    if let Some(dir) = images {
        config = config.with_images(&cwd.join(dir))?;
    }

    // Ctrl-C sets the flag instead of ending rvdbg; the line editor reads it
    // as a key while it waits for input.
    let interrupted = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    signal_hook::flag::register(signal_hook::consts::SIGINT, interrupted.clone())
        .map_err(|error| format!("cannot handle Ctrl-C: {error}"))?;

    let machine = json && !std::io::stdin().is_terminal();
    let style = if machine { Style::PLAIN } else { style };
    let mut session = Session::new(config.clone(), flag, json, style);
    session.interrupted = interrupted;
    session.connect(None);
    if machine {
        println!(
            "{}",
            serde_json::json!({
                "ready": true,
                "version": env!("CARGO_PKG_VERSION"),
                "config": config.file.display().to_string(),
                "backend": session.backend.map(Backend::name),
                "reason": session.reason,
            })
        );
    } else {
        print!("{}", session.header());
    }

    if !std::io::stdin().is_terminal() {
        // Lines from a pipe or a file: no prompt, no line editor.
        for line in std::io::stdin().lock().lines() {
            let line = line.map_err(|error| format!("cannot read the input: {error}"))?;
            if line.trim().is_empty() {
                continue;
            }
            let flow = session.handle_line(&line);
            if machine {
                println!(
                    "{}",
                    end_line(&line, session.code, session.failure.as_deref())
                );
            }
            if flow == Flow::Quit {
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
            Ok(input) => {
                for line in input.split('\n') {
                    if !line.trim().is_empty() {
                        let _ = editor.add_history_entry(line.trim_end());
                        if let Some(path) = &history {
                            let _ = editor.append_history(path);
                        }
                    }
                }
                if session.handle_input(&input) == Flow::Quit {
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
    fn pasted_lines_split_continue_and_skip_comments() {
        assert_eq!(
            logical_lines(
                "reset\r\n\n# a comment\n  go  \nfdx --send \\\n  \"lab five\" \\\n --expect x\n"
            ),
            [
                (1, "reset".to_string()),
                (4, "go".to_string()),
                (5, "fdx --send \"lab five\" --expect x".to_string()),
            ]
        );
        assert_eq!(logical_lines("status"), [(1, "status".to_string())]);
        assert_eq!(logical_lines("a \\"), [(1, "a".to_string())]);
        assert!(logical_lines(" \n# x").is_empty());
    }

    #[test]
    fn several_lines_run_in_order_and_stop_at_the_first_failure() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let mut session = Session::new(config.clone(), None, false, Style::PLAIN);
        session.detect = nothing_found;
        session.connect(None);
        // vscode runs without a probe; status fails; rustrover is not run.
        assert_eq!(
            session.handle_input("vscode\nstatus\nrustrover"),
            Flow::Continue
        );
        assert_eq!(session.code, 1);
        assert!(dir.path().join(".vscode/launch.json").exists());
        assert!(!dir.path().join(".run").exists());
        // A source file the same way, with its own line numbers.
        let file = dir.path().join("steps.rvdbg");
        std::fs::write(&file, "# steps\nhelp\nstatus\nrustrover\n").unwrap();
        assert_eq!(
            session.handle_line(&format!("source '{}'", file.display())),
            Flow::Continue
        );
        assert_eq!(session.code, 1);
        assert!(!dir.path().join(".run").exists());
        assert_eq!(session.handle_input("help\nquit\nstatus"), Flow::Quit);
        session.handle_line("source");
        assert_eq!(session.code, 2);
    }

    #[test]
    fn an_option_without_a_command_and_images_of_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let mut session = Session::new(config.clone(), None, false, Style::PLAIN);
        session.detect = nothing_found;
        session.connect(None);
        session.handle_line("--close");
        assert_eq!(session.code, 2);
        assert!(
            session
                .failure
                .as_deref()
                .unwrap()
                .contains("'fdx --close'")
        );
        let images = dir.path().join("case");
        std::fs::create_dir(&images).unwrap();
        for name in ["hypervisor", "guest0.elf", "guest1.elf"] {
            std::fs::write(images.join(name), b"\x7FELF....").unwrap();
        }
        std::fs::write(images.join("run.log"), b"log").unwrap();
        session.handle_line(&format!("images '{}'", images.display()));
        assert_eq!(session.code, 0, "{:?}", session.failure);
        assert_eq!(session.config.elf, images.join("hypervisor"));
        assert_eq!(
            session.config.extra_elf,
            [images.join("guest0.elf"), images.join("guest1.elf")]
        );
        assert_eq!(session.config.program, "hypervisor");
        assert!(session.images_line().starts_with("--images"));
        session.handle_line("images /nonexistent/dir");
        assert_eq!(session.code, 1);
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
        let mut session = Session::new(config.clone(), None, false, Style::PLAIN);
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
    fn each_line_leaves_its_exit_code_and_error_for_the_end_line() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let mut session = Session::new(config.clone(), None, true, Style::PLAIN);
        session.detect = nothing_found;
        session.connect(None);
        session.handle_line("status");
        assert_eq!(session.code, 1);
        assert_eq!(
            session.failure.as_deref(),
            Some("status needs a probe; plug one in, then 'reconnect'")
        );
        session.handle_line("ram");
        assert_eq!(session.code, 2, "a usage error");
        session.handle_line("help");
        assert_eq!((session.code, session.failure.as_deref()), (0, None));
        assert_eq!(
            end_line(" status ", 1, Some("no probe")),
            r#"{"end":"status","ok":false,"exit_code":1,"error":"no probe"}"#
        );
        assert_eq!(
            end_line("help", 0, None),
            r#"{"end":"help","ok":true,"exit_code":0,"error":null}"#
        );
    }

    #[test]
    fn the_commands_sharing_powerview_are_rvdbg_commands() {
        let cli = Cli::command();
        for name in SHARES_POWERVIEW {
            assert!(cli.find_subcommand(name).is_some(), "{name}");
        }
    }

    #[test]
    fn trace32_only_commands_are_refused_on_openocd() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let mut session = Session::new(config.clone(), None, false, Style::PLAIN);
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
