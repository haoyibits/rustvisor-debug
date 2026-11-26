//! FDX multi-link monitor state and rendering:
//! - Wide Fullscreen (`width >= 100` & expanded): right-hand split pane.
//! - Narrow Fullscreen / Inline: collapsible preview bar (`Ctrl+O` toggles drawer).
//! - Unified Composer mode cycling via `Shift+Tab` (`Command` -> `FdxSingle` -> `FdxAll`).

use std::collections::VecDeque;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use super::ansi;
use super::theme::Theme;

const MAX_FDX_LINES: usize = 1000;
pub(super) const WIDE_SPLIT_MIN_WIDTH: u16 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InputMode {
    Command,
    FdxSingle,
    FdxAll,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LinkSummary {
    pub name: String,
    pub open: bool,
}

pub(super) struct FdxView {
    pub active: bool,
    pub target: String,
    pub status: String,
    pub links: Vec<LinkSummary>,
    pub target_words: Vec<String>,
    pub expanded: bool,
    pub unread: usize,
    lines: VecDeque<Line<'static>>,
    scroll: usize,
}

impl FdxView {
    pub(super) fn new() -> Self {
        Self {
            active: false,
            target: "el2".into(),
            status: String::new(),
            links: Vec::new(),
            target_words: Vec::new(),
            expanded: true,
            unread: 0,
            lines: VecDeque::new(),
            scroll: 0,
        }
    }

    pub(super) fn open(&mut self, target: String) {
        self.active = true;
        self.target = target;
        self.status = "Checking links".into();
        self.expanded = true;
        self.unread = 0;
        self.scroll = 0;
    }

    pub(super) fn close(&mut self) {
        self.active = false;
        self.links.clear();
        self.target_words.clear();
        self.status.clear();
        self.unread = 0;
        self.scroll = 0;
    }

    pub(super) fn update_status(&mut self, target: String, status: String) {
        self.target = target;
        self.links = parse_link_summaries(&status);
        self.status = status;
    }

    pub(super) fn set_words(&mut self, words: Vec<String>) {
        self.target_words = words;
    }

    pub(super) fn push_line(&mut self, raw: &str, theme: Theme, is_boundary: bool, focused: bool) {
        let base = if is_boundary {
            theme.dim()
        } else {
            theme.plain()
        };
        for line in ansi::parse_lines(raw, base, theme.color) {
            let styled = if is_boundary {
                line
            } else {
                style_fdx_line(&ansi::plain_text(&line), theme)
            };
            if self.lines.len() >= MAX_FDX_LINES {
                self.lines.pop_front();
            }
            self.lines.push_back(styled);
            if !focused && !self.expanded {
                self.unread = self.unread.saturating_add(1);
            }
        }
        if self.scroll > 0 {
            self.scroll = self.scroll.saturating_add(1);
        }
    }

    pub(super) fn toggle_expanded(&mut self) {
        self.expanded = !self.expanded;
        if self.expanded {
            self.unread = 0;
        }
    }

    pub(super) fn mark_seen(&mut self) {
        self.unread = 0;
    }

    pub(super) fn link_names(&self) -> Vec<String> {
        if self.links.is_empty() {
            vec![self.target.clone()]
        } else {
            self.links.iter().map(|l| l.name.clone()).collect()
        }
    }

    pub(super) fn open_counts(&self) -> (usize, usize) {
        let total = self.links.len();
        let open = self.links.iter().filter(|l| l.open).count();
        (open, total)
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

    /// Height required when rendered as a horizontal drawer above the Composer.
    pub(super) fn drawer_height(&self) -> u16 {
        if !self.active {
            return 0;
        }
        if self.expanded { 10 } else { 3 }
    }

    /// Render the FDX panel inside `area` (used for both wide right-split pane and drawer).
    pub(super) fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        theme: Theme,
        paused_by: Option<&str>,
        focused: bool,
    ) {
        if area.is_empty() || !self.active {
            return;
        }
        let width = usize::from(area.width);
        // Header line: link state indicators + toggle hint.
        let mut header = Vec::new();
        header.push(Span::styled(
            " FDX ",
            if focused {
                theme.prompt_fdx()
            } else {
                theme.bold()
            },
        ));
        if let Some(cmd) = paused_by {
            header.push(Span::styled(format!("· Paused ({cmd}) "), theme.yellow()));
        } else if !self.links.is_empty() {
            for link in &self.links {
                let (dot, dot_style) = if link.open {
                    ("● ", theme.green())
                } else {
                    ("○ ", theme.dim())
                };
                let name_style = if link.name == self.target && focused {
                    theme.prompt_fdx()
                } else if link.open {
                    theme.plain()
                } else {
                    theme.dim()
                };
                header.push(Span::styled(dot, dot_style));
                header.push(Span::styled(format!("{}  ", link.name), name_style));
            }
        } else {
            header.push(Span::styled(self.status.clone(), theme.dim()));
        }
        let toggle_hint = if self.expanded {
            "Ctrl+O collapse".to_string()
        } else if self.unread > 0 {
            format!("Ctrl+O expand (+{})", self.unread)
        } else {
            "Ctrl+O expand".to_string()
        };
        let used: usize = header.iter().map(|s| s.content.len()).sum();
        if used + toggle_hint.len() + 2 <= width {
            let pad = width - used - toggle_hint.len() - 1;
            header.push(Span::raw(" ".repeat(pad)));
            header.push(Span::styled(toggle_hint, theme.dim()));
        }
        Line::from(header).render(Rect::new(area.x, area.y, area.width, 1), buf);

        if area.height == 1 {
            return;
        }

        // Subtle divider when tall enough.
        let (log_y, log_height) = if area.height >= 4 {
            let rule = "─".repeat(width);
            Line::styled(rule, theme.dim())
                .render(Rect::new(area.x, area.y + 1, area.width, 1), buf);
            (area.y + 2, usize::from(area.height - 2))
        } else {
            (area.y + 1, usize::from(area.height - 1))
        };

        if log_height == 0 {
            return;
        }

        let mut wrapped = Vec::new();
        for line in &self.lines {
            wrapped.extend(ansi::wrap_line(line, width.saturating_sub(1), 1));
        }
        let total = wrapped.len();
        let max_scroll = total.saturating_sub(log_height);
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
        let end = total.saturating_sub(self.scroll);
        let start = end.saturating_sub(log_height);
        for (row, line) in wrapped[start..end].iter().enumerate() {
            line.clone()
                .render(Rect::new(area.x, log_y + row as u16, area.width, 1), buf);
        }
    }
}

fn parse_link_summaries(status: &str) -> Vec<LinkSummary> {
    if status.starts_with("Waiting")
        || status.starts_with("Unavailable")
        || status.starts_with("Checking")
    {
        return Vec::new();
    }
    status
        .split(" · ")
        .filter_map(|part| {
            let (name, state) = part.rsplit_once(' ')?;
            match state {
                "open" => Some(LinkSummary {
                    name: name.to_string(),
                    open: true,
                }),
                "waiting" => Some(LinkSummary {
                    name: name.to_string(),
                    open: false,
                }),
                _ => None,
            }
        })
        .collect()
}

fn style_fdx_line(text: &str, theme: Theme) -> Line<'static> {
    if let Some(rest) = text.strip_prefix('<') {
        return Line::from(vec![
            Span::styled("<", theme.green()),
            Span::raw(rest.to_string()),
        ]);
    }
    if let Some(after_open) = text.strip_prefix('[')
        && let Some((name, rest)) = after_open.split_once("] <")
    {
        return Line::from(vec![
            Span::styled(format!("[{name}] "), theme.dim()),
            Span::styled("<", theme.green()),
            Span::raw(rest.to_string()),
        ]);
    }
    if let Some((who, payload)) = text.split_once("> ")
        && !who.contains(char::is_whitespace)
    {
        return Line::from(vec![
            Span::styled(format!("{who}> "), theme.prompt_fdx()),
            Span::styled(payload.to_string(), theme.bold()),
        ]);
    }
    if text.contains("waiting for") || text.contains("not sent") || text.contains("not read") {
        return Line::styled(text.to_string(), theme.yellow());
    }
    Line::styled(text.to_string(), theme.dim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multi_link_status_pills() {
        let parsed = parse_link_summaries("el2 open · guest0 open · guest1 waiting");
        assert_eq!(
            parsed,
            vec![
                LinkSummary {
                    name: "el2".into(),
                    open: true
                },
                LinkSummary {
                    name: "guest0".into(),
                    open: true
                },
                LinkSummary {
                    name: "guest1".into(),
                    open: false
                },
            ]
        );
    }
}
