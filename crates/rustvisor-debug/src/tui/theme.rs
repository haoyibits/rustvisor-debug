//! One Dark-friendly visual tokens for the TUI.
//!
//! Uses named ANSI 16 colors so the user's terminal palette (e.g. One Dark)
//! renders every component in harmony with `probecore::style`. Avoids
//! `Color::DarkGray` (ANSI 8) for text because One Dark themes often set it
//! very close to the `#282c34` background; secondary text uses `Modifier::DIM`
//! on the default foreground instead.

use ratatui::style::{Color, Modifier, Style};

pub(super) const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

#[derive(Debug, Clone, Copy)]
pub(super) struct Theme {
    pub color: bool,
    pub color256: bool,
}

impl Theme {
    pub(super) fn detect() -> Self {
        let color = probecore::style::Style::stdout().enabled();
        let color256 = color && supports_256_color();
        Self { color, color256 }
    }

    #[cfg(test)]
    pub(super) const PLAIN: Self = Self {
        color: false,
        color256: false,
    };

    pub(super) fn plain(self) -> Style {
        Style::default()
    }

    pub(super) fn bold(self) -> Style {
        if self.color {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        }
    }

    /// Secondary text, hints, ghost completion, inactive chrome.
    pub(super) fn dim(self) -> Style {
        if self.color {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default()
        }
    }

    pub(super) fn cyan(self) -> Style {
        self.fg(Color::Cyan)
    }

    pub(super) fn magenta(self) -> Style {
        self.fg(Color::Magenta)
    }

    pub(super) fn blue(self) -> Style {
        self.fg(Color::Blue)
    }

    pub(super) fn green(self) -> Style {
        self.fg(Color::Green)
    }

    pub(super) fn yellow(self) -> Style {
        self.fg(Color::Yellow)
    }

    pub(super) fn red(self) -> Style {
        self.fg(Color::Red)
    }

    pub(super) fn error(self) -> Style {
        if self.color {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        }
    }

    pub(super) fn border_command(self, active: bool) -> Style {
        if active { self.cyan() } else { self.dim() }
    }

    pub(super) fn border_fdx(self, active: bool) -> Style {
        if active { self.magenta() } else { self.dim() }
    }

    pub(super) fn border_rtt(self) -> Style {
        self.yellow()
    }

    pub(super) fn prompt_command(self) -> Style {
        if self.color {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        }
    }

    pub(super) fn prompt_fdx(self) -> Style {
        if self.color {
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        }
    }

    pub(super) fn prompt_rtt(self) -> Style {
        if self.color {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        }
    }

    pub(super) fn selected_row(self) -> Style {
        if self.color256 {
            Style::default()
                .bg(Color::Indexed(237))
                .add_modifier(Modifier::BOLD)
        } else if self.color {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::REVERSED)
        }
    }

    pub(super) fn selected_desc(self) -> Style {
        if self.color256 {
            Style::default().bg(Color::Indexed(237))
        } else if self.color {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().add_modifier(Modifier::REVERSED)
        }
    }

    pub(super) fn selected_marker(self) -> Style {
        if self.color256 {
            Style::default()
                .fg(Color::Cyan)
                .bg(Color::Indexed(237))
                .add_modifier(Modifier::BOLD)
        } else if self.color {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::REVERSED)
        }
    }

    pub(super) fn header_bar(self) -> Style {
        if self.color256 {
            Style::default().bg(Color::Indexed(235))
        } else {
            Style::default()
        }
    }

    pub(super) fn pill_style(self, target_state: &str) -> (char, Style) {
        if target_state.contains("running") {
            ('●', self.green())
        } else if target_state.contains("halted") {
            ('●', self.yellow())
        } else if target_state.contains("no probe")
            || target_state.contains("lost")
            || target_state.contains("no-PowerView")
            || target_state.contains("not-running")
        {
            ('○', self.red())
        } else if target_state.contains("Detecting") || target_state.contains("Connecting") {
            ('◐', self.cyan())
        } else {
            ('●', self.blue())
        }
    }

    fn fg(self, color: Color) -> Style {
        if self.color {
            Style::default().fg(color)
        } else {
            Style::default()
        }
    }
}

fn supports_256_color() -> bool {
    if std::env::var_os("COLORTERM").is_some_and(|v| !v.is_empty()) {
        return true;
    }
    std::env::var("TERM").is_ok_and(|term| {
        term.contains("256color")
            || term.contains("direct")
            || term.contains("kitty")
            || term.contains("ghostty")
            || term.contains("wezterm")
            || term.contains("alacritty")
    })
}
