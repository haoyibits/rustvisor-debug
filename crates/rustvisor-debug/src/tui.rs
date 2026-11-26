//! The inline terminal. The UI owns stdin and stdout. A worker owns the
//! debugger session for its entire lifetime, including channel consoles.

mod editor;

use std::collections::VecDeque;
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

use crossterm::cursor::Show;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::{execute, terminal};
use probecore::output::{self, Line as Output};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::config::{Backend, Config};
use crate::session::{self, Flow, MenuCommand, Session};
use editor::{Action, Editor, MENU_ROWS};

const HEIGHT: u16 = MENU_ROWS as u16 + 3;
const POLL: Duration = Duration::from_millis(40);

pub(crate) struct Status {
    pub target: String,
    pub backend: Option<Backend>,
}

enum Message {
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
    },
    FdxStatus {
        target: String,
        status: String,
    },
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

    pub fn fdx_open(&self, target: String, path: Option<PathBuf>) -> Result<(), String> {
        self.send(Message::FdxOpen { target, path })
    }

    pub fn fdx_status(&self, target: String, status: String) {
        let _ = self.send(Message::FdxStatus { target, status });
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

struct Screen {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Screen {
    fn open() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let result = (|| {
            execute!(io::stdout(), EnableBracketedPaste)?;
            let terminal = Terminal::with_options(
                CrosstermBackend::new(io::stdout()),
                TerminalOptions {
                    viewport: Viewport::Inline(HEIGHT),
                },
            )?;
            Ok(Self { terminal })
        })();
        if result.is_err() {
            Self::restore();
        }
        result
    }

    fn restore() {
        let _ = execute!(io::stdout(), DisableBracketedPaste, Show);
        let _ = terminal::disable_raw_mode();
    }

    fn record(&mut self, text: &str, style: Style) -> io::Result<()> {
        self.terminal.autoresize()?;
        let size = self.terminal.size()?;
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        let rows = wrap(text, usize::from(size.width));
        for chunk in rows.chunks(128) {
            self.terminal.insert_before(chunk.len() as u16, |buf| {
                for (index, row) in chunk.iter().enumerate() {
                    buf.set_string(0, index as u16, row, style);
                }
            })?;
        }
        Ok(())
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = self.terminal.clear();
        Self::restore();
    }
}

fn safe_text(text: &str) -> String {
    text.chars()
        .filter_map(|ch| match ch {
            '\n' | '\t' => Some(ch),
            '\r' => None,
            '\x1b' => Some('␛'),
            ch if ch.is_control() => Some('�'),
            ch => Some(ch),
        })
        .collect()
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for line in safe_text(text).replace('\t', "    ").split('\n') {
        let mut row = String::new();
        let mut used = 0;
        for glyph in line.graphemes(true) {
            let len = glyph.width();
            if len > width {
                continue;
            }
            if used + len > width {
                rows.push(std::mem::take(&mut row));
                used = 0;
            }
            row.push_str(glyph);
            used += len;
        }
        rows.push(row);
    }
    rows
}

fn worker(
    config: Config,
    flag: Option<Backend>,
    requests: Receiver<Request>,
    messages: SyncSender<Message>,
    interrupted: Arc<AtomicBool>,
) {
    let sink = messages.clone();
    output::with_sink(
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
            let mut session = Session::new(config, flag, false, probecore::style::Style::PLAIN);
            session.use_tui(CommandUi(messages.clone()), interrupted);
            session.connect(None);
            let _ = messages.send(Message::Text(session.tui_header(), false));
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

struct FdxPane {
    target: String,
    status: String,
    editor: Editor,
    lines: VecDeque<String>,
}

struct App {
    editor: Editor,
    commands: Vec<MenuCommand>,
    target: String,
    progress: Option<String>,
    running: Option<Running>,
    rtt: Option<SyncSender<Vec<u8>>>,
    fdx: Option<FdxPane>,
    fdx_focused: bool,
    rtt_partial: Vec<u8>,
    color: bool,
}

impl App {
    fn new(path: Option<PathBuf>) -> Self {
        Self {
            editor: Editor::new(path),
            commands: Vec::new(),
            target: "Detecting probe".into(),
            progress: None,
            running: Some(Running {
                name: "Connecting".into(),
                since: Instant::now(),
                cancellable: false,
                cancelling: false,
            }),
            rtt: None,
            fdx: None,
            fdx_focused: false,
            rtt_partial: Vec::new(),
            color: probecore::style::Style::stdout().enabled(),
        }
    }

    fn accent(&self, color: Color) -> Style {
        if self.color {
            Style::default().fg(color)
        } else {
            Style::default()
        }
    }

    fn flush_rtt(&mut self, screen: &mut Screen) -> io::Result<()> {
        if !self.rtt_partial.is_empty() {
            screen.record(
                &String::from_utf8_lossy(&self.rtt_partial),
                Style::default(),
            )?;
            self.rtt_partial.clear();
        }
        Ok(())
    }

    fn message(&mut self, message: Message, screen: &mut Screen) -> io::Result<bool> {
        match message {
            Message::Text(text, error) => {
                self.flush_rtt(screen)?;
                let style = if error {
                    self.accent(Color::Yellow)
                } else {
                    Style::default()
                };
                screen.record(&text, style)?;
            }
            Message::Progress(text) => self.progress = Some(text),
            Message::RttBytes(bytes) => {
                self.rtt_partial.extend(bytes);
                if let Some(end) = self.rtt_partial.iter().rposition(|&b| b == b'\n') {
                    let text = String::from_utf8_lossy(&self.rtt_partial[..end]);
                    screen.record(&text, Style::default())?;
                    self.rtt_partial.drain(..=end);
                }
                if self.rtt_partial.len() >= 16 * 1024 {
                    self.flush_rtt(screen)?;
                }
            }
            Message::Started { name, cancellable } => {
                self.flush_rtt(screen)?;
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
            Message::RttInput(input) => self.rtt = Some(input),
            Message::FdxOpen { target, path } => {
                self.fdx = Some(FdxPane {
                    target,
                    status: "Checking links".into(),
                    editor: Editor::new(path),
                    lines: VecDeque::new(),
                });
                self.fdx_focused = true;
            }
            Message::FdxStatus { target, status } => {
                if let Some(pane) = &mut self.fdx {
                    pane.target = target;
                    pane.status = status;
                }
            }
            Message::FdxLine(line) => self.record_fdx(&line, screen)?,
            Message::FdxBoundary(line) => {
                if let Some(pane) = &mut self.fdx {
                    pane.editor.clear();
                }
                self.record_fdx(&line, screen)?;
            }
            Message::FdxClosed => {
                self.fdx = None;
                self.fdx_focused = false;
            }
            Message::Finished { status, code, flow } => {
                self.flush_rtt(screen)?;
                self.commands = session::menu_commands(status.backend);
                self.target = if code == 0 {
                    status.target
                } else {
                    format!("{} · exit {code}", status.target)
                };
                self.running = None;
                self.progress = None;
                self.rtt = None;
                return Ok(flow == Flow::Quit);
            }
        }
        Ok(false)
    }

    fn record_fdx(&mut self, text: &str, screen: &mut Screen) -> io::Result<()> {
        if let Some(pane) = &mut self.fdx {
            let text = safe_text(text).replace('\t', "    ");
            for line in text.lines() {
                pane.lines.push_back(line.into());
                if pane.lines.len() > 24 {
                    pane.lines.pop_front();
                }
            }
            screen.record(&format!("[FDX] {text}"), self.accent(Color::Cyan))?;
        }
        Ok(())
    }

    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        if area.is_empty() {
            return;
        }

        let dim = self.accent(Color::DarkGray);
        let accent = self.accent(Color::Cyan);
        let mut body = area;
        if body.height > 1 {
            let footer = Rect::new(area.x, area.bottom() - 1, area.width, 1);
            let hint = if self.rtt.is_some() {
                "RTT · Ctrl+C return"
            } else if self.fdx.is_some() {
                "F6 switch focus"
            } else if self.running.is_none() {
                "/ commands · ↑↓ history"
            } else {
                ""
            };
            let text = if hint.is_empty() {
                self.target.clone()
            } else {
                format!("{}   {}", self.target, hint)
            };
            frame.render_widget(Line::styled(text, dim), footer);
            body.height -= 1;
        }
        if let Some(pane) = &self.fdx {
            let (command, channel) = if body.width >= 88 {
                let left = body.width / 2;
                (
                    Rect::new(body.x, body.y, left - 1, body.height),
                    Rect::new(body.x + left, body.y, body.width - left, body.height),
                )
            } else if body.height >= 5 {
                let top = body.height / 2;
                (
                    Rect::new(body.x, body.y + top, body.width, body.height - top),
                    Rect::new(body.x, body.y, body.width, top),
                )
            } else if self.fdx_focused {
                (Rect::default(), body)
            } else {
                (body, Rect::default())
            };
            self.draw_command(frame, command, accent, dim, !self.fdx_focused);
            self.draw_fdx(frame, channel, pane, accent, dim);
        } else {
            self.draw_command(frame, body, accent, dim, true);
        }
    }

    fn draw_fdx(&self, frame: &mut Frame, area: Rect, pane: &FdxPane, accent: Style, dim: Style) {
        if area.is_empty() {
            return;
        }
        let status = if let Some(running) = &self.running {
            format!("FDX · Paused · {}", running.name)
        } else {
            format!("FDX · {}", pane.status)
        };
        let focus_style = if self.fdx_focused {
            accent.add_modifier(Modifier::BOLD)
        } else {
            dim
        };
        frame.render_widget(
            Line::styled(status, focus_style),
            Rect::new(area.x, area.y, area.width, 1),
        );
        let log_rows = usize::from(area.height.saturating_sub(2));
        for (index, line) in pane
            .lines
            .iter()
            .skip(pane.lines.len().saturating_sub(log_rows))
            .enumerate()
        {
            frame.render_widget(
                Line::raw(line.as_str()),
                Rect::new(area.x, area.y + 1 + index as u16, area.width, 1),
            );
        }
        if area.height > 1 {
            Self::draw_editor(
                frame,
                Rect::new(area.x, area.bottom() - 1, area.width, 1),
                &format!("{}> ", pane.target),
                &pane.editor,
                focus_style,
                self.fdx_focused && self.running.is_none(),
            );
        }
    }

    fn draw_command(
        &self,
        frame: &mut Frame,
        body: Rect,
        accent: Style,
        dim: Style,
        focused: bool,
    ) {
        if body.is_empty() {
            return;
        }
        if let Some(running) = &self.running {
            let activity = if running.cancelling {
                "Interrupting…".to_string()
            } else if running.cancellable {
                format!(
                    "{}  {}s · Ctrl+C stop",
                    running.name,
                    running.since.elapsed().as_secs()
                )
            } else {
                format!("{}  {}s", running.name, running.since.elapsed().as_secs())
            };
            frame.render_widget(Line::styled(activity, accent), body);
            if body.height > 1 {
                let progress = if self.rtt_partial.is_empty() {
                    self.progress.as_deref().unwrap_or_default().into()
                } else {
                    safe_text(&String::from_utf8_lossy(&self.rtt_partial))
                };
                if progress.is_empty() && self.rtt.is_none() && self.fdx.is_some() {
                    Self::draw_editor(
                        frame,
                        Rect::new(body.x, body.y + 1, body.width, 1),
                        "› ",
                        &self.editor,
                        dim,
                        focused && !running.cancelling,
                    );
                } else {
                    frame.render_widget(
                        Line::raw(progress),
                        Rect::new(body.x, body.y + 1, body.width, 1),
                    );
                }
            }
        } else {
            Self::draw_editor(frame, body, "› ", &self.editor, accent, focused);
            let matches = self.editor.matches(&self.commands);
            let shown = usize::from(body.height.saturating_sub(1)).min(MENU_ROWS);
            let start = self.editor.selected.saturating_sub(shown.saturating_sub(1));
            for (index, entry) in matches.iter().enumerate().skip(start).take(shown) {
                let selected = index == self.editor.selected;
                let style = if selected {
                    accent.add_modifier(Modifier::REVERSED)
                } else if entry.unavailable.is_none() {
                    Style::default()
                } else {
                    dim
                };
                let cursor = if selected { '›' } else { ' ' };
                let help = entry.unavailable.unwrap_or(&entry.help);
                let line = format!("{cursor} {:<12} {help}", entry.name);
                let row = Rect::new(body.x, body.y + 1 + (index - start) as u16, body.width, 1);
                frame.render_widget(Paragraph::new(line).style(style), row);
            }
        }
    }

    fn draw_editor(
        frame: &mut Frame,
        area: Rect,
        prompt: &str,
        editor: &Editor,
        accent: Style,
        focused: bool,
    ) {
        let prompt = if prompt.width() + 1 < usize::from(area.width) {
            prompt
        } else {
            ""
        };
        let columns = usize::from(area.width) - prompt.width();
        let (text, cursor) = editor.visible(columns);
        let row = Rect::new(area.x, area.y, area.width, 1);
        frame.render_widget(
            Line::from(vec![
                ratatui::text::Span::styled(prompt, accent),
                ratatui::text::Span::raw(text),
            ]),
            row,
        );
        if editor.text.contains('\n') && area.height > 1 {
            let line = format!("{} lines · Enter submit", editor.text.split('\n').count());
            frame.render_widget(
                Line::styled(line, accent),
                Rect::new(area.x, area.y + 1, area.width, 1),
            );
        }
        if focused {
            frame.set_cursor_position((area.x + prompt.width() as u16 + cursor, area.y));
        }
    }

    fn key(
        &mut self,
        key: KeyEvent,
        interrupted: &AtomicBool,
        requests: &mpsc::Sender<Request>,
        screen: &mut Screen,
    ) -> Result<bool, String> {
        if key.code == KeyCode::F(6) && self.fdx.is_some() && self.rtt.is_none() {
            self.fdx_focused = !self.fdx_focused;
            return Ok(false);
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(running) = &mut self.running {
            if running.cancellable && control && key.code == KeyCode::Char('c') {
                interrupted.store(true, Ordering::SeqCst);
                running.cancelling = true;
            }
            if !running.cancelling {
                if let Some(input) = &self.rtt {
                    if let Some(bytes) = rtt_key(key) {
                        send_console(input, bytes, screen)?;
                    }
                } else if !self.fdx_focused && key.code != KeyCode::Enter {
                    let _ = self.editor.key(key, &self.commands);
                }
            }
            return Ok(false);
        }

        if self.fdx_focused
            && let Some(pane) = &mut self.fdx
        {
            let request = if control && key.code == KeyCode::Char('c') {
                Some(Request::FdxClose)
            } else {
                match pane.editor.key(key, &[]) {
                    Action::Submit(text) => Some(Request::FdxInput(text)),
                    Action::Quit => Some(Request::FdxClose),
                    Action::Edit => None,
                }
            };
            if let Some(request) = request {
                requests
                    .send(request)
                    .map_err(|_| "debug session worker stopped".to_string())?;
            }
            return Ok(false);
        }

        match self.editor.key(key, &self.commands) {
            Action::Quit => return Ok(true),
            Action::Edit => {}
            Action::Submit(text) => {
                interrupted.store(false, Ordering::SeqCst);
                screen
                    .record(&format!("› {text}"), self.accent(Color::Cyan))
                    .map_err(error)?;
                requests
                    .send(Request::Command(text))
                    .map_err(|_| "debug session worker stopped".to_string())?;
                self.running = Some(Running {
                    name: "Executing".into(),
                    since: Instant::now(),
                    cancellable: true,
                    cancelling: false,
                });
            }
        }
        Ok(false)
    }
}

fn send_console<T>(input: &SyncSender<T>, value: T, screen: &mut Screen) -> Result<bool, String> {
    match input.try_send(value) {
        Ok(()) => Ok(true),
        Err(mpsc::TrySendError::Full(_)) => {
            screen
                .record("Console input buffer is full", Style::default())
                .map_err(error)?;
            Ok(false)
        }
        Err(mpsc::TrySendError::Disconnected(_)) => Ok(false),
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

fn error(error: io::Error) -> String {
    format!("terminal: {error}")
}

fn event_loop(
    mut app: App,
    mut screen: Screen,
    receiver: Receiver<Message>,
    requests: mpsc::Sender<Request>,
    interrupted: &AtomicBool,
    terminate: &AtomicUsize,
) -> Result<u8, String> {
    let mut dirty = true;
    let mut painted_elapsed = None;
    loop {
        let signal = terminate.load(Ordering::SeqCst);
        if signal != 0 {
            return Ok(128 + signal as u8);
        }
        for _ in 0..32 {
            match receiver.try_recv() {
                Ok(message) => {
                    dirty = true;
                    if app.message(message, &mut screen).map_err(error)? {
                        return Ok(0);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err("debug session worker stopped".into());
                }
            }
        }
        let elapsed = app
            .running
            .as_ref()
            .map(|command| command.since.elapsed().as_secs());
        if dirty || elapsed != painted_elapsed {
            screen
                .terminal
                .draw(|frame| app.draw(frame))
                .map_err(error)?;
            dirty = false;
            painted_elapsed = elapsed;
        }
        if !event::poll(POLL).map_err(error)? {
            continue;
        }
        dirty = true;
        match event::read().map_err(error)? {
            Event::Key(key)
                if key.kind != KeyEventKind::Release
                    && app.key(key, interrupted, &requests, &mut screen)? =>
            {
                return Ok(0);
            }
            Event::Paste(text)
                if !app
                    .running
                    .as_ref()
                    .is_some_and(|running| running.cancelling) =>
            {
                if let Some(input) = &app.rtt {
                    for chunk in text.into_bytes().chunks(4096) {
                        if !send_console(input, chunk.to_vec(), &mut screen)? {
                            break;
                        }
                    }
                } else if app.fdx_focused {
                    if app.running.is_none()
                        && let Some(pane) = &mut app.fdx
                    {
                        pane.editor.paste(&text);
                    }
                } else {
                    app.editor.paste(&text);
                }
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

pub(crate) fn run(config: Config, flag: Option<Backend>) -> Result<u8, String> {
    let app = App::new(session::history_path(&config));
    let terminate = Arc::new(AtomicUsize::new(0));
    #[cfg(unix)]
    let _signals = SignalHandlers::install(&terminate)?;
    let screen = Screen::open().map_err(error)?;
    let (request, requests) = mpsc::channel();
    let (messages, receiver) = mpsc::sync_channel(64);
    let interrupted = Arc::new(AtomicBool::new(false));
    let cancel = interrupted.clone();
    let handle = std::thread::Builder::new()
        .name("rvdbg-session".into())
        .spawn(move || worker(config, flag, requests, messages, cancel))
        .map_err(|error| format!("cannot start debug session: {error}"))?;
    let result = event_loop(app, screen, receiver, request, &interrupted, &terminate);
    interrupted.store(true, Ordering::SeqCst);
    handle
        .join()
        .map_err(|_| "debug session worker panicked".to_string())?;
    result
}
