//! `rvdbg fdx`: talk to images over their FDX links while the cores run,
//! through any memory access that works then (TRACE32 `AD:`, the OpenOCD
//! MEM-AP).
//!
//! Interactive, each line typed is one packet to the target and each packet
//! from it is printed as one line, until Ctrl-C, `:q`, `exit` or `quit`. On a
//! terminal the input line is edited with rustyline (history, Tab completes
//! the words of the target's `help` reply) and the target's packets start
//! with `< `, printed above the line being typed; through a pipe the lines
//! are the bare packets. With `--all` every image's link is watched at once:
//! packets carry the link's name, input goes to the link the prompt names,
//! `@<name> <packet>` to another one, `:to <name>` switches.
//!
//! The terminal says when a packet stays unread (the target no longer
//! serves the link) or gets no reply, shows what the session last received
//! on the link, and while a link does not open says what the other links
//! and the hypervisor's stop records show.
//!
//! With `--send`/`--expect` it sends, waits for the expected packets and
//! exits: what a script or the board runner needs.

use probecore::output::{eprintln, println};

use std::collections::{HashMap, VecDeque};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use probecore::Memory;
use probecore::elf::SymbolTable;
use probecore::fdx::{Error, Packet};
use probecore::style::Style;
use rustvisor_debug::fdx::Link;
use rustvisor_debug::stop;
use serde_json::json;

use crate::EXIT_FAILED;

/// How often the open links are polled.
pub(crate) const POLL: Duration = Duration::from_millis(20);
/// How often a closed link is looked at again when several are watched.
const REOPEN_POLL: Duration = Duration::from_millis(200);
/// A packet still unread after this is reported.
const UNREAD_AFTER: Duration = Duration::from_secs(1);
/// ... and again after this.
const UNREAD_AGAIN: Duration = Duration::from_secs(10);
/// A packet read but not answered after this is reported, once.
const REPLY_AFTER: Duration = Duration::from_secs(2);
/// A link still closed after this gets a diagnosis.
const DIAGNOSE_AFTER: Duration = Duration::from_secs(3);
/// Packets after `help` that count as its reply (for completion).
const HELP_WINDOW: Duration = Duration::from_secs(3);
/// Packets a link's log keeps.
const LOG: usize = 20;
/// Packets of the log shown when a link is entered again.
const SHOWN: usize = 3;

/// One image's link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// `el2` for the ELF, `guest<n>` for the n-th extra image.
    pub name: String,
    pub symbol: String,
    pub address: u64,
}

/// What a session remembers of each link between commands: the packets the
/// target sent, newest last. A link that closes or reopens starts afresh.
#[derive(Debug, Default)]
pub struct History {
    links: HashMap<u64, VecDeque<(Instant, String)>>,
}

impl History {
    pub fn record(&mut self, address: u64, packet: &str, at: Instant) {
        let log = self.links.entry(address).or_default();
        if log.len() == LOG {
            log.pop_front();
        }
        log.push_back((at, packet.to_string()));
    }

    /// The last `n` packets, oldest first.
    pub fn last(&self, address: u64, n: usize) -> Vec<(Instant, String)> {
        self.links.get(&address).map_or_else(Vec::new, |log| {
            log.iter()
                .skip(log.len().saturating_sub(n))
                .cloned()
                .collect()
        })
    }

    pub fn forget(&mut self, address: u64) {
        self.links.remove(&address);
    }

    pub fn clear(&mut self) {
        self.links.clear();
    }
}

/// Whether a link's last packet means the target waits for a command.
fn waiting(packet: &str) -> bool {
    packet == "ready" || packet.starts_with("error ")
}

/// `35 s ago`, `2 min ago`.
fn ago(at: Instant, now: Instant) -> String {
    let seconds = now.saturating_duration_since(at).as_secs();
    if seconds < 120 {
        format!("{seconds} s ago")
    } else {
        format!("{} min ago", seconds / 60)
    }
}

/// What to read when a link does not open: the other links, and the stop
/// records of the EL2 image with the images' symbols for their addresses.
#[derive(Debug, Default)]
pub struct Diagnosis {
    pub peers: Vec<Target>,
    pub stops: Option<stop::Slots>,
    pub symbols: SymbolTable,
}

impl Diagnosis {
    /// One line per finding, read without halting a core.
    pub fn lines(&self, memory: &mut impl Memory, history: &History, now: Instant) -> Vec<String> {
        let mut lines = Vec::new();
        for peer in &self.peers {
            match Link::locate(memory, peer.address) {
                Ok(Some(_)) => match history.last(peer.address, 1).pop() {
                    Some((at, packet)) if waiting(&packet) => lines.push(format!(
                        "{} is open and its last packet was {packet:?} ({}): it may be waiting \
                         for a command",
                        peer.name,
                        ago(at, now)
                    )),
                    Some((at, packet)) => lines.push(format!(
                        "{} is open; its last packet was {packet:?} ({})",
                        peer.name,
                        ago(at, now)
                    )),
                    None => lines.push(format!("{} is open", peer.name)),
                },
                Ok(None) => lines.push(format!("{} is not open", peer.name)),
                Err(_) => {}
            }
        }
        if let Some(slots) = self.stops {
            match stop::read(memory, slots) {
                Ok(records) => {
                    let mut stopped = 0;
                    for (slot, record) in records.iter().enumerate() {
                        match record {
                            stop::Record::Complete(stop) => {
                                stopped += 1;
                                lines.push(format!(
                                    "{} stopped: {}",
                                    stop.core,
                                    stop.summary(&|address| self.symbol(address))
                                ));
                            }
                            stop::Record::Incomplete => {
                                lines.push(format!("stop slot {slot}: a record is being written"))
                            }
                            stop::Record::Empty | stop::Record::Invalid(_) => {}
                        }
                    }
                    if stopped == 0 {
                        lines.push("no core has a stop record".to_string());
                    }
                }
                Err(error) => lines.push(format!("stop records not read: {error}")),
            }
        }
        lines
    }

    /// `_start`, `main+0x1C`.
    pub fn symbol(&self, address: u32) -> Option<String> {
        self.symbols
            .lookup(u64::from(address))
            .map(|(symbol, offset)| match offset {
                0 => symbol.name.clone(),
                offset => format!("{}+0x{offset:X}", symbol.name),
            })
    }
}

pub struct Options {
    /// The link, or with `--all` every image's link.
    pub targets: Vec<Target>,
    /// Packets to send, then exit.
    pub send: Vec<String>,
    /// Packets to wait for after sending, in order.
    pub expect: Vec<String>,
    /// For the link to open, for room, and for each expected packet.
    pub timeout: Duration,
    pub json: bool,
    pub diagnosis: Diagnosis,
    /// rustyline's history of the interactive input.
    pub history_file: Option<PathBuf>,
}

impl Options {
    fn scripted(&self) -> bool {
        !self.send.is_empty() || !self.expect.is_empty()
    }
}

/// Runs `rvdbg fdx`; Ctrl-C sets `interrupted`. `probe` names the probe in
/// the status lines (`TRACE32`, `StellarLINK`).
pub fn run(
    memory: &mut impl Memory,
    probe: &str,
    options: &Options,
    history: &mut History,
    interrupted: &AtomicBool,
) -> Result<u8, String> {
    if options.scripted() {
        scripted(memory, options, history, interrupted)
    } else {
        interactive(memory, probe, options, history, interrupted)
    }
}

fn text(packet: &[u8]) -> String {
    String::from_utf8_lossy(packet).into_owned()
}

/// Waits until the target opens the link; `None` if `deadline` passes or
/// Ctrl-C comes first.
fn open(
    memory: &mut impl Memory,
    address: u64,
    deadline: Option<Instant>,
    interrupted: &AtomicBool,
) -> Result<Option<Link>, String> {
    loop {
        if let Some(link) = Link::locate(memory, address).map_err(|error| error.to_string())? {
            return Ok(Some(link));
        }
        if interrupted.load(Ordering::SeqCst) || deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok(None);
        }
        std::thread::sleep(POLL);
    }
}

fn scripted(
    memory: &mut impl Memory,
    options: &Options,
    history: &mut History,
    interrupted: &AtomicBool,
) -> Result<u8, String> {
    let [target] = options.targets.as_slice() else {
        return Err("--send and --expect talk to one link; name it with --guest".into());
    };
    let deadline = || Some(Instant::now() + options.timeout);
    let stopped = || {
        if interrupted.load(Ordering::SeqCst) {
            Err("interrupted".to_string())
        } else {
            Ok(())
        }
    };
    let Some(link) = open(memory, target.address, deadline(), interrupted)? else {
        stopped()?;
        let mut error = format!(
            "{} at 0x{:08X} did not open within {:?}; is the image running?",
            target.symbol, target.address, options.timeout
        );
        for line in options.diagnosis.lines(memory, history, Instant::now()) {
            error.push_str(&format!("; {line}"));
        }
        return Err(error);
    };
    for packet in &options.send {
        if packet.is_empty() || packet.len() > link.max_payload() {
            return Err(format!(
                "a packet must have 1 to {} bytes; {packet:?} has {}",
                link.max_payload(),
                packet.len()
            ));
        }
        let until = deadline().expect("deadline");
        while !link
            .send(memory, packet.as_bytes())
            .map_err(|error| error.to_string())?
        {
            stopped()?;
            if Instant::now() >= until {
                return Err(format!(
                    "no room for {packet:?} within {:?}: the target does not read",
                    options.timeout
                ));
            }
            std::thread::sleep(POLL);
        }
    }

    let mut received = Vec::new();
    let mut dropped = 0;
    let mut missing = None;
    let mut stdout = std::io::stdout();
    'expect: for wanted in &options.expect {
        let until = deadline().expect("deadline");
        loop {
            match link.receive(memory).map_err(|error| error.to_string())? {
                Some(Packet::Data(packet)) => {
                    let packet = text(&packet);
                    history.record(target.address, &packet, Instant::now());
                    if !options.json
                        && !probecore::output::send(probecore::output::Line::Text(&packet))
                    {
                        let _ = writeln!(stdout, "{packet}");
                    }
                    let found = packet == *wanted;
                    received.push(packet);
                    if found {
                        continue 'expect;
                    }
                }
                Some(Packet::Dropped) => dropped += 1,
                None if Instant::now() >= until => {
                    missing = Some(wanted.clone());
                    break 'expect;
                }
                None => {
                    stopped()?;
                    std::thread::sleep(POLL);
                }
            }
        }
    }

    if options.json {
        println!(
            "{}",
            json!({
                "symbol": target.symbol,
                "address": format!("0x{:08X}", target.address),
                "size": link.to_target.size,
                "sent": options.send,
                "received": received,
                "dropped": dropped,
                "expected": options.expect,
                "missing": missing,
                "ok": missing.is_none(),
            })
        );
    }
    let style = Style::stderr();
    if dropped > 0 {
        eprintln!(
            "{} the target dropped packets {dropped} time(s): its ring was full",
            style.warn("[fdx]")
        );
    }
    match missing {
        Some(wanted) => {
            if !options.json {
                eprintln!(
                    "{} no {wanted:?} within {:?}",
                    style.bad("[fdx]"),
                    options.timeout
                );
            }
            Ok(EXIT_FAILED)
        }
        None => Ok(0),
    }
}

// ------------------------------------------------------------ interactive

/// What a typed line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    Nothing,
    /// Leave `fdx`.
    Quit,
    /// A packet for this link.
    Send(usize, String),
    /// The link's log.
    Last(usize),
    Help,
    /// Talk to this link from now on.
    To(usize),
    /// Not sent: why.
    Refused(String),
}

/// The local commands, for `:help`.
const LOCAL_HELP: &str = ":q (or exit, quit, Ctrl-C) leaves fdx; :last lists what this session \
                          received on the link; :to <name> talks to another link and \
                          @<name> <packet> sends one packet to it (--all); ::<text> sends \
                          :<text>";

/// What `line`, typed while talking to `current`, asks for. `names` are
/// the links' names; `@` and `:to` only when several are watched.
fn parse(line: &str, names: &[String], current: usize, several: bool) -> Action {
    let line = line.trim_end_matches(['\r', '\n']);
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Action::Nothing;
    }
    let find = |name: &str| names.iter().position(|candidate| candidate == name);
    let unknown =
        |name: &str| Action::Refused(format!("no link {name:?}; links: {}", names.join(" ")));
    if let Some(rest) = line.strip_prefix("::") {
        return Action::Send(current, format!(":{rest}"));
    }
    if let Some(command) = trimmed.strip_prefix(':') {
        let mut words = command.split_whitespace();
        return match (words.next(), words.next(), words.next()) {
            (Some("q" | "quit"), None, _) => Action::Quit,
            (Some("last"), None, _) => Action::Last(current),
            (Some("help"), None, _) => Action::Help,
            (Some("to"), Some(name), None) if several => {
                find(name).map_or_else(|| unknown(name), Action::To)
            }
            _ => Action::Refused(format!("unknown :{command}; {LOCAL_HELP}")),
        };
    }
    if matches!(trimmed, "exit" | "quit") {
        return Action::Quit;
    }
    if trimmed.starts_with("--") {
        return Action::Refused(format!(
            "not sent: {trimmed:?} would go to the target and looks like an rvdbg option; \
             :q (or Ctrl-C) goes back to rvdbg"
        ));
    }
    if several && let Some(rest) = line.strip_prefix('@') {
        let (name, packet) = rest.split_once(' ').unwrap_or((rest, ""));
        return match find(name) {
            Some(index) if packet.is_empty() => Action::To(index),
            Some(index) => Action::Send(index, packet.to_string()),
            None => unknown(name),
        };
    }
    Action::Send(current, line.to_string())
}

/// The words of one line of a `help` reply that are worth completing:
/// names after a `label:`, a usage's command and its `<a|b>` and `[x]`
/// choices; a `<placeholder>` alone is skipped.
fn help_words(line: &str) -> Vec<String> {
    let body = match line.split_once(": ") {
        Some((label, rest)) if !label.contains(' ') => rest,
        _ => line,
    };
    let mut words = Vec::new();
    for token in body.split_whitespace() {
        let choices = if let Some(inner) = token.strip_prefix('<').and_then(|t| t.strip_suffix('>'))
        {
            if !inner.contains('|') {
                continue;
            }
            inner
        } else if let Some(inner) = token.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
            inner
        } else {
            token
        };
        for word in choices.split('|') {
            if !word.is_empty()
                && word
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            {
                words.push(word.to_string());
            }
        }
    }
    words
}

/// Where the terminal's lines go.
trait Out {
    /// A packet from the target; `from` names its link when several are
    /// watched.
    fn packet(&mut self, from: Option<&str>, text: &str);
    /// A status line (`[fdx] …`), already styled.
    fn note(&mut self, text: &str);
}

/// On a terminal: through rustyline's printer, above the line being typed.
struct Screen {
    printer: Box<dyn rustyline::ExternalPrinter + Send>,
    style: Style,
}

impl Out for Screen {
    fn packet(&mut self, from: Option<&str>, text: &str) {
        let from = from.map_or_else(String::new, |name| {
            format!("{} ", self.style.dim(format!("[{name}]")))
        });
        let _ = self
            .printer
            .print(format!("{from}{} {text}\n", self.style.good("<")));
    }

    fn note(&mut self, text: &str) {
        let _ = self.printer.print(format!("{text}\n"));
    }
}

struct Inline(crate::tui::CommandUi);

impl Out for Inline {
    fn packet(&mut self, from: Option<&str>, text: &str) {
        let line = match from {
            Some(name) => format!("[{name}] < {text}"),
            None => format!("< {text}"),
        };
        self.0.fdx_line(line);
    }

    fn note(&mut self, text: &str) {
        self.0
            .fdx_line(text.strip_prefix("[fdx] ").unwrap_or(text).to_string());
    }
}

/// A managed subscription. Each tick borrows memory and history from the
/// session just for the duration of that tick.
pub(crate) struct Panel {
    pub selection: crate::FdxArgs,
    terminal: Option<Terminal<Inline>>,
    diagnosis: Diagnosis,
    selected: String,
    status: Option<(String, String)>,
    retry_after: Instant,
    ui: crate::tui::CommandUi,
}

impl Panel {
    pub fn new(
        selection: crate::FdxArgs,
        options: Options,
        history: &History,
        ui: crate::tui::CommandUi,
    ) -> Result<Self, String> {
        let selected = options.targets[0].name.clone();
        ui.fdx_open(selected.clone(), options.history_file.clone())?;
        let mut panel = Self {
            selection,
            terminal: None,
            diagnosis: Diagnosis::default(),
            selected,
            status: None,
            retry_after: Instant::now(),
            ui,
        };
        panel.configure(Ok(options));
        if let Some(terminal) = &mut panel.terminal {
            terminal.show_history(history, Instant::now());
        }
        Ok(panel)
    }

    pub fn configure(&mut self, options: Result<Options, String>) {
        self.retry_after = Instant::now();
        self.terminal = match options {
            Ok(options) => {
                self.diagnosis = options.diagnosis;
                let mut terminal =
                    Terminal::new(&options.targets, Inline(self.ui.clone()), Style::PLAIN);
                terminal.current = options
                    .targets
                    .iter()
                    .position(|t| t.name == self.selected)
                    .unwrap_or(0);
                self.selected
                    .clone_from(&options.targets[terminal.current].name);
                Some(terminal)
            }
            Err(error) => {
                self.publish(format!("Unavailable · {error}"));
                None
            }
        };
        if self.terminal.is_some() {
            self.update_status();
        }
    }

    pub fn boundary(&mut self, command: &str) {
        self.retry_after = Instant::now();
        let mut discarded = 0;
        if let Some(terminal) = &mut self.terminal {
            let now = Instant::now();
            for slot in &mut terminal.slots {
                discarded += slot.queue.len();
                *slot = Slot::new(&slot.target, now);
            }
            terminal.help_from = None;
        }
        self.ui.fdx_boundary(format!(
            "--- {command} · discarded {discarded} queued packet(s) ---"
        ));
        self.update_status();
    }

    pub fn ready_to_poll(&self) -> bool {
        self.terminal.is_some() && Instant::now() >= self.retry_after
    }

    pub fn open_targets(&self) -> Vec<Target> {
        self.terminal.as_ref().map_or_else(Vec::new, |terminal| {
            terminal
                .slots
                .iter()
                .filter(|slot| slot.link.is_some())
                .map(|slot| slot.target.clone())
                .collect()
        })
    }

    pub fn input(&mut self, line: &str, history: &History) -> bool {
        if matches!(line.trim(), ":q" | "exit" | "quit") {
            return false;
        }
        if self
            .status
            .as_ref()
            .is_some_and(|(_, status)| status.starts_with("Waiting ·"))
        {
            self.ui
                .fdx_line("Probe unavailable; input was not sent".into());
            return true;
        }
        if let Some(terminal) = &mut self.terminal {
            self.ui.fdx_line(format!("{}> {line}", self.selected));
            if terminal.line(None, line, history, Instant::now()) == Flow::Quit {
                return false;
            }
            self.selected
                .clone_from(&terminal.slots[terminal.current].target.name);
            self.update_status();
        } else {
            self.ui
                .fdx_line("FDX links unavailable; reconnect or change images".into());
        }
        true
    }

    pub fn poll(&mut self, memory: &mut impl Memory, history: &mut History) -> Result<(), String> {
        if let Some(terminal) = &mut self.terminal {
            terminal.poll(memory, &self.diagnosis, history, Instant::now())?;
            self.update_status();
        }
        Ok(())
    }

    pub fn failed(&mut self, error: String) {
        let status = format!("Waiting · {error}");
        if self
            .status
            .as_ref()
            .is_none_or(|(_, previous)| *previous != status)
        {
            self.boundary("probe access interrupted");
        }
        self.retry_after = Instant::now() + Duration::from_millis(500);
        self.publish(status);
    }

    pub fn close(self) {
        self.ui.fdx_closed();
    }

    fn publish(&mut self, status: String) {
        if self
            .status
            .as_ref()
            .is_none_or(|(selected, previous)| *selected != self.selected || *previous != status)
        {
            self.ui.fdx_status(self.selected.clone(), status.clone());
            self.status = Some((self.selected.clone(), status));
        }
    }

    fn update_status(&mut self) {
        if let Some(terminal) = &self.terminal {
            let status = terminal
                .slots
                .iter()
                .map(|slot| {
                    let state = if slot.link.is_some() {
                        "open"
                    } else {
                        "waiting"
                    };
                    format!("{} {state}", slot.target.name)
                })
                .collect::<Vec<_>>()
                .join(" · ");
            self.publish(status);
        }
    }
}

/// Through a pipe: bare packets on stdout (`name: packet` when several are
/// watched), notes on stderr.
struct Plain;

impl Out for Plain {
    fn packet(&mut self, from: Option<&str>, text: &str) {
        let mut stdout = std::io::stdout();
        let _ = match from {
            Some(name) => writeln!(stdout, "{name}: {text}"),
            None => writeln!(stdout, "{text}"),
        };
        let _ = stdout.flush();
    }

    fn note(&mut self, text: &str) {
        eprintln!("{text}");
    }
}

/// A packet sent and not read yet.
struct Inflight {
    text: String,
    sent: Instant,
    /// When it was last reported unread.
    warned: Option<Instant>,
}

/// One watched link.
struct Slot {
    target: Target,
    link: Option<Link>,
    /// Closed: when to look again.
    next_look: Instant,
    /// Closed since (for the diagnosis).
    closed_since: Instant,
    diagnosed: bool,
    /// Typed, not sent yet (no room, or the link is closed).
    queue: VecDeque<String>,
    inflight: Option<Inflight>,
    /// Read by the target at this time, no packet since.
    reply_since: Option<Instant>,
    /// "waiting for the target to open the link" was said.
    said_closed: bool,
}

impl Slot {
    fn new(target: &Target, now: Instant) -> Slot {
        Slot {
            target: target.clone(),
            link: None,
            next_look: now,
            closed_since: now,
            diagnosed: false,
            queue: VecDeque::new(),
            inflight: None,
            reply_since: None,
            said_closed: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Quit,
}

/// The interactive terminal's state, independent of the terminal and the
/// clock so that tests drive it.
struct Terminal<O: Out> {
    slots: Vec<Slot>,
    current: usize,
    out: O,
    style: Style,
    /// `help` was sent to this link at this time: its reply feeds completion.
    help_from: Option<(usize, Instant)>,
    words: Arc<Mutex<Vec<Vec<String>>>>,
    consecutive_errors: u64,
}

impl<O: Out> Terminal<O> {
    fn new(targets: &[Target], out: O, style: Style) -> Self {
        let now = Instant::now();
        Terminal {
            slots: targets
                .iter()
                .map(|target| Slot::new(target, now))
                .collect(),
            current: 0,
            out,
            style,
            help_from: None,
            words: Arc::new(Mutex::new(vec![Vec::new(); targets.len()])),
            consecutive_errors: 0,
        }
    }

    fn several(&self) -> bool {
        self.slots.len() > 1
    }

    fn names(&self) -> Vec<String> {
        self.slots
            .iter()
            .map(|slot| slot.target.name.clone())
            .collect()
    }

    fn label(&self, index: usize) -> Option<String> {
        self.several()
            .then(|| self.slots[index].target.name.clone())
    }

    fn note(&mut self, text: String) {
        let line = format!("{} {text}", self.style.label("[fdx]"));
        self.out.note(&line);
    }

    fn warn(&mut self, text: String) {
        let line = format!("{} {text}", self.style.warn("[fdx]"));
        self.out.note(&line);
    }

    /// Prefix for a link's notes when several are watched.
    fn on(&self, index: usize) -> String {
        self.label(index)
            .map_or_else(String::new, |name| format!("{name}: "))
    }

    /// The session's log of each link, before anything is read.
    fn show_history(&mut self, history: &History, now: Instant) {
        for index in 0..self.slots.len() {
            let address = self.slots[index].target.address;
            let last = history.last(address, SHOWN);
            if last.is_empty() {
                continue;
            }
            let packets: Vec<String> = last
                .iter()
                .map(|(at, packet)| format!("< {packet} ({})", ago(*at, now)))
                .collect();
            let hint = match last.last() {
                Some((_, packet)) if waiting(packet) => "; the target may be waiting for a command",
                _ => "",
            };
            let on = self.on(index);
            self.note(format!(
                "{on}earlier on this link: {}{hint}",
                packets.join(", ")
            ));
        }
    }

    /// A typed line; `target` is the link the input was talking to (`None`:
    /// the terminal's current one).
    fn line(&mut self, target: Option<usize>, text: &str, history: &History, now: Instant) -> Flow {
        let names = self.names();
        let current = target.unwrap_or(self.current);
        match parse(text, &names, current, self.several()) {
            Action::Nothing => {}
            Action::Quit => return Flow::Quit,
            Action::Help => self.note(LOCAL_HELP.to_string()),
            Action::Refused(why) => self.warn(why),
            Action::To(index) => {
                self.current = index;
                self.note(format!("talking to {}", names[index]));
            }
            Action::Last(index) => {
                let address = self.slots[index].target.address;
                let last = history.last(address, LOG);
                let on = self.on(index);
                if last.is_empty() {
                    self.note(format!("{on}nothing received on this link in this session"));
                }
                for (at, packet) in last {
                    self.note(format!("{on}< {packet} ({})", ago(at, now)));
                }
            }
            Action::Send(index, packet) => {
                if self.slots[index].queue.len() >= 32 {
                    self.warn("not queued: the link's input queue is full".to_string());
                    return Flow::Continue;
                }
                if let Some(link) = self.slots[index].link
                    && packet.len() > link.max_payload()
                {
                    let max = link.max_payload();
                    self.warn(format!(
                        "not sent: {} bytes, the link takes up to {max}",
                        packet.len()
                    ));
                    return Flow::Continue;
                }
                if packet == "help" {
                    self.help_from = Some((index, now));
                }
                if self.slots[index].link.is_none() {
                    let on = self.on(index);
                    self.note(format!("{on}queued until the link opens"));
                }
                self.slots[index].queue.push_back(packet);
            }
        }
        Flow::Continue
    }

    /// One look at every link: open, read, send, and the hints.
    fn poll(
        &mut self,
        memory: &mut impl Memory,
        diagnosis: &Diagnosis,
        history: &mut History,
        now: Instant,
    ) -> Result<(), String> {
        for index in 0..self.slots.len() {
            self.poll_slot(memory, index, history, now)?;
        }
        // A link that stays closed: what the others and the stop records
        // say. With several links one diagnosis covers them all.
        let due = self.slots.iter().position(|slot| {
            slot.link.is_none()
                && !slot.diagnosed
                && now.saturating_duration_since(slot.closed_since) >= DIAGNOSE_AFTER
        });
        if let Some(index) = due {
            let several = self.several();
            for (i, slot) in self.slots.iter_mut().enumerate() {
                if several || i == index {
                    slot.diagnosed = true;
                }
            }
            let lines = diagnosis.lines(memory, history, now);
            if !lines.is_empty() {
                let head = if several {
                    "some links are still not open:".to_string()
                } else {
                    format!("{} is still not open:", self.slots[index].target.name)
                };
                self.warn(head);
                for line in lines {
                    self.warn(format!("  {line}"));
                }
            }
        }
        Ok(())
    }

    fn poll_slot(
        &mut self,
        memory: &mut impl Memory,
        index: usize,
        history: &mut History,
        now: Instant,
    ) -> Result<(), String> {
        let several = self.several();
        let on = self.on(index);
        let address = self.slots[index].target.address;
        let link = match self.slots[index].link {
            Some(link) => link,
            None => {
                if now < self.slots[index].next_look {
                    return Ok(());
                }
                self.slots[index].next_look = now + if several { REOPEN_POLL } else { POLL };
                match Link::locate(memory, address) {
                    Ok(Some(link)) => {
                        let slot = &mut self.slots[index];
                        slot.link = Some(link);
                        slot.diagnosed = false;
                        self.note(format!(
                            "{on}open, {} bytes each way, packets up to {} bytes",
                            link.to_target.size,
                            link.max_payload()
                        ));
                        link
                    }
                    Ok(None) => {
                        if !self.slots[index].said_closed {
                            self.slots[index].said_closed = true;
                            self.warn(format!("{on}waiting for the target to open the link"));
                        }
                        return Ok(());
                    }
                    Err(Error::Memory(error)) if error.lost => {
                        return Err(format!("FDX: the connection to the probe is gone: {error}"));
                    }
                    Err(error) => return Err(format!("FDX: {error}")),
                }
            }
        };
        match self.exchange(memory, index, history, link, now) {
            Ok(()) => self.consecutive_errors = 0,
            Err(Error::Closed) => {
                let slot = &mut self.slots[index];
                slot.link = None;
                slot.inflight = None;
                slot.reply_since = None;
                slot.closed_since = now;
                slot.diagnosed = false;
                slot.said_closed = true;
                let discarded = slot.queue.len();
                slot.queue.clear();
                history.forget(address);
                if discarded != 0 {
                    self.warn(format!("{on}discarded {discarded} queued packet(s)"));
                }
                self.warn(format!("{on}the target closed or reopened the link"));
            }
            Err(Error::Memory(error)) if error.lost => {
                return Err(format!("FDX: the connection to the probe is gone: {error}"));
            }
            Err(Error::Memory(error)) => {
                self.consecutive_errors += 1;
                let count = self.consecutive_errors;
                if matches!(count, 5 | 50) || count % 300 == 0 {
                    self.warn(format!("memory access failed x{count}: {error}"));
                }
            }
            Err(error) => return Err(format!("FDX: {error}")),
        }
        Ok(())
    }

    fn exchange(
        &mut self,
        memory: &mut impl Memory,
        index: usize,
        history: &mut History,
        link: Link,
        now: Instant,
    ) -> Result<(), Error> {
        let address = self.slots[index].target.address;
        let label = self.label(index);
        let mut answered = false;
        for _ in 0..16 {
            let Some(packet) = link.receive(memory)? else {
                break;
            };
            match packet {
                Packet::Data(packet) => {
                    let packet = text(&packet);
                    answered = true;
                    history.record(address, &packet, now);
                    self.slots[index].reply_since = None;
                    if let Some((from, since)) = self.help_from
                        && from == index
                        && now.saturating_duration_since(since) < HELP_WINDOW
                        && let Ok(mut words) = self.words.lock()
                    {
                        for word in help_words(&packet) {
                            if !words[index].contains(&word) {
                                words[index].push(word);
                            }
                        }
                    }
                    self.out.packet(label.as_deref(), &packet);
                }
                Packet::Dropped => {
                    let on = self.on(index);
                    self.warn(format!("{on}the target dropped packets: its ring was full"));
                }
            }
        }
        // The packet sent before: read yet?
        if let Some(inflight) = &self.slots[index].inflight {
            if link.unread(memory)? {
                let waited = now.saturating_duration_since(inflight.sent);
                let due = match inflight.warned {
                    None => waited >= UNREAD_AFTER,
                    Some(at) => now.saturating_duration_since(at) >= UNREAD_AGAIN,
                };
                if due {
                    let text = inflight.text.clone();
                    let on = self.on(index);
                    self.warn(format!(
                        "{on}the target has not read {text:?} for {} s: it may not serve this \
                         link now",
                        waited.as_secs()
                    ));
                    if let Some(inflight) = &mut self.slots[index].inflight {
                        inflight.warned = Some(now);
                    }
                }
                return Ok(());
            }
            let warned = inflight.warned.is_some();
            let text = inflight.text.clone();
            self.slots[index].inflight = None;
            if !answered {
                self.slots[index].reply_since = Some(now);
            }
            if warned {
                let on = self.on(index);
                self.note(format!("{on}the target read {text:?}"));
            }
        }
        if let Some(since) = self.slots[index].reply_since
            && now.saturating_duration_since(since) >= REPLY_AFTER
        {
            self.slots[index].reply_since = None;
            let on = self.on(index);
            self.warn(format!("{on}no reply yet"));
        }
        // The next packet, once the one before is read.
        if self.slots[index].inflight.is_none()
            && let Some(packet) = self.slots[index].queue.front().cloned()
        {
            if packet.len() > link.max_payload() {
                self.slots[index].queue.pop_front();
                let max = link.max_payload();
                self.warn(format!(
                    "not sent: {} bytes, the link takes up to {max}",
                    packet.len()
                ));
            } else if link.send(memory, packet.as_bytes())? {
                self.slots[index].queue.pop_front();
                self.slots[index].reply_since = None;
                self.slots[index].inflight = Some(Inflight {
                    text: packet,
                    sent: now,
                    warned: None,
                });
            }
        }
        Ok(())
    }
}

/// A line from the input, with the link it was typed for (`None`: the
/// terminal's current one).
enum Input {
    Line {
        target: Option<usize>,
        text: String,
    },
    /// Ctrl-C, Ctrl-D or a terminal error.
    Quit,
}

/// rustyline's completion of a packet: the words of the link's `help`
/// reply, the links' names after `@` and `:to`, the local commands.
struct FdxHelper {
    words: Arc<Mutex<Vec<Vec<String>>>>,
    current: Arc<AtomicUsize>,
    names: Vec<String>,
}

impl rustyline::completion::Completer for FdxHelper {
    type Candidate = String;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<String>)> {
        let before = &line[..pos];
        let start = before.rfind(' ').map_or(0, |at| at + 1);
        let word = &before[start..];
        let mut candidates: Vec<String> = if start == 0 && word.starts_with('@') {
            self.names.iter().map(|name| format!("@{name}")).collect()
        } else if before.starts_with(":to ") && start == 4 {
            self.names.clone()
        } else if start == 0 && word.starts_with(':') {
            [":q", ":last", ":help", ":to"].map(String::from).to_vec()
        } else {
            let index = self.current.load(Ordering::SeqCst);
            self.words
                .lock()
                .map(|words| words.get(index).cloned().unwrap_or_default())
                .unwrap_or_default()
        };
        candidates.retain(|candidate| candidate.starts_with(word));
        candidates.sort();
        candidates.dedup();
        Ok((start, candidates))
    }
}

impl rustyline::hint::Hinter for FdxHelper {
    type Hint = String;
}
impl rustyline::highlight::Highlighter for FdxHelper {}
impl rustyline::validate::Validator for FdxHelper {}
impl rustyline::Helper for FdxHelper {}

/// The input thread: rustyline reads a line at a time, a paste line by
/// line, and keeps the prompt on the link the lines are for.
fn spawn_reader(
    mut editor: rustyline::Editor<FdxHelper, rustyline::history::DefaultHistory>,
    names: Vec<String>,
    current: Arc<AtomicUsize>,
    history_file: Option<PathBuf>,
    stop: Arc<AtomicBool>,
    lines: mpsc::Sender<Input>,
) -> std::thread::JoinHandle<()> {
    let several = names.len() > 1;
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            let prompt = if several {
                format!("{}> ", names[current.load(Ordering::SeqCst)])
            } else {
                "> ".to_string()
            };
            let Ok(input) = editor.readline(&prompt) else {
                // Ctrl-C, Ctrl-D, or the terminal is gone.
                let _ = lines.send(Input::Quit);
                return;
            };
            for text in input.split('\n') {
                let index = current.load(Ordering::SeqCst);
                if !text.trim().is_empty() {
                    let _ = editor.add_history_entry(text);
                    if let Some(path) = &history_file {
                        let _ = editor.append_history(path);
                    }
                }
                let action = parse(text, &names, index, several);
                if let Action::To(to) = action {
                    current.store(to, Ordering::SeqCst);
                }
                let sent = lines.send(Input::Line {
                    target: Some(index),
                    text: text.to_string(),
                });
                if sent.is_err() || action == Action::Quit {
                    return;
                }
            }
        }
    })
}

fn interactive(
    memory: &mut impl Memory,
    probe: &str,
    options: &Options,
    history: &mut History,
    interrupted: &AtomicBool,
) -> Result<u8, String> {
    let style = Style::stderr();
    match options.targets.as_slice() {
        [target] => eprintln!(
            "{} {} @ {}; one line is one packet; Ctrl-C or :q to stop, :help for more",
            style.label(format!("{probe} FDX:")),
            style.symbol(&target.symbol),
            style.address(format!("0x{:08X}", target.address))
        ),
        targets => eprintln!(
            "{} {}; one line is one packet to the link the prompt names, @<name> <packet> to \
             another; Ctrl-C or :q to stop, :help for more",
            style.label(format!("{probe} FDX:")),
            targets
                .iter()
                .map(|t| format!(
                    "{} {}",
                    t.name,
                    style.address(format!("0x{:08X}", t.address))
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
    let result = if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        on_terminal(memory, options, history, interrupted)
    } else {
        let mut term = Terminal::new(&options.targets, Plain, Style::stderr());
        term.show_history(history, Instant::now());
        let mut pending: Vec<u8> = Vec::new();
        let mut lines: VecDeque<Input> = VecDeque::new();
        let mut open = true;
        drive(
            &mut term,
            memory,
            &options.diagnosis,
            history,
            interrupted,
            || {
                if open {
                    match tracebridge::backend::read_stdin_now() {
                        Some(bytes) => pending.extend(bytes),
                        None => open = false,
                    }
                    while let Some(end) = pending.iter().position(|&b| b == b'\n') {
                        let line: Vec<u8> = pending.drain(..=end).collect();
                        lines.push_back(Input::Line {
                            target: None,
                            text: text(&line),
                        });
                    }
                }
                // At the end of the input the links stay watched until Ctrl-C.
                Some(lines.pop_front())
            },
        )
    };
    eprintln!("{}", style.label(format!("{probe} FDX stopped")));
    result
}

/// The interactive terminal with rustyline reading the input on its own
/// thread.
fn on_terminal(
    memory: &mut impl Memory,
    options: &Options,
    history: &mut History,
    interrupted: &AtomicBool,
) -> Result<u8, String> {
    let current = Arc::new(AtomicUsize::new(0));
    let mut editor = rustyline::Editor::<FdxHelper, rustyline::history::DefaultHistory>::new()
        .map_err(|error| format!("cannot start the line editor: {error}"))?;
    let printer = editor
        .create_external_printer()
        .map_err(|error| format!("cannot print beside the line editor: {error}"))?;
    let mut term = Terminal::new(
        &options.targets,
        Screen {
            printer: Box::new(printer),
            style: Style::stdout(),
        },
        Style::stdout(),
    );
    editor.set_helper(Some(FdxHelper {
        words: term.words.clone(),
        current: current.clone(),
        names: term.names(),
    }));
    if let Some(path) = &options.history_file {
        let _ = editor.load_history(path);
    }
    let (sender, receiver) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let reader = spawn_reader(
        editor,
        term.names(),
        current,
        options.history_file.clone(),
        stop.clone(),
        sender,
    );
    term.show_history(history, Instant::now());
    let result = drive(
        &mut term,
        memory,
        &options.diagnosis,
        history,
        interrupted,
        || match receiver.try_recv() {
            Ok(input) => Some(Some(input)),
            Err(mpsc::TryRecvError::Empty) => Some(None),
            Err(mpsc::TryRecvError::Disconnected) => None,
        },
    );
    stop.store(true, Ordering::SeqCst);
    if !reader.is_finished() {
        // The reader waits in rustyline; only a line ends that.
        eprintln!(
            "{} press Enter to go back to rvdbg",
            Style::stderr().warn("[fdx]")
        );
    }
    let _ = reader.join();
    result
}

/// The loop: input, then one look at every link, until Quit, Ctrl-C or an
/// error. `next` gives the next input (`Some(None)`: none now; `None`: the
/// input has ended for good).
fn drive<O: Out>(
    term: &mut Terminal<O>,
    memory: &mut impl Memory,
    diagnosis: &Diagnosis,
    history: &mut History,
    interrupted: &AtomicBool,
    mut next: impl FnMut() -> Option<Option<Input>>,
) -> Result<u8, String> {
    while !interrupted.load(Ordering::SeqCst) {
        loop {
            match next() {
                Some(Some(Input::Line { target, text })) => {
                    if term.line(target, &text, history, Instant::now()) == Flow::Quit {
                        return Ok(0);
                    }
                }
                Some(Some(Input::Quit)) | None => return Ok(0),
                Some(None) => break,
            }
        }
        term.poll(memory, diagnosis, history, Instant::now())?;
        std::thread::sleep(POLL);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use probecore::fdx::HEADER;

    /// One open link of 64 bytes each way at 0x1000, with a target that
    /// answers every packet with "ok <packet>" while `serving`.
    struct Board {
        bytes: Vec<u8>,
        serving: bool,
    }

    const BASE: u64 = 0x1000;
    const N: usize = 64;

    impl Board {
        fn new() -> Board {
            let mut board = Board {
                bytes: vec![0; 2 * (HEADER + N)],
                serving: true,
            };
            board.put_word(BASE + (HEADER + N) as u64 + 4, N as u32);
            board.put_word(BASE + 4, N as u32);
            board
        }

        fn put_word(&mut self, address: u64, value: u32) {
            let at = (address - BASE) as usize;
            self.bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }

        fn word(&self, address: u64) -> u32 {
            let at = (address - BASE) as usize;
            u32::from_le_bytes(self.bytes[at..at + 4].try_into().unwrap())
        }

        /// The target's side, the way the agent's ring logic works for a
        /// few short packets: read one from `to_target`, answer on `to_host`.
        fn target_step(&mut self) {
            if !self.serving {
                return;
            }
            let to_target = BASE;
            let to_host = BASE + (HEADER + N) as u64;
            let mut index = self.word(to_target + 12) as usize;
            let data = (to_target - BASE) as usize + HEADER;
            let mut mark = self.bytes[data + index];
            if mark == 0xFF {
                index = N - 1;
                mark = self.bytes[data + index];
            }
            if mark == 0 {
                return;
            }
            let len = mark as usize;
            let next = index - len - 1;
            let packet = self.bytes[data + next + 1..data + next + 1 + len].to_vec();
            self.put_word(to_target + 12, next as u32);
            let mut answer = b"ok ".to_vec();
            answer.extend(packet);
            let out = (to_host - BASE) as usize + HEADER;
            let at = self.word(to_host + 12) as usize;
            self.bytes[out + at + 1..out + at + 1 + answer.len()].copy_from_slice(&answer);
            self.bytes[out + at + 1 + answer.len()] = 0;
            self.bytes[out + at] = answer.len() as u8;
            self.put_word(to_host + 12, (at + 1 + answer.len()) as u32);
        }
    }

    impl Memory for Board {
        fn read(&mut self, address: u64, length: usize) -> probecore::Result<Vec<u8>> {
            // The target runs between the debugger's reads.
            self.target_step();
            let at = (address - BASE) as usize;
            Ok(self.bytes[at..at + length].to_vec())
        }
        fn read_u32s(&mut self, address: u64, count: usize) -> probecore::Result<Vec<u32>> {
            Ok((0..count)
                .map(|i| self.word(address + 4 * i as u64))
                .collect())
        }
        fn write(&mut self, _: u64, _: &[u8]) -> probecore::Result<()> {
            unreachable!()
        }
        fn write_u32(&mut self, address: u64, value: u32) -> probecore::Result<()> {
            self.put_word(address, value);
            Ok(())
        }
    }

    fn target() -> Target {
        Target {
            name: "el2".into(),
            symbol: "RUSTVISOR_FDX".into(),
            address: BASE,
        }
    }

    fn options(send: &[&str], expect: &[&str]) -> Options {
        Options {
            targets: vec![target()],
            send: send.iter().map(|s| s.to_string()).collect(),
            expect: expect.iter().map(|s| s.to_string()).collect(),
            timeout: Duration::from_millis(300),
            json: true,
            diagnosis: Diagnosis::default(),
            history_file: None,
        }
    }

    /// The lines a terminal printed.
    #[derive(Default)]
    struct Lines(Vec<String>);

    impl Out for &mut Lines {
        fn packet(&mut self, from: Option<&str>, text: &str) {
            self.0.push(match from {
                Some(name) => format!("[{name}] < {text}"),
                None => format!("< {text}"),
            });
        }
        fn note(&mut self, text: &str) {
            self.0.push(text.to_string());
        }
    }

    #[test]
    fn send_then_expect_the_answer() {
        let mut board = Board::new();
        let stop = AtomicBool::new(false);
        let mut history = History::default();
        let code = run(
            &mut board,
            "TEST",
            &options(&["boot single"], &["ok boot single"]),
            &mut history,
            &stop,
        );
        assert_eq!(code, Ok(0));
        // The session remembers what came.
        assert_eq!(history.last(BASE, 5)[0].1, "ok boot single");
    }

    #[test]
    fn a_missing_answer_exits_3() {
        let mut board = Board::new();
        let stop = AtomicBool::new(false);
        let code = run(
            &mut board,
            "TEST",
            &options(&["boot"], &["ready"]),
            &mut History::default(),
            &stop,
        );
        assert_eq!(code, Ok(EXIT_FAILED));
    }

    #[test]
    fn a_closed_link_times_out_and_long_packets_are_refused() {
        let mut board = Board::new();
        board.put_word(BASE + 4, 0);
        let stop = AtomicBool::new(false);
        let error = run(
            &mut board,
            "TEST",
            &options(&["x"], &[]),
            &mut History::default(),
            &stop,
        )
        .unwrap_err();
        assert!(error.contains("did not open"), "{error}");
        let mut board = Board::new();
        let long = "x".repeat(40);
        let error = run(
            &mut board,
            "TEST",
            &options(&[long.as_str()], &[]),
            &mut History::default(),
            &stop,
        )
        .unwrap_err();
        assert!(error.contains("1 to 29 bytes"), "{error}");
    }

    #[test]
    fn local_commands_and_options_are_not_sent() {
        let names = vec!["el2".to_string(), "guest0".to_string()];
        assert_eq!(parse("exit", &names, 0, false), Action::Quit);
        assert_eq!(parse(" quit ", &names, 0, false), Action::Quit);
        assert_eq!(parse(":q", &names, 0, false), Action::Quit);
        assert_eq!(parse(":last", &names, 1, true), Action::Last(1));
        assert!(matches!(
            parse("--guest 0", &names, 0, false),
            Action::Refused(_)
        ));
        assert_eq!(parse("::x", &names, 0, false), Action::Send(0, ":x".into()));
        assert_eq!(parse("", &names, 0, false), Action::Nothing);
        assert_eq!(
            parse("run timer", &names, 1, true),
            Action::Send(1, "run timer".into())
        );
        // @ and :to only when several links are watched.
        assert_eq!(
            parse("@guest0 run timer", &names, 0, true),
            Action::Send(1, "run timer".into())
        );
        assert_eq!(parse("@guest0", &names, 0, true), Action::To(1));
        assert_eq!(parse(":to el2", &names, 1, true), Action::To(0));
        assert!(matches!(
            parse("@guest9 x", &names, 0, true),
            Action::Refused(_)
        ));
        assert_eq!(
            parse("@x y", &names, 0, false),
            Action::Send(0, "@x y".into())
        );
        assert!(matches!(
            parse(":to el2", &names, 0, false),
            Action::Refused(_)
        ));
    }

    #[test]
    fn help_replies_give_words_to_complete() {
        assert_eq!(help_words("run <scenario>"), ["run"]);
        assert_eq!(
            help_words("lab <deployment> <inject|direct> [pin] [trace]"),
            ["lab", "inject", "direct", "pin", "trace"]
        );
        assert_eq!(help_words("scenarios: none timer"), ["none", "timer"]);
        assert_eq!(help_words("ping-vm0 ping-vm1"), ["ping-vm0", "ping-vm1"]);
    }

    #[test]
    fn packets_are_sent_answered_and_logged() {
        let mut board = Board::new();
        let mut lines = Lines::default();
        let mut history = History::default();
        let mut term = Terminal::new(&[target()], &mut lines, Style::PLAIN);
        let t0 = term.slots[0].closed_since;
        let diagnosis = Diagnosis::default();
        assert_eq!(term.line(None, "--guest 0", &history, t0), Flow::Continue);
        assert_eq!(term.line(None, "help", &history, t0), Flow::Continue);
        for _ in 0..3 {
            term.poll(&mut board, &diagnosis, &mut history, t0).unwrap();
        }
        // Answered at once: no "no reply yet" later.
        term.poll(
            &mut board,
            &diagnosis,
            &mut history,
            t0 + Duration::from_secs(3),
        )
        .unwrap();
        assert_eq!(term.line(None, "exit", &history, t0), Flow::Quit);
        assert_eq!(term.words.lock().unwrap()[0], ["ok", "help"]);
        drop(term);
        assert!(lines.0[0].contains("not sent"), "{:?}", lines.0);
        assert!(
            lines.0[1].contains("queued until the link opens"),
            "{:?}",
            lines.0
        );
        assert!(lines.0[2].starts_with("[fdx] open"), "{:?}", lines.0);
        assert_eq!(lines.0[3], "< ok help");
        assert_eq!(lines.0.len(), 4, "{:?}", lines.0);
        assert_eq!(history.last(BASE, 1)[0].1, "ok help");
    }

    #[test]
    fn an_unread_packet_and_a_missing_reply_are_reported() {
        let mut board = Board::new();
        board.serving = false;
        let mut lines = Lines::default();
        let mut history = History::default();
        let mut term = Terminal::new(&[target()], &mut lines, Style::PLAIN);
        let t0 = term.slots[0].closed_since;
        let diagnosis = Diagnosis::default();
        let at = |ms| t0 + Duration::from_millis(ms);
        term.line(None, "lab five", &history, t0);
        term.poll(&mut board, &diagnosis, &mut history, t0).unwrap();
        term.poll(&mut board, &diagnosis, &mut history, at(500))
            .unwrap();
        term.poll(&mut board, &diagnosis, &mut history, at(1100))
            .unwrap();
        // The target takes it and stays silent.
        let host = board.word(BASE + 8);
        board.put_word(BASE + 12, host);
        term.poll(&mut board, &diagnosis, &mut history, at(1200))
            .unwrap();
        term.poll(&mut board, &diagnosis, &mut history, at(3300))
            .unwrap();
        drop(term);
        let text = lines.0.join("\n");
        assert!(text.contains("has not read \"lab five\" for 1 s"), "{text}");
        assert!(text.contains("the target read \"lab five\""), "{text}");
        assert!(text.contains("no reply yet"), "{text}");
    }

    #[test]
    fn entering_again_shows_what_came_before() {
        let mut history = History::default();
        let t0 = Instant::now();
        history.record(BASE, "ready", t0);
        let mut lines = Lines::default();
        let mut term = Terminal::new(&[target()], &mut lines, Style::PLAIN);
        term.show_history(&history, t0 + Duration::from_secs(35));
        drop(term);
        assert_eq!(
            lines.0,
            [
                "[fdx] earlier on this link: < ready (35 s ago); the target may be waiting for a \
              command"
            ]
        );
    }

    #[test]
    fn several_links_carry_their_names_and_a_closed_one_is_diagnosed() {
        let mut board = Board::new();
        let mut lines = Lines::default();
        let mut history = History::default();
        let closed = Target {
            name: "guest0".into(),
            symbol: "RUSTVISOR_FDX".into(),
            // In the board's RAM, with a size word of 0: closed.
            address: BASE + 0x40,
        };
        let targets = [target(), closed.clone()];
        let mut term = Terminal::new(&targets, &mut lines, Style::PLAIN);
        let diagnosis = Diagnosis {
            peers: vec![target(), closed],
            ..Diagnosis::default()
        };
        let t0 = term.slots[0].closed_since;
        term.line(None, "@el2 go", &history, t0);
        term.poll(&mut board, &diagnosis, &mut history, t0).unwrap();
        term.poll(&mut board, &diagnosis, &mut history, t0).unwrap();
        term.poll(
            &mut board,
            &diagnosis,
            &mut history,
            t0 + Duration::from_secs(4),
        )
        .unwrap();
        drop(term);
        let text = lines.0.join("\n");
        assert!(text.contains("[el2] < ok go"), "{text}");
        assert!(text.contains("guest0: waiting for the target"), "{text}");
        assert!(text.contains("some links are still not open"), "{text}");
        assert!(
            text.contains("el2 is open; its last packet was \"ok go\""),
            "{text}"
        );
        assert!(text.contains("guest0 is not open"), "{text}");
    }
}
