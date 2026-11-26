//! Pure-safe ANSI SGR parser, line wrapper, and serializer for the TUI.
//!
//! Parses SGR sequences emitted by `probecore::style`, `clap`, or firmware
//! logs into `ratatui::text::Line<'static>`, while stripping non-SGR escape
//! sequences instead of printing stray escape symbols.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Parse multi-line text containing ANSI SGR sequences into Ratatui lines.
/// When `color` is false, all SGR styles are stripped to plain text.
pub(super) fn parse_lines(text: &str, base: Style, color: bool) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut current_style = if color { base } else { Style::default() };
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();

    let flush_span = |buf: &mut String, spans: &mut Vec<Span<'static>>, style: Style| {
        if !buf.is_empty() {
            spans.push(Span::styled(std::mem::take(buf), style));
        }
    };

    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            flush_span(&mut buf, &mut spans, current_style);
            i += 1;
            if i >= bytes.len() {
                break;
            }
            match bytes[i] {
                b'[' => {
                    // CSI sequence: ESC [ <params> <intermediates> <final 0x40..=0x7E>
                    i += 1;
                    let start = i;
                    while i < bytes.len() && !(0x40..=0x7E).contains(&bytes[i]) {
                        i += 1;
                    }
                    if i < bytes.len() {
                        let final_byte = bytes[i];
                        let params = &text[start..i];
                        i += 1;
                        if final_byte == b'm' && color {
                            apply_sgr(&mut current_style, base, params);
                        }
                    }
                }
                b']' => {
                    // OSC sequence: terminated by BEL or ST (ESC \).
                    i += 1;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                _ => {
                    // Two-byte escape sequence.
                    i += 1;
                }
            }
            continue;
        }

        let ch = text[i..].chars().next().expect("valid utf-8 boundary");
        i += ch.len_utf8();
        match ch {
            '\n' => {
                flush_span(&mut buf, &mut spans, current_style);
                lines.push(Line::from(std::mem::take(&mut spans)));
            }
            '\r' => {}
            '\t' => buf.push_str("    "),
            c if c.is_control() => buf.push('\u{FFFD}'),
            c => buf.push(c),
        }
    }
    flush_span(&mut buf, &mut spans, current_style);
    lines.push(Line::from(spans));
    lines
}

fn apply_sgr(style: &mut Style, base: Style, params: &str) {
    if params.is_empty() {
        *style = base;
        return;
    }
    let nums: Vec<u16> = params
        .split(';')
        .map(|part| part.parse::<u16>().unwrap_or(0))
        .collect();
    let mut idx = 0;
    while idx < nums.len() {
        match nums[idx] {
            0 => *style = base,
            1 => *style = style.add_modifier(Modifier::BOLD),
            2 => *style = style.add_modifier(Modifier::DIM),
            3 => *style = style.add_modifier(Modifier::ITALIC),
            4 => *style = style.add_modifier(Modifier::UNDERLINED),
            7 => *style = style.add_modifier(Modifier::REVERSED),
            22 => *style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => *style = style.remove_modifier(Modifier::ITALIC),
            24 => *style = style.remove_modifier(Modifier::UNDERLINED),
            27 => *style = style.remove_modifier(Modifier::REVERSED),
            30 => *style = style.fg(Color::Black),
            31 => *style = style.fg(Color::Red),
            32 => *style = style.fg(Color::Green),
            33 => *style = style.fg(Color::Yellow),
            34 => *style = style.fg(Color::Blue),
            35 => *style = style.fg(Color::Magenta),
            36 => *style = style.fg(Color::Cyan),
            37 => *style = style.fg(Color::Gray),
            39 => *style = style.fg(base.fg.unwrap_or(Color::Reset)),
            40 => *style = style.bg(Color::Black),
            41 => *style = style.bg(Color::Red),
            42 => *style = style.bg(Color::Green),
            43 => *style = style.bg(Color::Yellow),
            44 => *style = style.bg(Color::Blue),
            45 => *style = style.bg(Color::Magenta),
            46 => *style = style.bg(Color::Cyan),
            47 => *style = style.bg(Color::Gray),
            49 => *style = style.bg(base.bg.unwrap_or(Color::Reset)),
            90 => *style = style.add_modifier(Modifier::DIM),
            91 => *style = style.fg(Color::LightRed),
            92 => *style = style.fg(Color::LightGreen),
            93 => *style = style.fg(Color::LightYellow),
            94 => *style = style.fg(Color::LightBlue),
            95 => *style = style.fg(Color::LightMagenta),
            96 => *style = style.fg(Color::LightCyan),
            97 => *style = style.fg(Color::White),
            100 => *style = style.bg(Color::DarkGray),
            101 => *style = style.bg(Color::LightRed),
            102 => *style = style.bg(Color::LightGreen),
            103 => *style = style.bg(Color::LightYellow),
            104 => *style = style.bg(Color::LightBlue),
            105 => *style = style.bg(Color::LightMagenta),
            106 => *style = style.bg(Color::LightCyan),
            107 => *style = style.bg(Color::White),
            38 | 48 => {
                let is_fg = nums[idx] == 38;
                if idx + 2 < nums.len() && nums[idx + 1] == 5 {
                    let c = Color::Indexed(nums[idx + 2] as u8);
                    *style = if is_fg { style.fg(c) } else { style.bg(c) };
                    idx += 2;
                } else if idx + 4 < nums.len() && nums[idx + 1] == 2 {
                    let c = Color::Rgb(
                        nums[idx + 2] as u8,
                        nums[idx + 3] as u8,
                        nums[idx + 4] as u8,
                    );
                    *style = if is_fg { style.fg(c) } else { style.bg(c) };
                    idx += 4;
                }
            }
            _ => {}
        }
        idx += 1;
    }
}

/// Soft-wrap a styled `Line` into display rows of at most `width` columns,
/// prefixing every emitted row with `indent` spaces (using `indent_style`).
pub(super) fn wrap_line(line: &Line<'static>, width: usize, indent: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let effective_indent = indent.min(width.saturating_sub(1));
    let content_width = width - effective_indent;
    let prefix = " ".repeat(effective_indent);

    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut row_spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;

    let push_prefix = |row_spans: &mut Vec<Span<'static>>| {
        if effective_indent > 0 {
            row_spans.push(Span::raw(prefix.clone()));
        }
    };

    push_prefix(&mut row_spans);
    let mut cur_buf = String::new();
    let mut cur_style = Style::default();

    for span in &line.spans {
        if span.style != cur_style && !cur_buf.is_empty() {
            row_spans.push(Span::styled(std::mem::take(&mut cur_buf), cur_style));
        }
        cur_style = span.style;
        for glyph in span.content.graphemes(true) {
            let w = glyph.width();
            if w > content_width {
                continue;
            }
            if used + w > content_width && used > 0 {
                if !cur_buf.is_empty() {
                    row_spans.push(Span::styled(std::mem::take(&mut cur_buf), cur_style));
                }
                rows.push(Line::from(std::mem::take(&mut row_spans)));
                push_prefix(&mut row_spans);
                used = 0;
            }
            cur_buf.push_str(glyph);
            used += w;
        }
    }
    if !cur_buf.is_empty() {
        row_spans.push(Span::styled(cur_buf, cur_style));
    }
    rows.push(Line::from(row_spans));
    rows
}

/// Extract plain text from a styled `Line`.
pub(super) fn plain_text(line: &Line<'_>) -> String {
    let mut out = String::new();
    for span in &line.spans {
        out.push_str(&span.content);
    }
    out
}

/// Serialize a Ratatui `Line` into an ANSI-encoded logical string for native
/// terminal scrollback in Inline mode.
pub(super) fn to_ansi(line: &Line<'_>, indent: usize, color: bool) -> String {
    let mut out = String::new();
    if indent > 0 {
        for _ in 0..indent {
            out.push(' ');
        }
    }
    if !color {
        for span in &line.spans {
            out.push_str(&span.content);
        }
        return out;
    }
    let mut active = Style::default();
    for span in &line.spans {
        if span.content.is_empty() {
            continue;
        }
        if span.style != active {
            write_sgr(&mut out, span.style);
            active = span.style;
        }
        out.push_str(&span.content);
    }
    if active != Style::default() {
        out.push_str("\x1b[0m");
    }
    out
}

/// Write a full SGR reset + attributes sequence for `style`.
pub(super) fn write_sgr(out: &mut String, style: Style) {
    out.push_str("\x1b[0");
    if style.add_modifier.contains(Modifier::BOLD) {
        out.push_str(";1");
    }
    if style.add_modifier.contains(Modifier::DIM) {
        out.push_str(";2");
    }
    if style.add_modifier.contains(Modifier::ITALIC) {
        out.push_str(";3");
    }
    if style.add_modifier.contains(Modifier::UNDERLINED) {
        out.push_str(";4");
    }
    if style.add_modifier.contains(Modifier::REVERSED) {
        out.push_str(";7");
    }
    if let Some(fg) = style.fg {
        write_color(out, fg, true);
    }
    if let Some(bg) = style.bg {
        write_color(out, bg, false);
    }
    out.push('m');
}

fn write_color(out: &mut String, color: Color, fg: bool) {
    use std::fmt::Write;
    let base: u8 = if fg { 30 } else { 40 };
    let bright: u8 = if fg { 90 } else { 100 };
    let ext: u8 = if fg { 38 } else { 48 };
    match color {
        Color::Reset => {}
        Color::Black => {
            let _ = write!(out, ";{base}");
        }
        Color::Red => {
            let _ = write!(out, ";{}", base + 1);
        }
        Color::Green => {
            let _ = write!(out, ";{}", base + 2);
        }
        Color::Yellow => {
            let _ = write!(out, ";{}", base + 3);
        }
        Color::Blue => {
            let _ = write!(out, ";{}", base + 4);
        }
        Color::Magenta => {
            let _ = write!(out, ";{}", base + 5);
        }
        Color::Cyan => {
            let _ = write!(out, ";{}", base + 6);
        }
        Color::Gray => {
            let _ = write!(out, ";{}", base + 7);
        }
        Color::DarkGray => {
            let _ = write!(out, ";{bright}");
        }
        Color::LightRed => {
            let _ = write!(out, ";{}", bright + 1);
        }
        Color::LightGreen => {
            let _ = write!(out, ";{}", bright + 2);
        }
        Color::LightYellow => {
            let _ = write!(out, ";{}", bright + 3);
        }
        Color::LightBlue => {
            let _ = write!(out, ";{}", bright + 4);
        }
        Color::LightMagenta => {
            let _ = write!(out, ";{}", bright + 5);
        }
        Color::LightCyan => {
            let _ = write!(out, ";{}", bright + 6);
        }
        Color::White => {
            let _ = write!(out, ";{}", bright + 7);
        }
        Color::Indexed(n) => {
            let _ = write!(out, ";{ext};5;{n}");
        }
        Color::Rgb(r, g, b) => {
            let _ = write!(out, ";{ext};2;{r};{g};{b}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sgr_and_strips_non_sgr() {
        let input = "\x1b[36mstatus\x1b[0m \x1b[1;31mFAIL\x1b[0m\x1b[2K\x1b]0;title\x07";
        let lines = parse_lines(input, Style::default(), true);
        assert_eq!(lines.len(), 1);
        assert_eq!(plain_text(&lines[0]), "status FAIL");
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Cyan));
        assert_eq!(lines[0].spans[2].style.fg, Some(Color::Red));
        assert!(
            lines[0].spans[2]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn respects_no_color() {
        let input = "\x1b[32mrunning\x1b[0m";
        let lines = parse_lines(input, Style::default(), false);
        assert_eq!(plain_text(&lines[0]), "running");
        assert_eq!(lines[0].spans[0].style, Style::default());
        assert_eq!(to_ansi(&lines[0], 2, false), "  running");
    }

    #[test]
    fn wraps_preserving_indent_and_styles() {
        let line = Line::from(vec![
            Span::styled("abcd", Style::default().fg(Color::Cyan)),
            Span::styled("efgh", Style::default().fg(Color::Yellow)),
        ]);
        let wrapped = wrap_line(&line, 6, 2);
        assert_eq!(wrapped.len(), 2);
        assert_eq!(plain_text(&wrapped[0]), "  abcd");
        assert_eq!(plain_text(&wrapped[1]), "  efgh");
        assert_eq!(wrapped[1].spans[1].style.fg, Some(Color::Yellow));
    }
}
