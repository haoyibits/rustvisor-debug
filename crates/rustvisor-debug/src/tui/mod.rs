//! Interactive terminal UI for `rvdbg`.
//!
//! - Default (`ScreenMode::Fullscreen`): flicker-free alternate screen with a
//!   top session bar, scrollable soft-wrapped transcript, wide-screen right
//!   FDX split pane (or collapsible drawer), live activity strip, rounded
//!   multiline One Dark composer, and structured status line.
//! - Inline (`--inline` / `Alt+F` / `/inline`): dynamic-height synchronized
//!   bottom surface preserving native terminal scrollback.
//! - The UI thread owns stdin/stdout; one dedicated worker thread owns the
//!   debugger `Session` and serialized probe access.

mod ansi;
mod complete;
mod composer;
mod fdx_view;
mod surface;
mod theme;
mod transcript;

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use probecore::output::{self, Line as Output};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};
use unicode_width::UnicodeWidthStr;

use crate::config::{Backend, Config};
use crate::session::{self, Flow, Session};
use complete::{Candidate, CommandSpec, POPUP_MAX_ROWS, Picker, SymbolCache};
use composer::{Composer, ComposerAction};
use fdx_view::{FdxView, InputMode, WIDE_SPLIT_MIN_WIDTH};
use surface::Surface;
use theme::{SPINNER, Theme};
use transcript::{Transcript, format_duration};

pub(crate) use surface::ScreenMode;

const POLL: Duration = Duration::from_millis(40);
const SPINNER_TICK: Duration = Duration::from_millis(80);
const TOAST_TTL: Duration = Duration::from_secs(3);

pub(crate) struct Status {
    pub target: String,
    pub backend: Option<Backend>,
    pub config_path: PathBuf,
    pub images_summary: String,
    pub image_paths: Vec<PathBuf>,
    pub port_summary: Option<String>,
}

enum Message {
    Banner(String),
    Text(String, bool),
    Progress(String),
    RttBytes(Vec<u8>),
    Started {
        name: String,
        cancellable: bool,
    },
    RttInput(SyncSender<Vec<u8>>),
    FdxOpen {
        target: String,
        path: Option<PathBuf>,
        all: bool,
    },
    FdxStatus {
        target: String,
        status: String,
    },
    FdxWords(Vec<String>),
    FdxLine(String),
    FdxBoundary(String),
    FdxClosed,
    Finished {
        status: Status,
        code: u8,
        flow: Flow,
    },
}

enum Request {
    Command(String),
    FdxInput(String),
    FdxClose,
}

#[derive(Clone)]
pub(crate) struct CommandUi(SyncSender<Message>);

impl CommandUi {
    fn send(&self, message: Message) -> Result<(), String> {
        self.0.send(message).map_err(|_| "terminal closed".into())
    }

    pub fn begin(&self, name: &str, cancellable: bool) -> Result<(), String> {
        self.send(Message::Started {
            name: name.into(),
            cancellable,
        })
    }

    pub fn rtt(&self) -> Result<Receiver<Vec<u8>>, String> {
        let (input, receiver) = mpsc::sync_channel(32);
        self.send(Message::RttInput(input))?;
        Ok(receiver)
    }

    pub fn fdx_open(&self, target: String, path: Option<PathBuf>, all: bool) -> Result<(), String> {
        self.send(Message::FdxOpen { target, path, all })
    }

    pub fn fdx_status(&self, target: String, status: String) {
        let _ = self.send(Message::FdxStatus { target, status });
    }

    pub fn fdx_words(&self, words: Vec<String>) {
        let _ = self.send(Message::FdxWords(words));
    }

    pub fn fdx_line(&self, line: String) {
        let _ = self.send(Message::FdxLine(line));
    }

    pub fn fdx_boundary(&self, line: String) {
        let _ = self.send(Message::FdxBoundary(line));
    }

    pub fn fdx_closed(&self) {
        let _ = self.send(Message::FdxClosed);
    }
}

fn worker(
    config: Config,
    flag: Option<Backend>,
    color: bool,
    requests: Receiver<Request>,
    messages: SyncSender<Message>,
    interrupted: Arc<AtomicBool>,
) {
    let sink = messages.clone();
    let style = if color {
        probecore::style::Style::COLOR
    } else {
        probecore::style::Style::PLAIN
    };
    output::with_colored_sink(
        color,
        move |line| {
            let message = match line {
                Output::Text(text) => Message::Text(text.into(), false),
                Output::Error(text) => Message::Text(text.into(), true),
                Output::Progress(text) => Message::Progress(text.into()),
                Output::Rtt(bytes) => Message::RttBytes(bytes.into()),
            };
            let _ = sink.send(message);
        },
        || {
            let mut session = Session::new(config, flag, false, style);
            session.use_tui(CommandUi(messages.clone()), interrupted);
            session.connect(None);
            let _ = messages.send(Message::Banner(session.tui_header()));
            if messages
                .send(Message::Finished {
                    status: session.tui_status(),
                    flow: Flow::Continue,
                    code: 0,
                })
                .is_err()
            {
                return;
            }
            let mut next_poll = Instant::now();
            loop {
                let request = if session.has_fdx_panel() {
                    match requests.recv_timeout(next_poll.saturating_duration_since(Instant::now()))
                    {
                        Ok(request) => request,
                        Err(RecvTimeoutError::Timeout) => {
                            session.poll_fdx();
                            next_poll = Instant::now() + crate::fdx_terminal::POLL;
                            continue;
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                } else {
                    let Ok(request) = requests.recv() else { break };
                    request
                };
                match request {
                    Request::Command(command) => {
                        let flow = session.handle_input(&command);
                        let status = session.tui_status();
                        if messages
                            .send(Message::Finished {
                                status,
                                flow,
                                code: session.last_code(),
                            })
                            .is_err()
                            || flow == Flow::Quit
                        {
                            break;
                        }
                    }
                    Request::FdxInput(input) => {
                        for line in input.lines() {
                            session.fdx_input(line);
                        }
                    }
                    Request::FdxClose => session.close_fdx_panel(),
                }
                if session.has_fdx_panel() && Instant::now() >= next_poll {
                    session.poll_fdx();
                    next_poll = Instant::now() + crate::fdx_terminal::POLL;
                }
            }
        },
    );
}

struct Running {
    name: String,
    since: Instant,
    cancellable: bool,
    cancelling: bool,
}

struct Popup {
    title: String,
    items: Vec<Candidate>,
    selected: usize,
    is_slash: bool,
}

impl Popup {
    fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    fn move_down(&mut self) {
        if !self.items.is_empty() {
            self.selected = (self.selected + 1).min(self.items.len() - 1);
        }
    }

    fn chosen(&self) -> Option<&Candidate> {
        self.items.get(self.selected)
    }
}

struct Toast {
    text: String,
    until: Instant,
}

struct App {
    config: Config,
    cwd: PathBuf,
    theme: Theme,
    transcript: Transcript,
    composer: Composer,
    fdx_composer: Composer,
    fdx: FdxView,
    mode: InputMode,
    specs: Vec<CommandSpec>,
    symbols: SymbolCache,
    popup: Option<Popup>,
    slash_dismissed: bool,
    picker: Option<Picker>,
    status: Status,
    last_code: u8,
    progress: Option<String>,
    running: Option<Running>,
    queued_command: Option<String>,
    rtt: Option<SyncSender<Vec<u8>>>,
    rtt_partial: Vec<u8>,
    toast: Option<Toast>,
    spinner_frame: usize,
    last_fdx_rect: Rect,
}

impl App {
    fn new(config: Config, cwd: PathBuf, theme: Theme) -> Self {
        let history = session::history_path(&config);
        let mut symbols = SymbolCache::default();
        let image_paths = config.images();
        symbols.refresh(&image_paths);
        let status = Status {
            target: "Detecting probe".into(),
            backend: None,
            config_path: config.file.clone(),
            images_summary: format_images_summary(&config),
            image_paths,
            port_summary: None,
        };
        Self {
            config,
            cwd,
            theme,
            transcript: Transcript::new(),
            composer: Composer::new(history),
            fdx_composer: Composer::new(None),
            fdx: FdxView::new(),
            mode: InputMode::Command,
            specs: complete::build_specs(None),
            symbols,
            popup: None,
            slash_dismissed: false,
            picker: None,
            status,
            last_code: 0,
            progress: None,
            running: Some(Running {
                name: "Connecting".into(),
                since: Instant::now(),
                cancellable: false,
                cancelling: false,
            }),
            queued_command: None,
            rtt: None,
            rtt_partial: Vec::new(),
            toast: None,
            spinner_frame: 0,
            last_fdx_rect: Rect::default(),
        }
    }

    fn set_toast(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            until: Instant::now() + TOAST_TTL,
        });
    }

    fn active_toast(&self) -> Option<&str> {
        self.toast
            .as_ref()
            .filter(|t| Instant::now() < t.until)
            .map(|t| t.text.as_str())
    }

    fn active_composer(&self) -> &Composer {
        if self.mode == InputMode::Command {
            &self.composer
        } else {
            &self.fdx_composer
        }
    }

    fn active_composer_mut(&mut self) -> &mut Composer {
        if self.mode == InputMode::Command {
            &mut self.composer
        } else {
            &mut self.fdx_composer
        }
    }

    fn flush_rtt(&mut self) {
        if !self.rtt_partial.is_empty() {
            let text = String::from_utf8_lossy(&self.rtt_partial).into_owned();
            self.transcript.push_output(&text, false, self.theme);
            self.rtt_partial.clear();
        }
    }

    fn handle_message(
        &mut self,
        message: Message,
        interrupted: &AtomicBool,
        requests: &mpsc::Sender<Request>,
    ) -> Result<bool, String> {
        match message {
            Message::Banner(text) => {
                self.transcript.push_banner(&text, self.theme);
            }
            Message::Text(text, is_error) => {
                self.flush_rtt();
                self.transcript.push_output(&text, is_error, self.theme);
            }
            Message::Progress(text) => {
                self.progress = Some(text);
            }
            Message::RttBytes(bytes) => {
                self.rtt_partial.extend(bytes);
                if let Some(end) = self.rtt_partial.iter().rposition(|&b| b == b'\n') {
                    let text = String::from_utf8_lossy(&self.rtt_partial[..end]).into_owned();
                    self.transcript.push_output(&text, false, self.theme);
                    self.rtt_partial.drain(..=end);
                }
                if self.rtt_partial.len() >= 16 * 1024 {
                    self.flush_rtt();
                }
            }
            Message::Started { name, cancellable } => {
                self.flush_rtt();
                self.rtt = None;
                self.progress = None;
                let cancelling = self
                    .running
                    .as_ref()
                    .is_some_and(|running| running.cancelling);
                self.running = Some(Running {
                    name,
                    since: Instant::now(),
                    cancellable,
                    cancelling,
                });
            }
            Message::RttInput(input) => {
                self.rtt = Some(input);
                self.popup = None;
                self.picker = None;
            }
            Message::FdxOpen { target, path, all } => {
                self.fdx.open(target);
                self.fdx_composer = Composer::new(path);
                self.mode = if all {
                    InputMode::FdxAll
                } else {
                    InputMode::FdxSingle
                };
                self.popup = None;
                self.picker = None;
            }
            Message::FdxStatus { target, status } => {
                self.fdx.update_status(target, status);
                if self.mode == InputMode::FdxSingle && self.fdx.links.len() > 1 {
                    self.mode = InputMode::FdxAll;
                }
            }
            Message::FdxWords(words) => {
                self.fdx.set_words(words);
            }
            Message::FdxLine(line) => {
                let focused = self.mode != InputMode::Command;
                self.fdx.push_line(&line, self.theme, false, focused);
            }
            Message::FdxBoundary(line) => {
                self.fdx_composer.clear();
                let focused = self.mode != InputMode::Command;
                self.fdx.push_line(&line, self.theme, true, focused);
            }
            Message::FdxClosed => {
                self.fdx.close();
                self.mode = InputMode::Command;
                self.popup = None;
            }
            Message::Finished { status, code, flow } => {
                self.flush_rtt();
                let elapsed = self
                    .running
                    .as_ref()
                    .map(|r| r.since.elapsed())
                    .unwrap_or_default();
                let was_cancelled = self.running.as_ref().is_some_and(|r| r.cancelling);
                self.transcript.finish_command(code, elapsed, self.theme);
                self.symbols.refresh(&status.image_paths);
                self.specs = complete::build_specs(status.backend);
                self.status = status;
                self.last_code = code;
                self.running = None;
                self.progress = None;
                self.rtt = None;
                if flow == Flow::Quit {
                    return Ok(true);
                }
                if let Some(queued) = self.queued_command.take() {
                    if code == 0 && !was_cancelled {
                        self.dispatch_command(queued, interrupted, requests)?;
                    } else if self.composer.is_empty() {
                        self.composer.set_text(queued);
                        self.set_toast("Queued command restored after failure");
                    }
                }
            }
        }
        Ok(false)
    }

    fn dispatch_command(
        &mut self,
        text: String,
        interrupted: &AtomicBool,
        requests: &mpsc::Sender<Request>,
    ) -> Result<(), String> {
        interrupted.store(false, Ordering::SeqCst);
        self.transcript.begin_command(&text, self.theme);
        requests
            .send(Request::Command(text))
            .map_err(|_| "debug session worker stopped".to_string())?;
        self.running = Some(Running {
            name: "Executing".into(),
            since: Instant::now(),
            cancellable: true,
            cancelling: false,
        });
        Ok(())
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, surface_mode: ScreenMode) {
        if surface_mode != ScreenMode::Fullscreen {
            return;
        }
        let over_fdx = self.fdx.active
            && !self.last_fdx_rect.is_empty()
            && mouse.column >= self.last_fdx_rect.x
            && mouse.column < self.last_fdx_rect.right()
            && mouse.row >= self.last_fdx_rect.y
            && mouse.row < self.last_fdx_rect.bottom();
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if over_fdx {
                    self.fdx.scroll_up(3);
                } else {
                    self.transcript.scroll_up(3);
                }
            }
            MouseEventKind::ScrollDown => {
                if over_fdx {
                    self.fdx.scroll_down(3);
                } else {
                    self.transcript.scroll_down(3);
                }
            }
            _ => {}
        }
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        interrupted: &AtomicBool,
        requests: &mpsc::Sender<Request>,
        surface: &mut Surface,
    ) -> Result<bool, String> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        // Global shortcuts that work regardless of composer mode (outside RTT raw passthrough).
        if self.rtt.is_none() {
            if alt && matches!(key.code, KeyCode::Char('f' | 'F')) {
                let next = match surface.mode() {
                    ScreenMode::Fullscreen => ScreenMode::Inline,
                    ScreenMode::Inline => ScreenMode::Fullscreen,
                };
                surface.set_mode(next).map_err(io_error)?;
                if next == ScreenMode::Inline {
                    self.transcript.discard_inline_queue();
                }
                self.set_toast(match next {
                    ScreenMode::Fullscreen => "Switched to fullscreen mode (Alt+F)",
                    ScreenMode::Inline => "Switched to inline mode (Alt+F)",
                });
                return Ok(false);
            }
            if ctrl && key.code == KeyCode::Char('l') {
                self.transcript.clear();
                surface.clear_screen().map_err(io_error)?;
                return Ok(false);
            }
            if ctrl && key.code == KeyCode::Char('o') && self.fdx.active {
                self.fdx.toggle_expanded();
                return Ok(false);
            }
            if key.code == KeyCode::BackTab || key.code == KeyCode::F(6) {
                self.cycle_mode(interrupted, requests)?;
                return Ok(false);
            }
        }

        // Handle active RTT console passthrough.
        if let Some(running) = &mut self.running {
            if running.cancellable && ctrl && key.code == KeyCode::Char('c') {
                interrupted.store(true, Ordering::SeqCst);
                running.cancelling = true;
                return Ok(false);
            }
            if let Some(rtt_tx) = &self.rtt {
                if !running.cancelling
                    && let Some(bytes) = rtt_key(key)
                {
                    if let Err(mpsc::TrySendError::Full(_)) = rtt_tx.try_send(bytes) {
                        self.transcript
                            .push_note("RTT input buffer is full", self.theme);
                    }
                }
                return Ok(false);
            }
        }

        // Interactive picker modal (`/images`, `/backend`, `/fdx`).
        if self.picker.is_some() {
            return self.handle_picker_key(key, interrupted, requests);
        }

        // Scroll keys in Fullscreen or expanded FDX drawer.
        match key.code {
            KeyCode::PageUp => {
                if self.mode != InputMode::Command && self.fdx.active {
                    self.fdx.scroll_up(8);
                } else {
                    self.transcript.scroll_up(10);
                }
                return Ok(false);
            }
            KeyCode::PageDown => {
                if self.mode != InputMode::Command && self.fdx.active {
                    self.fdx.scroll_down(8);
                } else {
                    self.transcript.scroll_down(10);
                }
                return Ok(false);
            }
            KeyCode::Home if ctrl => {
                self.transcript.scroll_to_top();
                return Ok(false);
            }
            KeyCode::End
                if ctrl
                    || (self.active_composer().is_empty()
                        && self.transcript.scroll_offset() > 0) =>
            {
                self.transcript.scroll_to_bottom();
                self.fdx.scroll_to_bottom();
                return Ok(false);
            }
            _ => {}
        }

        // Popup navigation when completion menu is open.
        if let Some(popup) = &mut self.popup {
            match key.code {
                KeyCode::Up => {
                    popup.move_up();
                    return Ok(false);
                }
                KeyCode::Down => {
                    popup.move_down();
                    return Ok(false);
                }
                KeyCode::Esc => {
                    let was_slash = popup.is_slash;
                    self.popup = None;
                    if was_slash {
                        self.slash_dismissed = true;
                    }
                    return Ok(false);
                }
                KeyCode::Tab => {
                    let chosen = popup.chosen().cloned();
                    self.popup = None;
                    if let Some(cand) = chosen {
                        self.apply_candidate(&cand, false, interrupted, requests, surface)?;
                    }
                    return Ok(false);
                }
                KeyCode::Enter
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
                {
                    let chosen = popup.chosen().cloned();
                    let is_slash = popup.is_slash;
                    self.popup = None;
                    if let Some(cand) = chosen {
                        if let Some(reason) = cand.unavailable {
                            self.set_toast(format!("{} unavailable: {reason}", cand.label));
                            return Ok(false);
                        }
                        if is_slash && cand.run_on_enter {
                            self.composer.clear();
                            return self.execute_submitted_command(
                                cand.insert,
                                interrupted,
                                requests,
                                surface,
                            );
                        }
                        self.apply_candidate(&cand, true, interrupted, requests, surface)?;
                    }
                    return Ok(false);
                }
                _ => {}
            }
        }

        // Accept inline ghost completion on Right at end of line, or trigger Tab completion.
        if key.code == KeyCode::Right
            && key.modifiers.is_empty()
            && self.mode == InputMode::Command
            && self.composer.search.is_none()
            && let Some(suffix) =
                complete::ghost_suffix(&self.composer.text, self.composer.cursor, &self.specs)
        {
            self.composer.insert(&suffix);
            return Ok(false);
        }

        if key.code == KeyCode::Tab && key.modifiers.is_empty() {
            self.trigger_tab_complete(interrupted, requests, surface)?;
            return Ok(false);
        }

        if key.code == KeyCode::Esc && self.active_composer().search.is_none() {
            if self.running.as_ref().is_some_and(|r| r.cancellable) {
                interrupted.store(true, Ordering::SeqCst);
                if let Some(running) = &mut self.running {
                    running.cancelling = true;
                }
            } else if !self.active_composer().is_empty() {
                self.active_composer_mut().clear();
                self.slash_dismissed = false;
            }
            return Ok(false);
        }

        // Route editing key to the active composer.
        let action = self.active_composer_mut().handle_key(key);
        match action {
            ComposerAction::None => {}
            ComposerAction::Changed => {
                self.slash_dismissed = false;
                self.refresh_slash_popup();
            }
            ComposerAction::ConfirmQuit => {
                if self.mode != InputMode::Command {
                    self.set_toast("Press Ctrl+D again to close FDX panel");
                } else {
                    self.set_toast("Press Ctrl+D again to exit rvdbg");
                }
            }
            ComposerAction::Quit => {
                if self.mode != InputMode::Command {
                    requests
                        .send(Request::FdxClose)
                        .map_err(|_| "debug session worker stopped".to_string())?;
                    return Ok(false);
                }
                return Ok(true);
            }
            ComposerAction::Cancel => {
                if self.mode != InputMode::Command {
                    self.mode = InputMode::Command;
                    self.popup = None;
                }
            }
            ComposerAction::Submit(text) => {
                self.popup = None;
                self.slash_dismissed = false;
                if self.mode != InputMode::Command {
                    if let Some(escaped) = text.trim_start().strip_prefix('!') {
                        let cmd = escaped.trim().to_string();
                        if !cmd.is_empty() {
                            return self.execute_submitted_command(
                                cmd,
                                interrupted,
                                requests,
                                surface,
                            );
                        }
                    } else if self.running.is_some() {
                        self.fdx_composer.set_text(text);
                        self.set_toast("Wait for current command to finish before sending FDX");
                    } else {
                        requests
                            .send(Request::FdxInput(text))
                            .map_err(|_| "debug session worker stopped".to_string())?;
                    }
                } else {
                    return self.execute_submitted_command(text, interrupted, requests, surface);
                }
            }
        }
        Ok(false)
    }

    fn handle_picker_key(
        &mut self,
        key: KeyEvent,
        interrupted: &AtomicBool,
        requests: &mpsc::Sender<Request>,
    ) -> Result<bool, String> {
        let Some(picker) = &mut self.picker else {
            return Ok(false);
        };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => picker.move_up(),
            KeyCode::Down | KeyCode::Char('j') => picker.move_down(),
            KeyCode::Esc => self.picker = None,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.picker = None;
            }
            KeyCode::Enter => {
                let chosen = picker.chosen().cloned();
                self.picker = None;
                if let Some(item) = chosen {
                    if item.command == "/fdx-close" {
                        requests
                            .send(Request::FdxClose)
                            .map_err(|_| "debug session worker stopped".to_string())?;
                    } else if self.running.is_some() {
                        self.queued_command = Some(item.command);
                        self.set_toast("Queued until current command finishes");
                    } else {
                        self.dispatch_command(item.command, interrupted, requests)?;
                    }
                }
            }
            _ => {}
        }
        Ok(false)
    }

    fn cycle_mode(
        &mut self,
        interrupted: &AtomicBool,
        requests: &mpsc::Sender<Request>,
    ) -> Result<(), String> {
        self.popup = None;
        self.picker = None;
        if !self.fdx.active {
            if self.running.is_some() {
                self.set_toast("Wait for running command before opening FDX");
                return Ok(());
            }
            let cmd = if self.status.image_paths.len() <= 1 {
                "fdx".to_string()
            } else {
                "fdx --all".to_string()
            };
            self.dispatch_command(cmd, interrupted, requests)?;
            return Ok(());
        }
        self.mode = match self.mode {
            InputMode::Command => {
                self.fdx.mark_seen();
                if self.fdx.links.len() > 1 {
                    InputMode::FdxAll
                } else {
                    InputMode::FdxSingle
                }
            }
            InputMode::FdxSingle => {
                if self.fdx.links.len() > 1 {
                    InputMode::FdxAll
                } else {
                    InputMode::Command
                }
            }
            InputMode::FdxAll => InputMode::Command,
        };
        Ok(())
    }

    fn refresh_slash_popup(&mut self) {
        if self.mode != InputMode::Command || self.composer.search.is_some() {
            self.popup = None;
            return;
        }
        if !self.composer.text.starts_with('/') || self.slash_dismissed {
            self.popup = None;
            return;
        }
        let items = complete::slash_candidates(&self.composer.text, &self.specs);
        if items.is_empty() {
            self.popup = None;
        } else {
            self.popup = Some(Popup {
                title: "Commands & Actions".into(),
                items,
                selected: 0,
                is_slash: true,
            });
        }
    }

    fn trigger_tab_complete(
        &mut self,
        interrupted: &AtomicBool,
        requests: &mpsc::Sender<Request>,
        surface: &mut Surface,
    ) -> Result<(), String> {
        if self.active_composer().search.is_some() {
            return Ok(());
        }
        let candidates = if self.mode == InputMode::Command {
            let links = self.fdx.link_names();
            complete::command_tab_candidates(
                &self.composer.text,
                self.composer.cursor,
                &self.specs,
                &self.symbols,
                &links,
                &self.cwd,
            )
        } else {
            let links = self.fdx.link_names();
            complete::fdx_tab_candidates(
                &self.fdx_composer.text,
                self.fdx_composer.cursor,
                &links,
                &self.fdx.target_words,
            )
        };
        match candidates.len() {
            0 => self.popup = None,
            1 => {
                let only = candidates.into_iter().next().unwrap();
                self.apply_candidate(&only, false, interrupted, requests, surface)?;
            }
            _ => {
                let is_slash =
                    self.mode == InputMode::Command && self.composer.text.starts_with('/');
                self.popup = Some(Popup {
                    title: if is_slash {
                        "Commands & Actions".into()
                    } else if self.mode != InputMode::Command {
                        "FDX Completions".into()
                    } else {
                        "Completions".into()
                    },
                    items: candidates,
                    selected: 0,
                    is_slash,
                });
            }
        }
        Ok(())
    }

    fn apply_candidate(
        &mut self,
        cand: &Candidate,
        from_enter: bool,
        _interrupted: &AtomicBool,
        _requests: &mpsc::Sender<Request>,
        _surface: &mut Surface,
    ) -> Result<(), String> {
        let mut replacement = cand.insert.clone();
        if (cand.add_space || (from_enter && !replacement.ends_with('/')))
            && !replacement.ends_with(' ')
            && !replacement.ends_with('/')
        {
            replacement.push(' ');
        }
        self.active_composer_mut().replace_range(
            cand.replace_start,
            cand.replace_end,
            &replacement,
        );
        Ok(())
    }

    fn execute_submitted_command(
        &mut self,
        text: String,
        interrupted: &AtomicBool,
        requests: &mpsc::Sender<Request>,
        surface: &mut Surface,
    ) -> Result<bool, String> {
        let trimmed = text.trim();
        if let Some(slash) = trimmed.strip_prefix('/') {
            let mut parts = slash.split_whitespace();
            let head = parts.next().unwrap_or("");
            let rest = parts.collect::<Vec<_>>().join(" ");
            match head {
                "fullscreen" | "alt" | "altscreen" => {
                    surface.set_mode(ScreenMode::Fullscreen).map_err(io_error)?;
                    self.set_toast("Fullscreen mode enabled (Alt+F to switch)");
                    return Ok(false);
                }
                "inline" => {
                    surface.set_mode(ScreenMode::Inline).map_err(io_error)?;
                    self.transcript.discard_inline_queue();
                    self.set_toast("Inline mode enabled (Alt+F to switch)");
                    return Ok(false);
                }
                "clear" => {
                    self.transcript.clear();
                    surface.clear_screen().map_err(io_error)?;
                    return Ok(false);
                }
                "mouse" => {
                    let on = surface.toggle_mouse().map_err(io_error)?;
                    self.set_toast(if on {
                        "Mouse scroll capture: on (hold Option/Shift to select)"
                    } else {
                        "Mouse scroll capture: off (native mouse selection active)"
                    });
                    return Ok(false);
                }
                "copy" => {
                    let n = rest.parse::<usize>().unwrap_or(1).max(1);
                    if let Some(copied) = self.transcript.copy_turn(n) {
                        surface.copy_osc52(&copied).map_err(io_error)?;
                        self.set_toast(format!(
                            "Copied {} bytes from turn #{n} to clipboard",
                            copied.len()
                        ));
                    } else {
                        self.set_toast("Nothing to copy yet");
                    }
                    return Ok(false);
                }
                "images" if rest.is_empty() => {
                    self.picker = Some(complete::images_picker(&self.config, &self.cwd));
                    return Ok(false);
                }
                "backend" if rest.is_empty() => {
                    self.picker = Some(complete::backend_picker(self.status.backend));
                    return Ok(false);
                }
                "fdx" if rest.is_empty() => {
                    let links = self.fdx.link_names();
                    self.picker = Some(complete::fdx_picker(&links, self.fdx.active));
                    return Ok(false);
                }
                "fdx-close" => {
                    requests
                        .send(Request::FdxClose)
                        .map_err(|_| "debug session worker stopped".to_string())?;
                    return Ok(false);
                }
                "quit" | "exit" => return Ok(true),
                other => {
                    let known = self
                        .specs
                        .iter()
                        .any(|s| s.name == other && !s.is_tui_action);
                    if !known {
                        self.transcript.begin_command(trimmed, self.theme);
                        self.transcript.push_output(
                            &format!("unknown command '/{other}'; type / to browse commands"),
                            true,
                            self.theme,
                        );
                        return Ok(false);
                    }
                    let rewritten = if rest.is_empty() {
                        other.to_string()
                    } else {
                        format!("{other} {rest}")
                    };
                    return self.submit_or_queue(rewritten, interrupted, requests);
                }
            }
        }
        self.submit_or_queue(text, interrupted, requests)
    }

    fn submit_or_queue(
        &mut self,
        text: String,
        interrupted: &AtomicBool,
        requests: &mpsc::Sender<Request>,
    ) -> Result<bool, String> {
        if self.running.is_some() {
            self.queued_command = Some(text);
            self.set_toast("Command queued — will run when current step finishes");
            return Ok(false);
        }
        self.dispatch_command(text, interrupted, requests)?;
        Ok(false)
    }

    fn desired_inline_height(&self) -> u16 {
        let mut rows = 0u16;
        if self.running.is_some() {
            rows += 1;
        }
        if self.fdx.active {
            rows += self.fdx.drawer_height();
        }
        rows += self.overlay_height();
        let editor_rows = self.active_composer().desired_rows() as u16;
        rows += editor_rows + 2; // Rounded border top + bottom
        rows += 1; // Status bar
        rows
    }

    fn overlay_height(&self) -> u16 {
        if let Some(picker) = &self.picker {
            return (picker.items.len().min(POPUP_MAX_ROWS) as u16) + 2;
        }
        if let Some(popup) = &self.popup {
            return (popup.items.len().min(POPUP_MAX_ROWS) as u16) + 2;
        }
        0
    }

    fn draw_fullscreen(&mut self, frame: &mut ratatui::Frame) {
        let area = frame.area();
        if area.is_empty() {
            return;
        }
        let buf = frame.buffer_mut();
        let cursor = self.render_fullscreen_into(area, buf);
        if let Some((cx, cy)) = cursor {
            frame.set_cursor_position((cx, cy));
        }
    }

    fn render_fullscreen_into(&mut self, area: Rect, buf: &mut Buffer) -> Option<(u16, u16)> {
        self.last_fdx_rect = Rect::default();
        let mut y = area.y;
        let mut bottom = area.bottom();

        // 1. Top header bar (1 row + subtle 1-row divider when height >= 8).
        if bottom > y + 4 {
            self.render_header_bar(Rect::new(area.x, y, area.width, 1), buf);
            y += 1;
            if bottom > y + 6 {
                Line::styled("─".repeat(usize::from(area.width)), self.theme.dim())
                    .render(Rect::new(area.x, y, area.width, 1), buf);
                y += 1;
            }
        }

        // 2. Bottom anchored chrome: Status bar + Composer + Overlay + Activity + (narrow FDX drawer).
        let wide_split = self.fdx.active
            && self.fdx.expanded
            && area.width >= WIDE_SPLIT_MIN_WIDTH
            && bottom.saturating_sub(y) >= 10;

        let status_h = if bottom > y { 1 } else { 0 };
        bottom = bottom.saturating_sub(status_h);
        let status_rect = Rect::new(area.x, bottom, area.width, status_h);

        let editor_rows = self.active_composer().desired_rows() as u16;
        let composer_h = (editor_rows + 2).min(bottom.saturating_sub(y).max(1));
        bottom = bottom.saturating_sub(composer_h);
        let composer_rect = Rect::new(area.x, bottom, area.width, composer_h);

        let overlay_h = self.overlay_height().min(bottom.saturating_sub(y));
        bottom = bottom.saturating_sub(overlay_h);
        let overlay_rect = Rect::new(area.x, bottom, area.width, overlay_h);

        let activity_h = if self.running.is_some() && bottom > y {
            1
        } else {
            0
        };
        bottom = bottom.saturating_sub(activity_h);
        let activity_rect = Rect::new(area.x, bottom, area.width, activity_h);

        let fdx_drawer_h = if self.fdx.active && !wide_split {
            self.fdx.drawer_height().min(bottom.saturating_sub(y))
        } else {
            0
        };
        bottom = bottom.saturating_sub(fdx_drawer_h);
        let fdx_drawer_rect = Rect::new(area.x, bottom, area.width, fdx_drawer_h);

        let scroll_hint_h = if self.transcript.scroll_offset() > 0 && bottom > y {
            1
        } else {
            0
        };
        bottom = bottom.saturating_sub(scroll_hint_h);
        let scroll_hint_rect = Rect::new(area.x, bottom, area.width, scroll_hint_h);

        // 3. Middle main canvas (Transcript + optional wide-screen right FDX split).
        let canvas = Rect::new(area.x, y, area.width, bottom.saturating_sub(y));
        if !canvas.is_empty() {
            if wide_split {
                let left_w = (canvas.width * 56) / 100;
                let right_w = canvas.width.saturating_sub(left_w + 1);
                let left = Rect::new(canvas.x, canvas.y, left_w, canvas.height);
                let sep_x = canvas.x + left_w;
                let right = Rect::new(sep_x + 1, canvas.y, right_w, canvas.height);
                self.render_transcript(left, buf);
                for row in canvas.y..canvas.bottom() {
                    if let Some(cell) = buf.cell_mut((sep_x, row)) {
                        cell.set_symbol("│").set_style(self.theme.dim());
                    }
                }
                let paused = self.running.as_ref().map(|r| r.name.as_str());
                let focused = self.mode != InputMode::Command;
                self.fdx.render(right, buf, self.theme, paused, focused);
                self.last_fdx_rect = right;
            } else {
                self.render_transcript(canvas, buf);
            }
        }

        if !scroll_hint_rect.is_empty() {
            let offset = self.transcript.scroll_offset();
            Line::from(vec![
                Span::styled(format!("  ↓ {offset} lines below"), self.theme.yellow()),
                Span::styled(" · End jump to bottom", self.theme.dim()),
            ])
            .render(scroll_hint_rect, buf);
        }

        if !fdx_drawer_rect.is_empty() {
            let paused = self.running.as_ref().map(|r| r.name.as_str());
            let focused = self.mode != InputMode::Command;
            self.fdx
                .render(fdx_drawer_rect, buf, self.theme, paused, focused);
            self.last_fdx_rect = fdx_drawer_rect;
        }

        if !activity_rect.is_empty() {
            self.render_activity(activity_rect, buf);
        }
        if !overlay_rect.is_empty() {
            self.render_overlay(overlay_rect, buf);
        }
        let cursor = self.render_composer(composer_rect, buf);
        if !status_rect.is_empty() {
            self.render_status_bar(status_rect, buf, ScreenMode::Fullscreen);
        }
        cursor
    }

    fn render_inline_chrome(&mut self, buf: &mut Buffer) -> Option<(u16, u16)> {
        let area = buf.area;
        let mut y = area.y;
        let bottom = area.bottom();

        if self.fdx.active {
            let h = self.fdx.drawer_height().min(bottom.saturating_sub(y + 4));
            if h > 0 {
                let rect = Rect::new(area.x, y, area.width, h);
                let paused = self.running.as_ref().map(|r| r.name.as_str());
                let focused = self.mode != InputMode::Command;
                self.fdx.render(rect, buf, self.theme, paused, focused);
                y += h;
            }
        }

        if self.running.is_some() && y + 3 < bottom {
            self.render_activity(Rect::new(area.x, y, area.width, 1), buf);
            y += 1;
        }

        let overlay_h = self.overlay_height().min(bottom.saturating_sub(y + 3));
        if overlay_h > 0 {
            self.render_overlay(Rect::new(area.x, y, area.width, overlay_h), buf);
            y += overlay_h;
        }

        let status_h = if bottom > y + 1 { 1 } else { 0 };
        let composer_h = bottom.saturating_sub(y + status_h).max(1);
        let cursor = self.render_composer(Rect::new(area.x, y, area.width, composer_h), buf);
        if status_h > 0 {
            self.render_status_bar(
                Rect::new(area.x, y + composer_h, area.width, 1),
                buf,
                ScreenMode::Inline,
            );
        }
        cursor
    }

    fn render_header_bar(&self, area: Rect, buf: &mut Buffer) {
        let bg = self.theme.header_bar();
        for x in area.x..area.right() {
            if let Some(cell) = buf.cell_mut((x, area.y)) {
                cell.set_style(bg);
            }
        }
        let mut right = self.status.images_summary.clone();
        if let Some(port) = &self.status.port_summary {
            right.push_str(" · ");
            right.push_str(port);
        }
        right.push(' ');
        let total = usize::from(area.width);
        let right_w = right.width();
        let prefix = format!(" rvdbg {} · ", env!("CARGO_PKG_VERSION"));
        let avail_path = total.saturating_sub(prefix.width() + right_w + 2);
        let left_path = truncate_left(&short_path(&self.status.config_path), avail_path);
        let mut spans = vec![
            Span::styled(" rvdbg ", self.theme.prompt_command().patch(bg)),
            Span::styled(
                format!("{} ", env!("CARGO_PKG_VERSION")),
                self.theme.dim().patch(bg),
            ),
            Span::styled("· ", self.theme.dim().patch(bg)),
            Span::styled(left_path, self.theme.plain().patch(bg)),
        ];
        let left_w: usize = spans.iter().map(|s| s.content.width()).sum();
        if left_w + right_w < total {
            spans.push(Span::styled(" ".repeat(total - left_w - right_w), bg));
            spans.push(Span::styled(right, self.theme.dim().patch(bg)));
        }
        Line::from(spans).render(area, buf);
    }

    fn render_transcript(&mut self, area: Rect, buf: &mut Buffer) {
        let content_w = usize::from(area.width.saturating_sub(2)).max(1);
        let height = usize::from(area.height);
        let (rows, total, scroll) = self.transcript.visible_rows(content_w, height);
        for (idx, line) in rows.into_iter().enumerate() {
            line.render(
                Rect::new(
                    area.x + 1,
                    area.y + idx as u16,
                    area.width.saturating_sub(2),
                    1,
                ),
                buf,
            );
        }
        // Subtle right-edge scrollbar when transcript overflows.
        if total > height && area.width >= 4 {
            let bar_x = area.right() - 1;
            let thumb_h = ((height * height) / total).clamp(1, height);
            let max_scroll = total.saturating_sub(height).max(1);
            let from_top = max_scroll.saturating_sub(scroll);
            let thumb_y = (from_top * (height - thumb_h)) / max_scroll;
            for r in 0..height {
                let (sym, style) = if r >= thumb_y && r < thumb_y + thumb_h {
                    ("┃", self.theme.cyan())
                } else {
                    ("│", self.theme.dim())
                };
                if let Some(cell) = buf.cell_mut((bar_x, area.y + r as u16)) {
                    cell.set_symbol(sym).set_style(style);
                }
            }
        }
    }

    fn render_activity(&self, area: Rect, buf: &mut Buffer) {
        let Some(running) = &self.running else {
            return;
        };
        let spinner = SPINNER[self.spinner_frame % SPINNER.len()];
        let elapsed = format_duration(running.since.elapsed());
        let mut spans = vec![
            Span::styled(format!("  {spinner} "), self.theme.cyan()),
            Span::styled(running.name.clone(), self.theme.bold()),
        ];

        if running.cancelling {
            spans.push(Span::styled("  Interrupting…", self.theme.yellow()));
        } else if let Some(progress) = &self.progress {
            spans.push(Span::raw("  "));
            if let Some((pct, summary)) = parse_progress(progress) {
                let bar_w = 16usize;
                let filled = (pct * bar_w) / 100;
                spans.push(Span::styled(summary, self.theme.dim()));
                spans.push(Span::raw("  "));
                spans.push(Span::styled("━".repeat(filled), self.theme.cyan()));
                if filled < bar_w {
                    spans.push(Span::styled("╺", self.theme.cyan()));
                    spans.push(Span::styled(
                        "━".repeat(bar_w - filled - 1),
                        self.theme.dim(),
                    ));
                }
                spans.push(Span::styled(format!(" {pct:3}%"), self.theme.bold()));
            } else {
                spans.push(Span::styled(progress.clone(), self.theme.dim()));
            }
        } else if !self.rtt_partial.is_empty() {
            let preview = String::from_utf8_lossy(&self.rtt_partial);
            spans.push(Span::raw("  "));
            spans.push(Span::styled(preview.into_owned(), self.theme.dim()));
        }

        spans.push(Span::styled(format!("  {elapsed}"), self.theme.dim()));
        if running.cancellable && !running.cancelling {
            spans.push(Span::styled(" · Ctrl+C stop", self.theme.dim()));
        }
        Line::from(spans).render(area, buf);
    }

    fn render_overlay(&self, area: Rect, buf: &mut Buffer) {
        if let Some(picker) = &self.picker {
            let count = format!(" {} / {} ", picker.selected + 1, picker.items.len());
            let block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(self.theme.dim())
                .title(Span::styled(
                    format!(" {} ", picker.title),
                    self.theme.bold(),
                ))
                .title_bottom(
                    Line::from(Span::styled(" Enter select · Esc close ", self.theme.dim()))
                        .right_aligned(),
                )
                .title_top(Line::from(Span::styled(count, self.theme.dim())).right_aligned());
            let inner = block.inner(area);
            block.render(area, buf);
            let shown = usize::from(inner.height).min(POPUP_MAX_ROWS);
            let start = picker.selected.saturating_sub(shown.saturating_sub(1));
            for (row_idx, item) in picker.items.iter().enumerate().skip(start).take(shown) {
                let selected = row_idx == picker.selected;
                render_popup_row(
                    Rect::new(inner.x, inner.y + (row_idx - start) as u16, inner.width, 1),
                    buf,
                    self.theme,
                    selected,
                    &item.label,
                    &item.detail,
                    None,
                );
            }
            return;
        }

        if let Some(popup) = &self.popup {
            let shown_max = usize::from(area.height.saturating_sub(2)).min(POPUP_MAX_ROWS);
            let total = popup.items.len();
            let count = format!(" {} / {} ", (popup.selected + 1).min(total), total);
            let hint = if popup.is_slash {
                " Enter run/fill · Tab fill · Esc close "
            } else {
                " Tab / Enter fill · Esc close "
            };
            let block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(self.theme.dim())
                .title(Span::styled(
                    format!(" {} ", popup.title),
                    self.theme.bold(),
                ))
                .title_top(Line::from(Span::styled(count, self.theme.dim())).right_aligned())
                .title_bottom(Line::from(Span::styled(hint, self.theme.dim())).right_aligned());
            let inner = block.inner(area);
            block.render(area, buf);
            let start = popup.selected.saturating_sub(shown_max.saturating_sub(1));
            for (row_idx, cand) in popup.items.iter().enumerate().skip(start).take(shown_max) {
                let selected = row_idx == popup.selected;
                let desc = cand.unavailable.unwrap_or(&cand.detail);
                render_popup_row(
                    Rect::new(inner.x, inner.y + (row_idx - start) as u16, inner.width, 1),
                    buf,
                    self.theme,
                    selected,
                    &cand.label,
                    desc,
                    cand.unavailable,
                );
            }
        }
    }

    fn render_composer(&mut self, area: Rect, buf: &mut Buffer) -> Option<(u16, u16)> {
        if area.is_empty() {
            return None;
        }
        let is_rtt = self.rtt.is_some();
        let (title, prompt, prompt_style, border_style) = if is_rtt {
            (
                " rtt · direct console (Ctrl+C return) ".to_string(),
                "rtt › ".to_string(),
                self.theme.prompt_rtt(),
                self.theme.border_rtt(),
            )
        } else if self.mode != InputMode::Command {
            let label = match self.mode {
                InputMode::FdxAll => format!(
                    " fdx:all ({}) · @guest or :to · !cmd runs rvdbg ",
                    self.fdx.target
                ),
                InputMode::FdxSingle => format!(" fdx:{} · !cmd runs rvdbg ", self.fdx.target),
                InputMode::Command => unreachable!(),
            };
            (
                label,
                format!("{} › ", self.fdx.target),
                self.theme.prompt_fdx(),
                self.theme.border_fdx(self.running.is_none()),
            )
        } else {
            let title = if let Some(queued) = &self.queued_command {
                format!(" rvdbg · queued: {queued} ")
            } else if self.running.is_some() {
                " rvdbg · Enter queues next ".to_string()
            } else {
                " rvdbg ".to_string()
            };
            (
                title,
                "› ".to_string(),
                self.theme.prompt_command(),
                self.theme.border_command(self.running.is_none()),
            )
        };

        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border_style)
            .title(Span::styled(title, border_style));

        let inner = if area.height >= 3 && area.width >= 8 {
            let inner = block.inner(area);
            // Peek line badge before rendering block.
            let prompt_w = prompt.width();
            let text_w = usize::from(inner.width).saturating_sub(prompt_w + 1).max(1);
            let max_rows = usize::from(inner.height).max(1);
            let vis = self.active_composer_mut().visible(text_w, max_rows);
            if let Some((ln, total)) = vis.line_badge {
                block = block.title_top(
                    Line::from(Span::styled(
                        format!(" Ln {ln}/{total} · Ctrl+J newline "),
                        self.theme.dim(),
                    ))
                    .right_aligned(),
                );
            }
            block.render(area, buf);
            return self.paint_composer_lines(inner, buf, &prompt, prompt_style, vis, is_rtt);
        } else {
            area
        };

        let prompt_w = prompt.width();
        let text_w = usize::from(inner.width).saturating_sub(prompt_w + 1).max(1);
        let vis = self
            .active_composer_mut()
            .visible(text_w, usize::from(inner.height).max(1));
        self.paint_composer_lines(inner, buf, &prompt, prompt_style, vis, is_rtt)
    }

    fn paint_composer_lines(
        &self,
        inner: Rect,
        buf: &mut Buffer,
        prompt: &str,
        prompt_style: Style,
        vis: composer::VisibleComposer,
        is_rtt: bool,
    ) -> Option<(u16, u16)> {
        if inner.is_empty() {
            return None;
        }
        let prompt_w = prompt.width() as u16;
        let ghost = if !is_rtt
            && self.mode == InputMode::Command
            && self.composer.search.is_none()
            && self.composer.is_single_line()
            && self.composer.cursor_at_end()
        {
            complete::ghost_suffix(&self.composer.text, self.composer.cursor, &self.specs)
        } else {
            None
        };

        let cont_prefix = " ".repeat(usize::from(prompt_w));
        for (idx, text) in vis.lines.iter().enumerate() {
            if idx >= usize::from(inner.height) {
                break;
            }
            let row = Rect::new(
                inner.x + 1,
                inner.y + idx as u16,
                inner.width.saturating_sub(1),
                1,
            );
            let mut spans = vec![Span::styled(
                if idx == 0 { prompt } else { &cont_prefix },
                prompt_style,
            )];
            if is_rtt && idx == 0 {
                spans.push(Span::styled(
                    "Type to send bytes directly to RTT channel 0…",
                    self.theme.dim(),
                ));
            } else {
                spans.push(Span::raw(text.as_str()));
                if idx == 0
                    && let Some(suffix) = &ghost
                {
                    spans.push(Span::styled(suffix.as_str(), self.theme.dim()));
                }
            }
            Line::from(spans).render(row, buf);
        }

        if is_rtt || self.picker.is_some() {
            return None;
        }
        let cx = (inner.x + 1 + prompt_w + vis.cursor_col).min(inner.right().saturating_sub(1));
        let cy = (inner.y + vis.cursor_row).min(inner.bottom().saturating_sub(1));
        Some((cx, cy))
    }

    fn render_status_bar(&self, area: Rect, buf: &mut Buffer, screen: ScreenMode) {
        let (dot, dot_style) = self.theme.pill_style(&self.status.target);
        let mut left = vec![
            Span::raw("  "),
            Span::styled(format!("{dot} "), dot_style),
            Span::styled(self.status.target.clone(), dot_style),
        ];
        if self.last_code != 0 && self.running.is_none() {
            left.push(Span::styled(
                format!(" · exit {}", self.last_code),
                self.theme.red(),
            ));
        }
        if self.fdx.active {
            let (open, total) = self.fdx.open_counts();
            if total > 0 {
                left.push(Span::styled(
                    format!(" · FDX {open}/{total} open"),
                    if open > 0 {
                        self.theme.green()
                    } else {
                        self.theme.dim()
                    },
                ));
            } else {
                left.push(Span::styled(" · FDX", self.theme.magenta()));
            }
        } else if screen == ScreenMode::Inline {
            left.push(Span::styled(
                format!(" · {}", self.status.images_summary),
                self.theme.dim(),
            ));
        }

        let right_text = if let Some(toast) = self.active_toast() {
            toast.to_string()
        } else if self.composer.quit_pending() || self.fdx_composer.quit_pending() {
            "Press Ctrl+D again to confirm".to_string()
        } else if self.rtt.is_some() {
            "RTT · Ctrl+C return".to_string()
        } else if self.mode != InputMode::Command {
            "⇧Tab rvdbg · Ctrl+O layout · !cmd run · :q close".to_string()
        } else if self.running.is_some() {
            "Enter queue · Ctrl+C interrupt".to_string()
        } else {
            let mode_hint = match screen {
                ScreenMode::Fullscreen => "Alt+F inline",
                ScreenMode::Inline => "Alt+F full",
            };
            format!("Tab complete · / menu · ⇧Tab fdx · {mode_hint}")
        };

        let left_w: usize = left.iter().map(|s| s.content.width()).sum();
        let width = usize::from(area.width);
        let right_style = if self.active_toast().is_some() {
            self.theme.yellow()
        } else {
            self.theme.dim()
        };
        let compact_fallback = if self.active_toast().is_some()
            || self.composer.quit_pending()
            || self.fdx_composer.quit_pending()
            || self.rtt.is_some()
        {
            None
        } else if self.mode != InputMode::Command {
            Some("⇧Tab rvdbg · Ctrl+O · :q")
        } else if self.running.is_some() {
            Some("Ctrl+C stop")
        } else {
            Some("Tab · / menu · ⇧Tab")
        };
        let chosen = if left_w + right_text.width() + 3 <= width {
            Some(right_text.as_str())
        } else {
            compact_fallback.filter(|hint| left_w + hint.width() + 3 <= width)
        };
        if let Some(hint) = chosen {
            let right_w = hint.width();
            left.push(Span::raw(" ".repeat(width - left_w - right_w - 1)));
            left.push(Span::styled(hint.to_string(), right_style));
        }
        Line::from(left).render(area, buf);
    }
}

fn render_popup_row(
    row: Rect,
    buf: &mut Buffer,
    theme: Theme,
    selected: bool,
    label: &str,
    detail: &str,
    unavailable: Option<&str>,
) {
    if row.is_empty() {
        return;
    }
    let width = usize::from(row.width);
    let col_w = 18usize.min(width / 2);
    let label_pad = format!("{label:<col_w$}");
    let (marker, marker_style, name_style, desc_style, bg_style) = if selected {
        (
            " › ",
            theme.selected_marker(),
            theme.selected_row(),
            theme.selected_desc(),
            theme.selected_desc(),
        )
    } else if unavailable.is_some() {
        ("   ", theme.dim(), theme.dim(), theme.dim(), theme.plain())
    } else {
        (
            "   ",
            theme.plain(),
            theme.plain(),
            theme.dim(),
            theme.plain(),
        )
    };

    if selected {
        for x in row.x..row.right() {
            if let Some(cell) = buf.cell_mut((x, row.y)) {
                cell.set_style(bg_style);
            }
        }
    }

    let line = Line::from(vec![
        Span::styled(marker, marker_style),
        Span::styled(label_pad, name_style),
        Span::styled(" ", desc_style),
        Span::styled(detail.to_string(), desc_style),
    ]);
    Paragraph::new(line).render(row, buf);
}

fn parse_progress(raw: &str) -> Option<(usize, String)> {
    // OpenOCD format: "write   [####----------------]  20%  16.0/80.0 KiB  1.2s"
    let pct_pos = raw.find('%')?;
    let before = raw[..pct_pos].trim_end();
    let digits_start = before
        .rfind(|c: char| !c.is_ascii_digit())
        .map_or(0, |i| i + 1);
    let pct: usize = before[digits_start..].parse().ok()?;
    let phase = raw.split_whitespace().next().unwrap_or("working");
    let after_pct = raw[pct_pos + 1..].trim();
    let summary = if after_pct.is_empty() {
        phase.to_string()
    } else {
        format!("{phase} · {after_pct}")
    };
    Some((pct.min(100), summary))
}

fn short_path(path: &std::path::Path) -> String {
    if let Ok(home) = std::env::var("HOME")
        && let Ok(rel) = path.strip_prefix(&home)
    {
        return format!("~/{}", rel.display());
    }
    path.display().to_string()
}

fn truncate_left(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".repeat(max_width);
    }
    let mut kept = String::new();
    let mut width = 1; // leading '…'
    for ch in text.chars().rev() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + w > max_width {
            break;
        }
        kept.push(ch);
        width += w;
    }
    let tail: String = kept.chars().rev().collect();
    format!("…{tail}")
}

pub(crate) fn format_images_summary(config: &Config) -> String {
    let count = 1 + config.extra_elf.len();
    match &config.images_from {
        crate::config::ImagesFrom::Directory(dir) => {
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| dir.display().to_string());
            format!("{name} ({count} ELF{})", if count == 1 { "" } else { "s" })
        }
        crate::config::ImagesFrom::Environment => {
            format!("env ({count} ELF{})", if count == 1 { "" } else { "s" })
        }
        crate::config::ImagesFrom::Config => {
            let elf_name = config
                .elf
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| config.program.clone());
            if config.extra_elf.is_empty() {
                elf_name
            } else {
                format!("{elf_name} +{} guests", config.extra_elf.len())
            }
        }
    }
}

fn rtt_key(key: KeyEvent) -> Option<Vec<u8>> {
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && let KeyCode::Char(ch @ 'a'..='z') = key.code
    {
        return Some(vec![ch as u8 - b'a' + 1]);
    }
    Some(match key.code {
        KeyCode::Char(ch) => ch.encode_utf8(&mut [0; 4]).as_bytes().to_vec(),
        KeyCode::Enter => vec![b'\n'],
        KeyCode::Backspace => vec![8],
        KeyCode::Esc => vec![27],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        _ => return None,
    })
}

fn io_error(error: io::Error) -> String {
    format!("terminal: {error}")
}

fn event_loop(
    mut app: App,
    mut surface: Surface,
    receiver: Receiver<Message>,
    requests: mpsc::Sender<Request>,
    interrupted: &AtomicBool,
    terminate: &AtomicUsize,
) -> Result<u8, String> {
    let mut dirty = true;
    let mut last_tick = Instant::now();
    loop {
        let signal = terminate.load(Ordering::SeqCst);
        if signal != 0 {
            return Ok(128 + signal as u8);
        }
        for _ in 0..64 {
            match receiver.try_recv() {
                Ok(message) => {
                    dirty = true;
                    if app.handle_message(message, interrupted, &requests)? {
                        return Ok(0);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err("debug session worker stopped".into());
                }
            }
        }

        // Expire transient toast cleanly.
        if app
            .toast
            .as_ref()
            .is_some_and(|t| Instant::now() >= t.until)
        {
            app.toast = None;
            dirty = true;
        }

        // Animate spinner & elapsed timer ONLY while a command is running.
        if app.running.is_some() && last_tick.elapsed() >= SPINNER_TICK {
            app.spinner_frame = app.spinner_frame.wrapping_add(1);
            last_tick = Instant::now();
            dirty = true;
        }

        if dirty {
            match surface.mode() {
                ScreenMode::Fullscreen => {
                    app.transcript.discard_inline_queue();
                    surface
                        .draw_fullscreen(|frame| app.draw_fullscreen(frame))
                        .map_err(io_error)?;
                }
                ScreenMode::Inline => {
                    let queued = app.transcript.take_inline_queue();
                    surface.emit_inline_lines(&queued).map_err(io_error)?;
                    let h = app.desired_inline_height();
                    let color = app.theme.color;
                    surface
                        .draw_inline(h, color, |buf| app.render_inline_chrome(buf))
                        .map_err(io_error)?;
                }
            }
            dirty = false;
        }

        if !event::poll(POLL).map_err(io_error)? {
            continue;
        }
        dirty = true;
        match event::read().map_err(io_error)? {
            Event::Key(key)
                if key.kind != KeyEventKind::Release
                    && app.handle_key(key, interrupted, &requests, &mut surface)? =>
            {
                return Ok(0);
            }
            Event::Mouse(mouse) => {
                app.handle_mouse(mouse, surface.mode());
            }
            Event::Paste(text)
                if !app
                    .running
                    .as_ref()
                    .is_some_and(|running| running.cancelling) =>
            {
                if let Some(rtt_tx) = &app.rtt {
                    for chunk in text.into_bytes().chunks(4096) {
                        if rtt_tx.try_send(chunk.to_vec()).is_err() {
                            break;
                        }
                    }
                } else {
                    app.active_composer_mut().paste(&text);
                    app.refresh_slash_popup();
                }
            }
            Event::Resize(w, h) if w > 0 && h > 0 => {
                // Redraws cleanly on next iteration.
            }
            _ => {}
        }
    }
}

#[cfg(unix)]
struct SignalHandlers(Vec<signal_hook::SigId>);

#[cfg(unix)]
impl SignalHandlers {
    fn install(terminate: &Arc<AtomicUsize>) -> Result<Self, String> {
        use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};

        let mut handlers = Self(Vec::new());
        for signal in [SIGHUP, SIGINT, SIGTERM] {
            let id = signal_hook::flag::register_usize(signal, terminate.clone(), signal as usize)
                .map_err(|error| {
                    format!("cannot install terminal cleanup for signal {signal}: {error}")
                })?;
            handlers.0.push(id);
        }
        Ok(handlers)
    }
}

#[cfg(unix)]
impl Drop for SignalHandlers {
    fn drop(&mut self) {
        for id in self.0.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

pub(crate) fn run(config: Config, flag: Option<Backend>, mode: ScreenMode) -> Result<u8, String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| config.root.clone());
    let theme = Theme::detect();
    let color = theme.color;
    let app = App::new(config.clone(), cwd, theme);
    let terminate = Arc::new(AtomicUsize::new(0));
    #[cfg(unix)]
    let _signals = SignalHandlers::install(&terminate)?;
    let surface = Surface::open(mode).map_err(io_error)?;
    let (request, requests) = mpsc::channel();
    let (messages, receiver) = mpsc::sync_channel(128);
    let interrupted = Arc::new(AtomicBool::new(false));
    let cancel = interrupted.clone();
    let handle = std::thread::Builder::new()
        .name("rvdbg-session".into())
        .spawn(move || worker(config, flag, color, requests, messages, cancel))
        .map_err(|error| format!("cannot start debug session: {error}"))?;
    let result = event_loop(app, surface, receiver, request, &interrupted, &terminate);
    interrupted.store(true, Ordering::SeqCst);
    handle
        .join()
        .map_err(|_| "debug session worker panicked".to_string())?;
    result
}
