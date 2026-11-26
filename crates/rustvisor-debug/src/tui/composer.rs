//! Multiline composer with cross-line cursor movement, word editing,
//! inline `Ctrl+R` reverse history search, double-`Ctrl+D` quit confirmation,
//! and `.rvdbg/history` persistence.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rustyline::history::{DefaultHistory, History, SearchDirection};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) const MAX_VISIBLE_LINES: usize = 8;
const CONFIRM_QUIT_WINDOW: Duration = Duration::from_millis(1500);

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ComposerAction {
    None,
    Changed,
    Submit(String),
    ConfirmQuit,
    Quit,
    Cancel,
}

#[derive(Debug, Clone)]
pub(super) struct VisibleComposer {
    pub lines: Vec<String>,
    /// Visual cursor column (display width) within `lines[cursor_row]`.
    pub cursor_col: u16,
    /// Visual cursor row within `lines`.
    pub cursor_row: u16,
    /// `(current_line_1based, total_lines)` when text has > 1 line.
    pub line_badge: Option<(usize, usize)>,
}

pub(super) struct ReverseSearch {
    pub query: String,
    /// Match index from newest (0 = most recent matching history entry).
    pub occurrence: usize,
    pub matched: Option<String>,
    saved_draft: String,
    saved_cursor: usize,
}

pub(super) struct Composer {
    pub text: String,
    pub cursor: usize,
    history: DefaultHistory,
    path: Option<PathBuf>,
    history_index: Option<usize>,
    saved_draft: String,
    scroll_row: usize,
    confirm_quit_until: Option<Instant>,
    pub search: Option<ReverseSearch>,
}

impl Composer {
    pub(super) fn new(path: Option<PathBuf>) -> Self {
        let mut history = DefaultHistory::new();
        if let Some(path) = &path {
            let _ = history.load(path);
        }
        Self {
            text: String::new(),
            cursor: 0,
            history,
            path,
            history_index: None,
            saved_draft: String::new(),
            scroll_row: 0,
            confirm_quit_until: None,
            search: None,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.history_index = None;
        self.saved_draft.clear();
        self.scroll_row = 0;
        self.confirm_quit_until = None;
        self.search = None;
    }

    pub(super) fn set_text(&mut self, text: String) {
        self.cursor = text.len();
        self.text = text;
        self.confirm_quit_until = None;
    }

    pub(super) fn replace_range(&mut self, start: usize, end: usize, replacement: &str) {
        let start = start.min(self.text.len());
        let end = end.min(self.text.len()).max(start);
        self.text.replace_range(start..end, replacement);
        self.cursor = self.snap_grapheme(start + replacement.len());
        self.confirm_quit_until = None;
    }

    pub(super) fn paste(&mut self, pasted: &str) {
        self.confirm_quit_until = None;
        let normalized = pasted.replace("\r\n", "\n").replace('\r', "\n");
        let clean: String = normalized
            .chars()
            .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
            .collect();
        if let Some(search) = &mut self.search {
            let first = clean.lines().next().unwrap_or("");
            search.query.push_str(first);
            search.occurrence = 0;
            self.update_search_match();
            return;
        }
        self.insert(&clean);
    }

    pub(super) fn insert(&mut self, snippet: &str) {
        self.confirm_quit_until = None;
        self.text.insert_str(self.cursor, snippet);
        self.cursor = self.snap_grapheme(self.cursor + snippet.len());
    }

    pub(super) fn quit_pending(&self) -> bool {
        self.confirm_quit_until
            .is_some_and(|until| Instant::now() < until)
    }

    pub(super) fn desired_rows(&self) -> usize {
        if self.search.is_some() {
            return 1;
        }
        self.text.split('\n').count().clamp(1, MAX_VISIBLE_LINES)
    }

    pub(super) fn cursor_at_end(&self) -> bool {
        self.cursor == self.text.len()
    }

    pub(super) fn is_single_line(&self) -> bool {
        !self.text.contains('\n')
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> ComposerAction {
        if self.search.is_some() {
            return self.handle_search_key(key);
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        if !(ctrl && key.code == KeyCode::Char('d')) {
            self.confirm_quit_until = None;
        }

        if ctrl {
            match key.code {
                KeyCode::Char('c') => {
                    if self.text.is_empty() {
                        return ComposerAction::Cancel;
                    }
                    self.clear();
                    return ComposerAction::Changed;
                }
                KeyCode::Char('d') if self.text.is_empty() => {
                    let now = Instant::now();
                    if self.confirm_quit_until.is_some_and(|until| now < until) {
                        self.confirm_quit_until = None;
                        return ComposerAction::Quit;
                    }
                    self.confirm_quit_until = Some(now + CONFIRM_QUIT_WINDOW);
                    return ComposerAction::ConfirmQuit;
                }
                KeyCode::Char('d') => {
                    let right = self.right();
                    if right > self.cursor {
                        self.text.replace_range(self.cursor..right, "");
                        return ComposerAction::Changed;
                    }
                    return ComposerAction::None;
                }
                KeyCode::Char('j') => {
                    self.insert("\n");
                    return ComposerAction::Changed;
                }
                KeyCode::Char('a') => {
                    self.cursor = self.line_start(self.cursor);
                    return ComposerAction::None;
                }
                KeyCode::Char('e') => {
                    self.cursor = self.line_end(self.cursor);
                    return ComposerAction::None;
                }
                KeyCode::Char('u') => {
                    let start = self.line_start(self.cursor);
                    if start == self.cursor && start > 0 {
                        self.clear();
                    } else {
                        self.text.replace_range(start..self.cursor, "");
                        self.cursor = start;
                    }
                    return ComposerAction::Changed;
                }
                KeyCode::Char('k') => {
                    let end = self.line_end(self.cursor);
                    if end > self.cursor {
                        self.text.replace_range(self.cursor..end, "");
                        return ComposerAction::Changed;
                    } else if self.cursor < self.text.len() {
                        self.text.replace_range(self.cursor..self.cursor + 1, "");
                        return ComposerAction::Changed;
                    }
                    return ComposerAction::None;
                }
                KeyCode::Char('w') => {
                    return self.delete_prev_word();
                }
                KeyCode::Char('r') => {
                    self.start_reverse_search();
                    return ComposerAction::Changed;
                }
                KeyCode::Left => {
                    self.cursor = self.prev_word();
                    return ComposerAction::None;
                }
                KeyCode::Right => {
                    self.cursor = self.next_word();
                    return ComposerAction::None;
                }
                _ => return ComposerAction::None,
            }
        }

        if alt {
            match key.code {
                KeyCode::Enter => {
                    self.insert("\n");
                    return ComposerAction::Changed;
                }
                KeyCode::Char('b') | KeyCode::Left => {
                    self.cursor = self.prev_word();
                    return ComposerAction::None;
                }
                KeyCode::Char('f') | KeyCode::Right => {
                    self.cursor = self.next_word();
                    return ComposerAction::None;
                }
                KeyCode::Backspace => return self.delete_prev_word(),
                KeyCode::Char('d') => {
                    let next = self.next_word();
                    if next > self.cursor {
                        self.text.replace_range(self.cursor..next, "");
                        return ComposerAction::Changed;
                    }
                    return ComposerAction::None;
                }
                _ => return ComposerAction::None,
            }
        }

        match key.code {
            KeyCode::Enter if shift => {
                self.insert("\n");
                ComposerAction::Changed
            }
            KeyCode::Enter => self.submit(),
            KeyCode::Up => {
                if self.move_vertical(-1) {
                    ComposerAction::None
                } else {
                    self.history_step(true);
                    ComposerAction::Changed
                }
            }
            KeyCode::Down => {
                if self.move_vertical(1) {
                    ComposerAction::None
                } else {
                    self.history_step(false);
                    ComposerAction::Changed
                }
            }
            KeyCode::Left => {
                self.cursor = self.left();
                ComposerAction::None
            }
            KeyCode::Right => {
                self.cursor = self.right();
                ComposerAction::None
            }
            KeyCode::Home => {
                self.cursor = self.line_start(self.cursor);
                ComposerAction::None
            }
            KeyCode::End => {
                self.cursor = self.line_end(self.cursor);
                ComposerAction::None
            }
            KeyCode::Backspace => {
                let left = self.left();
                if left < self.cursor {
                    self.text.replace_range(left..self.cursor, "");
                    self.cursor = left;
                    ComposerAction::Changed
                } else {
                    ComposerAction::None
                }
            }
            KeyCode::Delete => {
                let right = self.right();
                if right > self.cursor {
                    self.text.replace_range(self.cursor..right, "");
                    ComposerAction::Changed
                } else {
                    ComposerAction::None
                }
            }
            KeyCode::Char(ch) if !ch.is_control() => {
                let mut buf = [0u8; 4];
                self.insert(ch.encode_utf8(&mut buf));
                ComposerAction::Changed
            }
            _ => ComposerAction::None,
        }
    }

    pub(super) fn submit(&mut self) -> ComposerAction {
        let text = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.history_index = None;
        self.saved_draft.clear();
        self.scroll_row = 0;
        self.confirm_quit_until = None;
        self.record_history(&text);
        if text.trim().is_empty() {
            ComposerAction::None
        } else {
            ComposerAction::Submit(text)
        }
    }

    pub(super) fn record_history(&mut self, text: &str) {
        let mut added = false;
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            added |= self.history.add(line.trim_end()).unwrap_or(false);
        }
        if added && let Some(path) = &self.path {
            let _ = self.history.append(path);
        }
    }

    pub(super) fn visible(&mut self, columns: usize, max_rows: usize) -> VisibleComposer {
        let cols = columns.max(1);
        let max_rows = max_rows.clamp(1, MAX_VISIBLE_LINES);

        if let Some(search) = &self.search {
            let display = match &search.matched {
                Some(matched) => format!("(reverse-i-search) `{}`: {}", search.query, matched),
                None => format!("(reverse-i-search) `{}`: [no match]", search.query),
            };
            let prefix = format!("(reverse-i-search) `{}", search.query);
            let (line, col) = slice_horizontal(&display, prefix.len(), cols);
            return VisibleComposer {
                lines: vec![line],
                cursor_col: col,
                cursor_row: 0,
                line_badge: None,
            };
        }

        let raw_lines: Vec<&str> = self.text.split('\n').collect();
        let total = raw_lines.len();
        let (cur_line, col_byte) = self.cursor_line_col();

        if cur_line < self.scroll_row {
            self.scroll_row = cur_line;
        } else if cur_line >= self.scroll_row + max_rows {
            self.scroll_row = cur_line + 1 - max_rows;
        }
        let max_scroll = total.saturating_sub(max_rows);
        if self.scroll_row > max_scroll {
            self.scroll_row = max_scroll;
        }

        let end = (self.scroll_row + max_rows).min(total);
        let mut lines = Vec::with_capacity(end - self.scroll_row);
        let mut cursor_col = 0u16;
        for (offset, &raw) in raw_lines[self.scroll_row..end].iter().enumerate() {
            let line_idx = self.scroll_row + offset;
            let expanded = raw.replace('\t', "    ");
            if line_idx == cur_line {
                let expanded_col = raw[..col_byte].replace('\t', "    ").len();
                let (vis, col) = slice_horizontal(&expanded, expanded_col, cols);
                lines.push(vis);
                cursor_col = col;
            } else {
                let (vis, _) = slice_horizontal(&expanded, 0, cols);
                lines.push(vis);
            }
        }
        let line_badge = (total > 1).then_some((cur_line + 1, total));
        VisibleComposer {
            lines,
            cursor_col,
            cursor_row: (cur_line - self.scroll_row) as u16,
            line_badge,
        }
    }

    fn start_reverse_search(&mut self) {
        self.search = Some(ReverseSearch {
            query: String::new(),
            occurrence: 0,
            matched: None,
            saved_draft: self.text.clone(),
            saved_cursor: self.cursor,
        });
        self.update_search_match();
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> ComposerAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                if let Some(search) = self.search.take() {
                    self.text = search.saved_draft;
                    self.cursor = search.saved_cursor;
                }
                ComposerAction::Changed
            }
            KeyCode::Char('c') | KeyCode::Char('g') if ctrl => {
                if let Some(search) = self.search.take() {
                    self.text = search.saved_draft;
                    self.cursor = search.saved_cursor;
                }
                ComposerAction::Changed
            }
            KeyCode::Char('r') if ctrl => {
                if let Some(search) = &mut self.search {
                    search.occurrence += 1;
                }
                self.update_search_match();
                ComposerAction::Changed
            }
            KeyCode::Enter => {
                if let Some(search) = self.search.take() {
                    if let Some(matched) = search.matched {
                        self.set_text(matched);
                        return self.submit();
                    }
                    self.text = search.saved_draft;
                    self.cursor = search.saved_cursor;
                }
                ComposerAction::Changed
            }
            KeyCode::Tab | KeyCode::Right => {
                if let Some(search) = self.search.take() {
                    if let Some(matched) = search.matched {
                        self.set_text(matched);
                    } else {
                        self.text = search.saved_draft;
                        self.cursor = search.saved_cursor;
                    }
                }
                ComposerAction::Changed
            }
            KeyCode::Backspace => {
                if let Some(search) = &mut self.search {
                    search.query.pop();
                    search.occurrence = 0;
                }
                self.update_search_match();
                ComposerAction::Changed
            }
            KeyCode::Char(ch) if !ctrl && !ch.is_control() => {
                if let Some(search) = &mut self.search {
                    search.query.push(ch);
                    search.occurrence = 0;
                }
                self.update_search_match();
                ComposerAction::Changed
            }
            _ => ComposerAction::None,
        }
    }

    fn update_search_match(&mut self) {
        let Some(search) = &mut self.search else {
            return;
        };
        let query = search.query.to_lowercase();
        let len = self.history.len();
        let mut matches = Vec::new();
        for idx in (0..len).rev() {
            if let Ok(Some(entry)) = self.history.get(idx, SearchDirection::Reverse) {
                let text = entry.entry.into_owned();
                if (query.is_empty() || text.to_lowercase().contains(&query))
                    && !matches.contains(&text)
                {
                    matches.push(text);
                }
            }
        }
        if matches.is_empty() {
            search.matched = None;
            search.occurrence = 0;
        } else {
            let pick = search.occurrence % matches.len();
            search.occurrence = pick;
            search.matched = Some(matches[pick].clone());
        }
    }

    fn history_step(&mut self, previous: bool) {
        let len = self.history.len();
        if previous && self.history_index.is_none() {
            self.saved_draft.clone_from(&self.text);
        }
        let next = match (previous, self.history_index) {
            (true, Some(0)) => return,
            (true, Some(idx)) => idx - 1,
            (true, None) => {
                if len == 0 {
                    return;
                }
                len - 1
            }
            (false, Some(idx)) => idx + 1,
            (false, None) => return,
        };
        if let Ok(Some(entry)) = self.history.get(next, SearchDirection::Reverse) {
            self.set_text(entry.entry.into_owned());
            self.history_index = Some(next);
        } else if !previous {
            let draft = std::mem::take(&mut self.saved_draft);
            self.set_text(draft);
            self.history_index = None;
        }
    }

    fn move_vertical(&mut self, delta: isize) -> bool {
        if !self.text.contains('\n') {
            return false;
        }
        let (cur_line, col_byte) = self.cursor_line_col();
        let target_line = cur_line as isize + delta;
        let starts: Vec<usize> = std::iter::once(0)
            .chain(self.text.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        if target_line < 0 || target_line as usize >= starts.len() {
            return false;
        }
        let target_line = target_line as usize;
        let target_col_width = self.text[starts[cur_line]..starts[cur_line] + col_byte].width();
        let line_start = starts[target_line];
        let line_end = self.line_end(line_start);
        let slice = &self.text[line_start..line_end];
        let mut acc = 0usize;
        let mut chosen = slice.len();
        for (idx, g) in slice.grapheme_indices(true) {
            if acc >= target_col_width {
                chosen = idx;
                break;
            }
            acc += g.width();
        }
        self.cursor = line_start + chosen;
        true
    }

    fn cursor_line_col(&self) -> (usize, usize) {
        let before = &self.text[..self.cursor];
        let line = before.bytes().filter(|&b| b == b'\n').count();
        let line_start = before.rfind('\n').map_or(0, |i| i + 1);
        (line, self.cursor - line_start)
    }

    fn line_start(&self, pos: usize) -> usize {
        self.text[..pos].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self, pos: usize) -> usize {
        self.text[pos..]
            .find('\n')
            .map_or(self.text.len(), |i| pos + i)
    }

    fn left(&self) -> usize {
        self.text[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn right(&self) -> usize {
        self.cursor
            + self.text[self.cursor..]
                .graphemes(true)
                .next()
                .map_or(0, str::len)
    }

    fn prev_word(&self) -> usize {
        let mut pos = self.cursor;
        while pos > 0 {
            let prev = self.text[..pos]
                .grapheme_indices(true)
                .next_back()
                .map_or(0, |(i, _)| i);
            if !is_word_sep(&self.text[prev..pos]) {
                break;
            }
            pos = prev;
        }
        while pos > 0 {
            let prev = self.text[..pos]
                .grapheme_indices(true)
                .next_back()
                .map_or(0, |(i, _)| i);
            if is_word_sep(&self.text[prev..pos]) {
                break;
            }
            pos = prev;
        }
        pos
    }

    fn next_word(&self) -> usize {
        let mut pos = self.cursor;
        while pos < self.text.len() {
            let next = pos + self.text[pos..].graphemes(true).next().map_or(0, str::len);
            if !is_word_sep(&self.text[pos..next]) {
                break;
            }
            pos = next;
        }
        while pos < self.text.len() {
            let next = pos + self.text[pos..].graphemes(true).next().map_or(0, str::len);
            if is_word_sep(&self.text[pos..next]) {
                break;
            }
            pos = next;
        }
        pos
    }

    fn delete_prev_word(&mut self) -> ComposerAction {
        let prev = self.prev_word();
        if prev < self.cursor {
            self.text.replace_range(prev..self.cursor, "");
            self.cursor = prev;
            ComposerAction::Changed
        } else {
            ComposerAction::None
        }
    }

    fn snap_grapheme(&self, pos: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .find(|&i| i >= pos)
            .unwrap_or(self.text.len())
    }
}

fn is_word_sep(grapheme: &str) -> bool {
    grapheme
        .chars()
        .all(|c| c.is_whitespace() || matches!(c, '/' | ':' | '=' | ','))
}

fn slice_horizontal(line: &str, cursor_byte: usize, columns: usize) -> (String, u16) {
    let cursor_byte = cursor_byte.min(line.len());
    let before = &line[..cursor_byte];
    let after = &line[cursor_byte..];
    let mut painted = 0usize;
    let mut start = before.len();
    for (offset, glyph) in before.grapheme_indices(true).rev() {
        let w = glyph.width();
        if painted + w >= columns {
            break;
        }
        painted += w;
        start = offset;
    }
    let prefix = &before[start..];
    let cursor_col = prefix.width() as u16;
    let mut out = String::from(prefix);
    let mut used = prefix.width();
    for glyph in after.graphemes(true) {
        let w = glyph.width();
        if used + w > columns {
            break;
        }
        out.push_str(glyph);
        used += w;
    }
    (out, cursor_col)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
    }

    #[test]
    fn multiline_navigation_moves_lines_before_history() {
        let mut c = Composer::new(None);
        c.paste("line one\nline two");
        assert_eq!(c.desired_rows(), 2);
        let vis = c.visible(40, 8);
        assert_eq!(vis.lines, vec!["line one", "line two"]);
        assert_eq!(vis.cursor_row, 1);
        assert_eq!(vis.line_badge, Some((2, 2)));
        c.handle_key(key(KeyCode::Up));
        let vis = c.visible(40, 8);
        assert_eq!(vis.cursor_row, 0);
        assert_eq!(vis.line_badge, Some((1, 2)));
    }

    #[test]
    fn double_ctrl_d_confirms_then_quits() {
        let mut c = Composer::new(None);
        assert_eq!(c.handle_key(ctrl('d')), ComposerAction::ConfirmQuit);
        assert_eq!(c.handle_key(ctrl('d')), ComposerAction::Quit);
    }

    #[test]
    fn word_delete_and_ctrl_k() {
        let mut c = Composer::new(None);
        c.paste("mem 0x60000000 4");
        c.handle_key(ctrl('w'));
        assert_eq!(c.text, "mem 0x60000000 ");
        c.handle_key(ctrl('a'));
        c.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT));
        c.handle_key(ctrl('k'));
        assert_eq!(c.text, "mem");
    }
}
