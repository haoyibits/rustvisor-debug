use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rustyline::history::{DefaultHistory, History, SearchDirection};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::session::MenuCommand;

pub(super) const MENU_ROWS: usize = 4;

pub(super) enum Action {
    Edit,
    Submit(String),
    Quit,
}

pub(super) struct Editor {
    pub(super) text: String,
    cursor: usize,
    pub(super) selected: usize,
    menu_closed: bool,
    history: DefaultHistory,
    path: Option<PathBuf>,
    history_index: Option<usize>,
    saved_draft: String,
}

impl Editor {
    pub(super) fn new(path: Option<PathBuf>) -> Self {
        let mut history = DefaultHistory::new();
        if let Some(path) = &path {
            let _ = history.load(path);
        }
        Self {
            text: String::new(),
            cursor: 0,
            selected: 0,
            menu_closed: false,
            history,
            path,
            history_index: None,
            saved_draft: String::new(),
        }
    }

    pub(super) fn clear(&mut self) {
        self.replace(String::new());
        self.history_index = None;
        self.saved_draft.clear();
    }

    pub(super) fn paste(&mut self, pasted: &str) {
        let text = pasted.replace("\r\n", "\n").replace('\r', "\n");
        let text: String = text
            .chars()
            .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
            .collect();
        self.insert(&text);
    }

    fn insert(&mut self, text: &str) {
        self.menu_closed = false;
        self.selected = 0;
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
        // A pasted character can join an existing grapheme on the right.
        self.cursor = self
            .text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .find(|&i| i >= self.cursor)
            .unwrap_or(self.text.len());
    }

    fn replace(&mut self, text: String) {
        self.text = text;
        self.cursor = self.text.len();
        self.selected = 0;
        self.menu_closed = false;
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

    pub(super) fn matches<'a>(&self, commands: &'a [MenuCommand]) -> Vec<&'a MenuCommand> {
        let Some(query) = self
            .text
            .strip_prefix('/')
            .filter(|s| !s.contains(char::is_whitespace))
        else {
            return Vec::new();
        };
        if self.menu_closed {
            return Vec::new();
        }
        commands
            .iter()
            .filter(|entry| entry.name.starts_with(query))
            .collect()
    }

    fn history(&mut self, previous: bool) {
        let len = self.history.len();
        if previous && self.history_index.is_none() {
            self.saved_draft.clone_from(&self.text);
        }
        let next = match (previous, self.history_index) {
            (true, index) => index.unwrap_or(len).saturating_sub(1),
            (false, Some(index)) => index + 1,
            (false, None) => return,
        };
        if let Ok(Some(entry)) = self.history.get(next, SearchDirection::Reverse) {
            self.replace(entry.entry.into_owned());
            self.history_index = Some(next);
        } else if !previous {
            let draft = std::mem::take(&mut self.saved_draft);
            self.replace(draft);
            self.history_index = None;
        }
    }

    fn submit(&mut self) -> Action {
        let text = std::mem::take(&mut self.text);
        self.replace(String::new());
        self.history_index = None;
        let mut added = false;
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            added |= self.history.add(line.trim_end()).unwrap_or(false);
        }
        if added && let Some(path) = &self.path {
            let _ = self.history.append(path);
        }
        if text.trim().is_empty() {
            Action::Edit
        } else {
            Action::Submit(text)
        }
    }

    pub(super) fn key(&mut self, key: KeyEvent, commands: &[MenuCommand]) -> Action {
        let menu = self.matches(commands);
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c' | 'u') => {
                    self.replace(String::new());
                    self.history_index = None;
                }
                KeyCode::Char('d') if self.text.is_empty() => return Action::Quit,
                KeyCode::Char('d') => {
                    let right = self.right();
                    self.text.replace_range(self.cursor..right, "");
                    self.selected = 0;
                }
                KeyCode::Char('a') => self.cursor = 0,
                KeyCode::Char('e') => self.cursor = self.text.len(),
                _ => {}
            }
            return Action::Edit;
        }
        match key.code {
            KeyCode::Tab | KeyCode::Enter if !menu.is_empty() => {
                // Fill in the command and leave it editable for flags and arguments.
                self.replace(format!("{} ", menu[self.selected.min(menu.len() - 1)].name));
            }
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
            {
                self.insert("\n");
            }
            KeyCode::Enter if self.text.starts_with('/') => {
                self.menu_closed = false;
            }
            KeyCode::Enter => return self.submit(),
            KeyCode::Up if !menu.is_empty() => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down if !menu.is_empty() => {
                self.selected = (self.selected + 1).min(menu.len() - 1)
            }
            KeyCode::Up => self.history(true),
            KeyCode::Down => self.history(false),
            KeyCode::Esc => self.menu_closed = true,
            KeyCode::Left => self.cursor = self.left(),
            KeyCode::Right => self.cursor = self.right(),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            KeyCode::Backspace => {
                let left = self.left();
                self.text.replace_range(left..self.cursor, "");
                self.cursor = left;
                self.selected = 0;
                self.menu_closed = false;
            }
            KeyCode::Delete => {
                let right = self.right();
                self.text.replace_range(self.cursor..right, "");
                self.selected = 0;
                self.menu_closed = false;
            }
            KeyCode::Char(ch) if !ch.is_control() => {
                self.insert(ch.encode_utf8(&mut [0; 4]));
            }
            _ => {}
        }
        Action::Edit
    }

    pub(super) fn visible(&self, columns: usize) -> (String, u16) {
        let line_start = self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
        let line_end = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i);
        let before = self.text[line_start..self.cursor].replace('\t', "    ");
        let after = self.text[self.cursor..line_end].replace('\t', "    ");
        let mut painted = 0;
        let mut start = before.len();
        for (offset, glyph) in before.grapheme_indices(true).rev() {
            painted += glyph.width();
            if painted >= columns {
                break;
            }
            start = offset;
        }
        (
            format!("{}{after}", &before[start..]),
            before[start..].width() as u16,
        )
    }
}
