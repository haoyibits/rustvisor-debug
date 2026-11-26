//! TrueColor startup logo for the TUI.
//!
//! Renders a precision 17×7 vector QFP microcontroller package with a
//! laser-etched 3-row `RV` monogram in the Rustvisor gradient
//! (`#FF6B35` → `#FACC15` → `#38BDF8`) alongside the session metadata,
//! gracefully degrading to 256-color, 16-color, plain text (`NO_COLOR`), or a
//! logo-free layout on narrow terminals (`< 68` columns).

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::ansi;
use super::theme::Theme;

/// Minimum terminal width to render the chip emblem beside the metadata.
pub(super) const MIN_LOGO_WIDTH: u16 = 68;

const STOPS: [(u8, u8, u8); 3] = [(255, 107, 53), (250, 204, 21), (56, 189, 248)];
const PIN_COLOR: (u8, u8, u8) = (229, 192, 123);

/// 17×7 QFP microcontroller with centered 3-row `RV` silkscreen monogram.
const CHIP_LINES: [&str; 7] = [
    "    ╷ ╷ ╷ ╷ ╷    ",
    " ╭──┴─┴─┴─┴─┴──╮ ",
    "╶┤  █▀▀▄ █  █  ├╴",
    "╶┤  █▄▄▀ █  █  ├╴",
    "╶┤  █  █ ▀▄▄▀  ├╴",
    " ╰──┬─┬─┬─┬─┬──╯ ",
    "    ╵ ╵ ╵ ╵ ╵    ",
];

/// Build the styled startup banner lines combining the `RV` MCU emblem (when
/// wide enough) with the session header metadata.
pub(super) fn banner_lines(header: &str, theme: Theme, width: u16) -> Vec<Line<'static>> {
    let mut info = ansi::parse_lines(header, theme.plain(), theme.color);
    if let Some(first) = info.first_mut() {
        style_rvdbg_prefix(first, theme);
    }
    let tips = Line::from(Span::styled(
        "/ menu · @target FDX · Tab complete",
        theme.dim(),
    ));
    if width < MIN_LOGO_WIDTH {
        if info.len() < 4 {
            info.push(tips);
        }
        return info;
    }

    // Vertically center the 5-row metadata block beside the 7-row chip
    // (1 blank row above and below aligned with the external pin rows).
    if info.len() >= 3 {
        info.insert(1, Line::from(Span::styled("─".repeat(30), theme.dim())));
    }
    if info.len() < 5 {
        info.push(tips);
    }

    let art = chip_rows(theme);
    let art_width = CHIP_LINES[0].width();
    let max_info = usize::from(width).saturating_sub(2 + art_width + 3);
    let mut out = Vec::with_capacity(CHIP_LINES.len());
    for (idx, art_spans) in art.into_iter().enumerate() {
        let mut spans = art_spans;
        if let Some(info_idx) = idx.checked_sub(1).filter(|&i| i < info.len()) {
            spans.push(Span::raw("   "));
            spans.extend(truncate_spans(&info[info_idx].spans, max_info, theme));
        }
        out.push(Line::from(spans));
    }
    out
}

fn truncate_spans(spans: &[Span<'static>], max_width: usize, theme: Theme) -> Vec<Span<'static>> {
    if max_width == 0 {
        return Vec::new();
    }
    let total: usize = spans.iter().map(|s| s.content.width()).sum();
    if total <= max_width {
        return spans.to_vec();
    }
    let limit = max_width.saturating_sub(1);
    let mut used = 0;
    let mut out = Vec::new();
    for span in spans {
        if used >= limit {
            break;
        }
        let w = span.content.width();
        if used + w <= limit {
            out.push(span.clone());
            used += w;
        } else {
            let mut part = String::new();
            for g in span.content.graphemes(true) {
                let gw = g.width();
                if used + gw > limit {
                    break;
                }
                part.push_str(g);
                used += gw;
            }
            if !part.is_empty() {
                out.push(Span::styled(part, span.style));
            }
            break;
        }
    }
    out.push(Span::styled("…", theme.dim()));
    out
}

fn style_rvdbg_prefix(line: &mut Line<'static>, theme: Theme) {
    let Some(first) = line.spans.first() else {
        return;
    };
    if first.content != "rvdbg" {
        return;
    }
    let rest: Vec<Span<'static>> = line.spans.drain(1..).collect();
    line.spans = gradient_text("rvdbg", theme, true);
    line.spans.extend(rest);
}

fn gradient_text(text: &str, theme: Theme, bold: bool) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let denom = chars.len().saturating_sub(1).max(1) as f32;
    chars
        .into_iter()
        .enumerate()
        .map(|(i, ch)| {
            if !theme.color || ch == ' ' {
                return Span::raw(ch.to_string());
            }
            let (r, g, b) = grad_color(i as f32 / denom);
            let mut style = Style::default().fg(theme.rgb(r, g, b));
            if bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            Span::styled(ch.to_string(), style)
        })
        .collect()
}

fn chip_rows(theme: Theme) -> Vec<Vec<Span<'static>>> {
    let h = CHIP_LINES.len();
    let w = CHIP_LINES[0].chars().count();
    CHIP_LINES
        .iter()
        .enumerate()
        .map(|(y, line)| {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (x, ch) in line.chars().enumerate() {
                let style = if !theme.color || ch == ' ' {
                    Style::default()
                } else if matches!(ch, '╷' | '╵' | '╴' | '╶') {
                    let (r, g, b) = PIN_COLOR;
                    Style::default().fg(theme.rgb(r, g, b))
                } else {
                    let tx = x as f32 / w.saturating_sub(1).max(1) as f32;
                    let ty = y as f32 / h.saturating_sub(1).max(1) as f32;
                    let (r, g, b) = grad_color(tx * 0.68 + ty * 0.32);
                    let mut s = Style::default().fg(theme.rgb(r, g, b));
                    if matches!(ch, '█' | '▀' | '▄') {
                        s = s.add_modifier(Modifier::BOLD);
                    }
                    s
                };
                if let Some(last) = spans.last_mut()
                    && last.style == style
                {
                    last.content.to_mut().push(ch);
                } else {
                    spans.push(Span::styled(ch.to_string(), style));
                }
            }
            spans
        })
        .collect()
}

fn grad_color(t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    let seg = t * (STOPS.len() - 1) as f32;
    let idx = (seg.floor() as usize).min(STOPS.len() - 2);
    let frac = seg - idx as f32;
    let (r1, g1, b1) = STOPS[idx];
    let (r2, g2, b2) = STOPS[idx + 1];
    (lerp(r1, r2, frac), lerp(g1, g2, frac), lerp(b1, b2, frac))
}

fn lerp(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_banner_renders_seven_rows_with_metadata_and_plain_fallback() {
        let header = "rvdbg 0.1.41  rvdbg.toml\nbackend  trace32\nimages   inject-five";
        let wide = banner_lines(header, Theme::PLAIN, 100);
        assert_eq!(wide.len(), 7);
        let text: Vec<String> = wide.iter().map(ansi::plain_text).collect();
        assert!(text[1].contains("rvdbg 0.1.41"));
        assert!(text[3].contains("backend  trace32"));
        assert!(text[4].contains("images   inject-five"));
        assert!(text[5].contains("/ menu"));
        assert!(
            wide.iter()
                .flat_map(|line| &line.spans)
                .all(|span| span.style.fg.is_none() && span.style.bg.is_none())
        );

        let narrow = banner_lines(header, Theme::PLAIN, 50);
        assert_eq!(narrow.len(), 4);
        assert!(ansi::plain_text(&narrow[0]).starts_with("rvdbg"));
    }

    #[test]
    fn truecolor_banner_emits_rgb_spans() {
        let theme = Theme {
            color: true,
            color256: true,
            truecolor: true,
        };
        let header = "rvdbg 0.1.41  rvdbg.toml\nbackend  trace32\nimages   inject-five";
        let lines = banner_lines(header, theme, 100);
        let has_rgb = lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| matches!(span.style.fg, Some(ratatui::style::Color::Rgb(_, _, _))));
        assert!(has_rgb);
    }
}
