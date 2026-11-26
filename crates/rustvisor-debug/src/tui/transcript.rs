//! Structured command transcript with virtual scrolling (Fullscreen) and
//! incremental logical-line flushing (Inline).

use std::collections::VecDeque;
use std::time::Duration;

use ratatui::text::{Line, Span};

use super::ansi;
use super::logo;
use super::theme::Theme;

const MAX_TURNS: usize = 500;
const MAX_LINES_PER_TURN: usize = 4000;

struct RecordLine {
    line: Line<'static>,
    indent: usize,
}

struct Turn {
    command: Option<String>,
    banner: Option<String>,
    lines: Vec<RecordLine>,
}

pub(super) struct Transcript {
    turns: VecDeque<Turn>,
    /// Number of visual lines scrolled up from the bottom (0 = pinned to bottom).
    scroll: usize,
    /// Pending logical lines (ANSI-serialized) waiting to be flushed in Inline mode.
    inline_queue: Vec<String>,
}

impl Transcript {
    pub(super) fn new() -> Self {
        Self {
            turns: VecDeque::new(),
            scroll: 0,
            inline_queue: Vec::new(),
        }
    }

    pub(super) fn push_banner(&mut self, header: &str, theme: Theme, width: u16) {
        let parsed = logo::banner_lines(header, theme, width);
        for line in &parsed {
            self.inline_queue.push(ansi::to_ansi(line, 0, theme.color));
        }
        self.push_turn(Turn {
            command: None,
            banner: Some(header.to_string()),
            lines: Vec::new(),
        });
    }

    pub(super) fn begin_command(&mut self, command: &str, theme: Theme) {
        if !self.turns.is_empty() {
            self.inline_queue.push(String::new());
        }
        let mut turn_lines = Vec::new();
        for (idx, part) in command.lines().enumerate() {
            let prefix = if idx == 0 { "❯ " } else { "  " };
            let line = Line::from(vec![
                Span::styled(prefix, theme.prompt_command()),
                Span::styled(part.to_string(), theme.bold()),
            ]);
            self.inline_queue.push(ansi::to_ansi(&line, 0, theme.color));
            turn_lines.push(RecordLine { line, indent: 0 });
        }
        self.push_turn(Turn {
            command: Some(command.to_string()),
            banner: None,
            lines: turn_lines,
        });
        self.scroll = 0;
    }

    pub(super) fn push_output(&mut self, text: &str, is_error: bool, theme: Theme) {
        let base = if is_error {
            theme.yellow()
        } else {
            theme.plain()
        };
        let parsed = ansi::parse_lines(text, base, theme.color);
        let has_command = self.turns.back().is_some_and(|turn| turn.command.is_some());
        let indent = if has_command { 2 } else { 0 };
        if self.turns.is_empty() || self.turns.back().is_some_and(|turn| turn.banner.is_some()) {
            self.push_turn(Turn {
                command: None,
                banner: None,
                lines: Vec::new(),
            });
        }
        let turn = self.turns.back_mut().expect("turn exists");
        for line in parsed {
            self.inline_queue
                .push(ansi::to_ansi(&line, indent, theme.color));
            if turn.lines.len() < MAX_LINES_PER_TURN {
                turn.lines.push(RecordLine { line, indent });
            }
        }
    }

    pub(super) fn push_note(&mut self, text: &str, theme: Theme) {
        let line = Line::from(vec![
            Span::styled("· ", theme.dim()),
            Span::styled(text.to_string(), theme.dim()),
        ]);
        self.inline_queue.push(ansi::to_ansi(&line, 2, theme.color));
        if self.turns.is_empty() || self.turns.back().is_some_and(|turn| turn.banner.is_some()) {
            self.push_turn(Turn {
                command: None,
                banner: None,
                lines: Vec::new(),
            });
        }
        let turn = self.turns.back_mut().expect("turn exists");
        if turn.lines.len() < MAX_LINES_PER_TURN {
            turn.lines.push(RecordLine { line, indent: 2 });
        }
    }

    pub(super) fn finish_command(&mut self, code: u8, elapsed: Duration, theme: Theme) {
        let Some(turn) = self.turns.back_mut().filter(|t| t.command.is_some()) else {
            return;
        };
        if code == 0 && elapsed < Duration::from_secs(2) {
            return;
        }
        let secs = format_duration(elapsed);
        let line = if code == 0 {
            Line::from(vec![
                Span::styled("✓ ", theme.green()),
                Span::styled(format!("done · {secs}"), theme.dim()),
            ])
        } else {
            Line::from(vec![
                Span::styled("✗ ", theme.error()),
                Span::styled(format!("exit {code} · {secs}"), theme.dim()),
            ])
        };
        self.inline_queue.push(ansi::to_ansi(&line, 2, theme.color));
        turn.lines.push(RecordLine { line, indent: 2 });
    }

    pub(super) fn take_inline_queue(&mut self) -> Vec<String> {
        std::mem::take(&mut self.inline_queue)
    }

    pub(super) fn discard_inline_queue(&mut self) {
        self.inline_queue.clear();
    }

    pub(super) fn clear(&mut self) {
        self.turns.clear();
        self.scroll = 0;
        self.inline_queue.clear();
    }

    /// Return the plain text of the N-th most recent command turn (1 = latest).
    pub(super) fn copy_turn(&self, one_based_from_end: usize) -> Option<String> {
        let index = one_based_from_end.checked_sub(1)?;
        let turn = self
            .turns
            .iter()
            .rev()
            .filter(|t| t.command.is_some())
            .nth(index)?;
        let mut out = Vec::new();
        for item in &turn.lines {
            let text = ansi::plain_text(&item.line);
            if item.indent == 0 && text.starts_with("❯ ") {
                continue;
            }
            out.push(text);
        }
        Some(out.join("\n"))
    }

    pub(super) fn scroll_offset(&self) -> usize {
        self.scroll
    }

    pub(super) fn scroll_up(&mut self, rows: usize) {
        self.scroll = self.scroll.saturating_add(rows);
    }

    pub(super) fn scroll_down(&mut self, rows: usize) {
        self.scroll = self.scroll.saturating_sub(rows);
    }

    pub(super) fn scroll_to_bottom(&mut self) {
        self.scroll = 0;
    }

    pub(super) fn scroll_to_top(&mut self) {
        self.scroll = usize::MAX / 2;
    }

    /// Render wrapped visual rows for Fullscreen viewport of size `(width, height)`.
    /// Returns `(visible_rows, total_rows, clamped_scroll_from_bottom)`.
    pub(super) fn visible_rows(
        &mut self,
        width: usize,
        height: usize,
        theme: Theme,
    ) -> (Vec<Line<'static>>, usize, usize) {
        if width == 0 || height == 0 {
            return (Vec::new(), 0, 0);
        }
        let mut all = Vec::new();
        for (idx, turn) in self.turns.iter().enumerate() {
            if idx > 0 {
                all.push(Line::default());
            }
            if let Some(header) = &turn.banner {
                let banner_width = u16::try_from(width).unwrap_or(u16::MAX);
                for line in logo::banner_lines(header, theme, banner_width) {
                    all.extend(ansi::wrap_line(&line, width, 0));
                }
                continue;
            }
            for item in &turn.lines {
                all.extend(ansi::wrap_line(&item.line, width, item.indent));
            }
        }
        let total = all.len();
        let max_scroll = total.saturating_sub(height);
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
        let end = total.saturating_sub(self.scroll);
        let start = end.saturating_sub(height);
        (all[start..end].to_vec(), total, self.scroll)
    }

    fn push_turn(&mut self, turn: Turn) {
        if self.turns.len() >= MAX_TURNS {
            self.turns.pop_front();
        }
        self.turns.push_back(turn);
    }
}

pub(super) fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs_f64();
    if secs < 10.0 {
        format!("{secs:.1}s")
    } else if secs < 60.0 {
        format!("{}s", duration.as_secs())
    } else {
        let total = duration.as_secs();
        format!("{}m{:02}s", total / 60, total % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_turns_and_copies_latest_output() {
        let mut t = Transcript::new();
        t.begin_command("status", Theme::PLAIN);
        t.push_output("halted   PC 0x4000", false, Theme::PLAIN);
        t.finish_command(0, Duration::from_millis(20), Theme::PLAIN);
        assert_eq!(t.copy_turn(1).as_deref(), Some("halted   PC 0x4000"));
        let (rows, total, scroll) = t.visible_rows(80, 10, Theme::PLAIN);
        assert_eq!(scroll, 0);
        assert_eq!(total, 2);
        assert_eq!(ansi::plain_text(&rows[0]), "❯ status");
        assert_eq!(ansi::plain_text(&rows[1]), "  halted   PC 0x4000");
    }

    #[test]
    fn scrolling_clamps_to_content() {
        let mut t = Transcript::new();
        t.begin_command("mem 0x0 10", Theme::PLAIN);
        for i in 0..20 {
            t.push_output(&format!("row {i}"), false, Theme::PLAIN);
        }
        t.scroll_up(100);
        let (rows, total, scroll) = t.visible_rows(80, 5, Theme::PLAIN);
        assert_eq!(rows.len(), 5);
        assert_eq!(total, 21);
        assert_eq!(scroll, 16);
        assert_eq!(ansi::plain_text(&rows[0]), "❯ mem 0x0 10");
    }

    #[test]
    fn banner_adapts_dynamically_to_narrow_and_wide_viewports() {
        let mut t = Transcript::new();
        let header = "rvdbg 0.1.42  /very/long/path/to/rustvisor-sdk/rvdbg.toml\n\
                      backend  no probe found: connect a StellarLINK or a Lauterbach probe\n\
                      images   /very/long/path/to/target/board-tests/build/latency/inject-five";
        t.push_banner(header, Theme::PLAIN, 120);
        // Wide viewport: chip stays 7 rows, zero lines wrap across the chip.
        let (wide, total_wide, _) = t.visible_rows(72, 20, Theme::PLAIN);
        assert_eq!(total_wide, 7);
        assert_eq!(wide.len(), 7);
        // Narrow viewport (< 68): drops chip cleanly without splitting it.
        let (narrow, _, _) = t.visible_rows(55, 20, Theme::PLAIN);
        assert!(ansi::plain_text(&narrow[0]).starts_with("rvdbg"));
    }
}
