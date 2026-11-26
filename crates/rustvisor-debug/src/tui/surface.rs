//! Dual-mode terminal rendering backend:
//! - `ScreenMode::Fullscreen` (default): flicker-free alternate screen with
//!   optional mouse wheel capture and full terminal viewport.
//! - `ScreenMode::Inline`: zero-CPR dynamic-height synchronized buffer drawn
//!   at the bottom of native terminal scrollback.
//!
//! Both modes can hot-switch at runtime via `set_mode` without losing the
//! in-memory transcript, and `Drop` guarantees clean terminal restoration.

use std::io::{self, Stdout, Write};

use crossterm::cursor::{Hide, MoveToColumn, MoveUp, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::{Frame, Terminal};

use super::ansi;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScreenMode {
    Fullscreen,
    Inline,
}

struct InlineState {
    painted_height: u16,
    cursor_row: u16,
}

impl InlineState {
    fn new() -> Self {
        Self {
            painted_height: 0,
            cursor_row: 0,
        }
    }
}

pub(super) struct Surface {
    mode: ScreenMode,
    mouse_enabled: bool,
    mouse_active: bool,
    alt_active: bool,
    terminal: Option<Terminal<CrosstermBackend<Stdout>>>,
    inline: InlineState,
}

impl Surface {
    pub(super) fn open(mode: ScreenMode) -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut surface = Self {
            mode,
            mouse_enabled: true,
            mouse_active: false,
            alt_active: false,
            terminal: None,
            inline: InlineState::new(),
        };
        let result = (|| -> io::Result<()> {
            execute!(io::stdout(), EnableBracketedPaste)?;
            match mode {
                ScreenMode::Fullscreen => surface.enter_fullscreen()?,
                ScreenMode::Inline => {}
            }
            Ok(())
        })();
        if let Err(error) = result {
            drop(surface);
            return Err(error);
        }
        Ok(surface)
    }

    pub(super) fn mode(&self) -> ScreenMode {
        self.mode
    }

    pub(super) fn toggle_mouse(&mut self) -> io::Result<bool> {
        self.mouse_enabled = !self.mouse_enabled;
        if self.mode == ScreenMode::Fullscreen {
            if self.mouse_enabled && !self.mouse_active {
                execute!(io::stdout(), EnableMouseCapture)?;
                self.mouse_active = true;
            } else if !self.mouse_enabled && self.mouse_active {
                execute!(io::stdout(), DisableMouseCapture)?;
                self.mouse_active = false;
            }
        }
        Ok(self.mouse_enabled)
    }

    pub(super) fn set_mode(&mut self, next: ScreenMode) -> io::Result<()> {
        if self.mode == next {
            return Ok(());
        }
        match next {
            ScreenMode::Fullscreen => {
                self.clear_inline()?;
                self.enter_fullscreen()?;
            }
            ScreenMode::Inline => {
                self.leave_fullscreen()?;
                self.inline = InlineState::new();
            }
        }
        self.mode = next;
        Ok(())
    }

    pub(super) fn clear_screen(&mut self) -> io::Result<()> {
        match self.mode {
            ScreenMode::Fullscreen => {
                if let Some(terminal) = &mut self.terminal {
                    terminal.clear()?;
                }
            }
            ScreenMode::Inline => {
                let mut out = io::stdout();
                execute!(out, Clear(ClearType::All), crossterm::cursor::MoveTo(0, 0))?;
                self.inline = InlineState::new();
            }
        }
        Ok(())
    }

    /// Copy text to the host clipboard via OSC 52.
    pub(super) fn copy_osc52(&mut self, text: &str) -> io::Result<()> {
        let encoded = base64_encode(text.as_bytes());
        let mut out = io::stdout();
        write!(out, "\x1b]52;c;{encoded}\x07")?;
        out.flush()
    }

    pub(super) fn draw_fullscreen(&mut self, render: impl FnOnce(&mut Frame)) -> io::Result<()> {
        if let Some(terminal) = &mut self.terminal {
            terminal.draw(render)?;
        }
        Ok(())
    }

    /// Emit committed logical lines above the active Inline surface so the
    /// terminal emulator owns soft-wrapping, reflow, and native copy.
    pub(super) fn emit_inline_lines(&mut self, lines: &[String]) -> io::Result<()> {
        if self.mode != ScreenMode::Inline || lines.is_empty() {
            return Ok(());
        }
        let mut out = io::stdout();
        write!(out, "\x1b[?2026h")?;
        queue!(out, Hide)?;
        self.move_to_inline_top(&mut out)?;
        queue!(out, Clear(ClearType::FromCursorDown))?;
        for line in lines {
            write!(out, "{line}\r\n")?;
        }
        write!(out, "\x1b[?2026l")?;
        out.flush()?;
        self.inline.painted_height = 0;
        self.inline.cursor_row = 0;
        Ok(())
    }

    /// Render the dynamic-height Inline active area into a Ratatui buffer and
    /// flush it atomically with Synchronized Output (`?2026h`).
    pub(super) fn draw_inline(
        &mut self,
        desired_height: u16,
        color: bool,
        render: impl FnOnce(&mut Buffer) -> Option<(u16, u16)>,
    ) -> io::Result<()> {
        if self.mode != ScreenMode::Inline {
            return Ok(());
        }
        let (cols, rows) = terminal::size()?;
        if cols == 0 || rows == 0 {
            return Ok(());
        }
        let height = desired_height.max(1).min(rows.saturating_sub(1).max(1));
        let area = Rect::new(0, 0, cols, height);
        let mut buf = Buffer::empty(area);
        let cursor = render(&mut buf);

        let mut out = io::stdout();
        let mut frame = String::with_capacity(usize::from(cols) * usize::from(height) + 64);
        frame.push_str("\x1b[?2026h");
        write!(out, "{frame}")?;
        frame.clear();
        queue!(out, Hide)?;
        self.move_to_inline_top(&mut out)?;

        for y in 0..height {
            let mut active = Style::default();
            let mut x = 0u16;
            while x < cols {
                let cell = &buf[(x, y)];
                let style = if color {
                    cell.style()
                } else {
                    Style::default()
                };
                if style != active {
                    ansi::write_sgr(&mut frame, style);
                    active = style;
                }
                let sym = cell.symbol();
                if sym.is_empty() {
                    frame.push(' ');
                } else {
                    frame.push_str(sym);
                }
                let width = unicode_width::UnicodeWidthStr::width(sym).max(1) as u16;
                x = x.saturating_add(width);
            }
            if active != Style::default() {
                frame.push_str("\x1b[0m");
            }
            frame.push_str("\x1b[K");
            if y + 1 < height {
                frame.push_str("\r\n");
            }
        }
        write!(out, "{frame}")?;

        if self.inline.painted_height > height {
            queue!(out, Clear(ClearType::FromCursorDown))?;
        }

        let end_row = height - 1;
        if let Some((cx, cy)) = cursor {
            let cy = cy.min(end_row);
            let cx = cx.min(cols.saturating_sub(1));
            if end_row > cy {
                queue!(out, MoveUp(end_row - cy))?;
            }
            queue!(out, MoveToColumn(cx), Show)?;
            self.inline.cursor_row = cy;
        } else {
            self.inline.cursor_row = end_row;
        }
        write!(out, "\x1b[?2026l")?;
        out.flush()?;
        self.inline.painted_height = height;
        Ok(())
    }

    fn enter_fullscreen(&mut self) -> io::Result<()> {
        let mut out = io::stdout();
        execute!(out, EnterAlternateScreen)?;
        self.alt_active = true;
        if self.mouse_enabled {
            execute!(out, EnableMouseCapture)?;
            self.mouse_active = true;
        }
        let mut term = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        term.clear()?;
        self.terminal = Some(term);
        Ok(())
    }

    fn leave_fullscreen(&mut self) -> io::Result<()> {
        self.terminal = None;
        let mut out = io::stdout();
        if self.mouse_active {
            let _ = execute!(out, DisableMouseCapture);
            self.mouse_active = false;
        }
        if self.alt_active {
            execute!(out, LeaveAlternateScreen, Show)?;
            self.alt_active = false;
        }
        Ok(())
    }

    fn move_to_inline_top(&self, out: &mut Stdout) -> io::Result<()> {
        if self.inline.cursor_row > 0 {
            queue!(out, MoveUp(self.inline.cursor_row))?;
        }
        queue!(out, MoveToColumn(0))
    }

    fn clear_inline(&mut self) -> io::Result<()> {
        if self.inline.painted_height == 0 {
            return Ok(());
        }
        let mut out = io::stdout();
        self.move_to_inline_top(&mut out)?;
        execute!(out, Clear(ClearType::FromCursorDown), Show)?;
        self.inline = InlineState::new();
        Ok(())
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        if self.mode == ScreenMode::Inline {
            let _ = self.clear_inline();
        }
        let _ = self.leave_fullscreen();
        let _ = execute!(io::stdout(), DisableBracketedPaste, Show);
        let _ = terminal::disable_raw_mode();
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_base64_for_osc52() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"0x28010020"), "MHgyODAxMDAyMA==");
    }
}
