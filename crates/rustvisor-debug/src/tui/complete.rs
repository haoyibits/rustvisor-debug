//! Context-aware completion, Ghost text hints, `/` slash command menu,
//! interactive pickers (`/images`, `/backend`, `/fdx`), and cached ELF symbol
//! lookups.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use clap::CommandFactory;

use crate::Cli;
use crate::config::{self, Backend, Config};
use crate::session::{self, MenuCommand};

pub(super) const POPUP_MAX_ROWS: usize = 6;

#[derive(Debug, Clone)]
pub(super) struct FlagSpec {
    pub flag: String,
    pub help: String,
}

#[derive(Debug, Clone)]
pub(super) struct CommandSpec {
    pub name: String,
    pub help: String,
    pub unavailable: Option<&'static str>,
    pub requires_args: bool,
    pub is_tui_action: bool,
    pub flags: Vec<FlagSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Candidate {
    pub insert: String,
    pub label: String,
    pub detail: String,
    pub unavailable: Option<&'static str>,
    /// When selected via Enter in `/` menu, execute directly instead of filling.
    pub run_on_enter: bool,
    /// Byte range `[replace_start..replace_end]` in the composer text to replace.
    pub replace_start: usize,
    pub replace_end: usize,
    /// Trailing space to append when filling.
    pub add_space: bool,
}

#[derive(Debug, Clone)]
pub(super) struct PickerItem {
    pub label: String,
    pub detail: String,
    pub command: String,
}

#[derive(Debug, Clone)]
pub(super) struct Picker {
    pub title: String,
    pub items: Vec<PickerItem>,
    pub selected: usize,
}

impl Picker {
    pub(super) fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub(super) fn move_down(&mut self) {
        if !self.items.is_empty() {
            self.selected = (self.selected + 1).min(self.items.len() - 1);
        }
    }

    pub(super) fn chosen(&self) -> Option<&PickerItem> {
        self.items.get(self.selected)
    }
}

/// Cached ELF symbols for `mem`, `ram`, and `run-until` completion,
/// invalidated automatically when image paths or mtimes change.
#[derive(Default)]
pub(super) struct SymbolCache {
    stamp: Vec<(PathBuf, Option<SystemTime>)>,
    symbols: Vec<(String, u64)>,
    per_image: Vec<(String, Vec<(String, u64)>)>,
}

impl SymbolCache {
    pub(super) fn refresh(&mut self, images: &[PathBuf]) {
        let next_stamp: Vec<(PathBuf, Option<SystemTime>)> = images
            .iter()
            .map(|p| {
                let mtime = std::fs::metadata(p).and_then(|m| m.modified()).ok();
                (p.clone(), mtime)
            })
            .collect();
        if self.stamp == next_stamp {
            return;
        }
        self.stamp = next_stamp;
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        let mut per_image = Vec::new();
        for (idx, path) in images.iter().enumerate() {
            let img_name = rustvisor_debug::fdx::image_name(idx.checked_sub(1));
            let Ok(bytes) = std::fs::read(path) else {
                per_image.push((img_name, Vec::new()));
                continue;
            };
            let Ok(list) = probecore::elf::named_symbols(&bytes) else {
                per_image.push((img_name, Vec::new()));
                continue;
            };
            let mut img_syms = Vec::new();
            for (name, value, is_abs) in list {
                if name.is_empty() || (is_abs && value < 0x10000) {
                    continue;
                }
                img_syms.push((name.clone(), value));
                if seen.insert(name.clone()) {
                    out.push((name, value));
                }
            }
            img_syms.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            img_syms.dedup_by(|a, b| a.0 == b.0);
            per_image.push((img_name, img_syms));
        }
        out.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        self.symbols = out;
        self.per_image = per_image;
    }

    pub(super) fn matches(&self, prefix: &str, limit: usize) -> Vec<(&str, u64)> {
        match_symbol_slice(&self.symbols, prefix, limit)
    }

    pub(super) fn scoped_matches(
        &self,
        scope: &str,
        prefix: &str,
        limit: usize,
    ) -> Vec<(&str, u64)> {
        let names: Vec<String> = self.per_image.iter().map(|(n, _)| n.clone()).collect();
        let Ok(indices) = rustvisor_debug::fdx::resolve_targets(scope, &names) else {
            return Vec::new();
        };
        if let Some(&first) = indices.first() {
            return match_symbol_slice(&self.per_image[first].1, prefix, limit);
        }
        Vec::new()
    }
}

fn match_symbol_slice<'a>(
    list: &'a [(String, u64)],
    prefix: &str,
    limit: usize,
) -> Vec<(&'a str, u64)> {
    let needle = prefix.to_lowercase();
    let mut starts = Vec::new();
    let mut contains = Vec::new();
    for (name, addr) in list {
        let lower = name.to_lowercase();
        if lower.starts_with(&needle) {
            starts.push((name.as_str(), *addr));
            if starts.len() >= limit {
                break;
            }
        } else if !needle.is_empty() && contains.len() < limit && lower.contains(&needle) {
            contains.push((name.as_str(), *addr));
        }
    }
    for item in contains {
        if starts.len() >= limit {
            break;
        }
        starts.push(item);
    }
    starts
}

pub(super) fn build_specs(backend: Option<Backend>) -> Vec<CommandSpec> {
    let menu: Vec<MenuCommand> = session::menu_commands(backend);
    let cli = Cli::command();
    let mut specs = Vec::new();
    for entry in menu {
        let sub = cli.find_subcommand(&entry.name);
        let requires_args = match sub {
            Some(cmd) => cmd.get_positionals().any(|arg| arg.is_required_set()),
            None => matches!(entry.name.as_str(), "source" | "backend"),
        };
        let flags = sub.map_or_else(Vec::new, |cmd| {
            cmd.get_arguments()
                .filter(|arg| !arg.is_positional() && !arg.is_hide_set())
                .filter_map(|arg| {
                    let long = arg.get_long()?;
                    Some(FlagSpec {
                        flag: format!("--{long}"),
                        help: arg
                            .get_help()
                            .map(|h| h.to_string().lines().next().unwrap_or("").to_string())
                            .unwrap_or_default(),
                    })
                })
                .collect()
        });
        specs.push(CommandSpec {
            name: entry.name,
            help: entry.help,
            unavailable: entry.unavailable,
            requires_args,
            is_tui_action: false,
            flags,
        });
    }

    let tui_actions = [
        (
            "fullscreen",
            "Switch to flicker-free fullscreen mode (Alt+F)",
        ),
        ("inline", "Switch to inline terminal stream mode (Alt+F)"),
        ("clear", "Clear transcript and redraw (Ctrl+L)"),
        ("copy", "Copy latest command output to clipboard (OSC 52)"),
        ("mouse", "Toggle mouse scroll capture in fullscreen mode"),
    ];
    for (name, help) in tui_actions {
        specs.push(CommandSpec {
            name: name.to_string(),
            help: help.to_string(),
            unavailable: None,
            requires_args: false,
            is_tui_action: true,
            flags: Vec::new(),
        });
    }
    specs.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    specs
}

/// Inline dimmed ghost completion suffix for the first word of the active command line.
pub(super) fn ghost_suffix(text: &str, cursor: usize, specs: &[CommandSpec]) -> Option<String> {
    let clamped = cursor.min(text.len());
    let line_start = text[..clamped].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[clamped..]
        .find('\n')
        .map_or(text.len(), |i| clamped + i);
    if clamped != line_end {
        return None;
    }
    let line = &text[line_start..clamped];
    if line.is_empty()
        || line.contains(char::is_whitespace)
        || line.starts_with('/')
        || line.starts_with('@')
    {
        return None;
    }
    specs
        .iter()
        .filter(|spec| spec.unavailable.is_none())
        .find(|spec| spec.name.starts_with(line) && spec.name.len() > line.len())
        .map(|spec| spec.name[line.len()..].to_string())
}

/// Inline dimmed ghost completion suffix in FDX composer mode (`:cmd`, `!cmd`, target words).
pub(super) fn fdx_ghost_suffix(
    text: &str,
    cursor: usize,
    specs: &[CommandSpec],
    target_words: &[String],
) -> Option<String> {
    let clamped = cursor.min(text.len());
    if clamped != text.len() || text.is_empty() || text.contains('\n') {
        return None;
    }
    if let Some(escaped) = text.strip_prefix('!') {
        return ghost_suffix(escaped, escaped.len(), specs);
    }
    if text.starts_with(':') && !text.contains(' ') {
        return [":help", ":last", ":q", ":send", ":to"]
            .into_iter()
            .find(|cmd| cmd.starts_with(text) && cmd.len() > text.len())
            .map(|cmd| cmd[text.len()..].to_string());
    }
    if !text.contains(' ') && !text.starts_with('@') {
        return target_words
            .iter()
            .map(String::as_str)
            .chain(["help", "lab", "ping", "run"])
            .find(|w| w.starts_with(text) && w.len() > text.len())
            .map(|w| w[text.len()..].to_string());
    }
    None
}

/// Live command palette candidates for the first word (works with or without a
/// leading `/`; stays closed on empty input, `@target` FDX input, or once
/// whitespace starts arguments).
pub(super) fn slash_candidates(text: &str, specs: &[CommandSpec]) -> Vec<Candidate> {
    if text.is_empty() || text.starts_with('@') || text.contains(char::is_whitespace) {
        return Vec::new();
    }
    let has_slash = text.starts_with('/');
    let query = text.strip_prefix('/').unwrap_or(text);
    let needle = query.to_lowercase();
    let mut prefix_matches = Vec::new();
    let mut fuzzy_matches = Vec::new();
    for spec in specs {
        let lower = spec.name.to_lowercase();
        let item = Candidate {
            insert: spec.name.clone(),
            label: if has_slash {
                format!("/{}", spec.name)
            } else {
                spec.name.clone()
            },
            detail: spec.help.clone(),
            unavailable: spec.unavailable,
            run_on_enter: !spec.requires_args && spec.unavailable.is_none(),
            replace_start: 0,
            replace_end: text.len(),
            add_space: spec.requires_args,
        };
        if lower.starts_with(&needle) {
            prefix_matches.push(item);
        } else if !needle.is_empty() && fuzzy_match(&lower, &needle) {
            fuzzy_matches.push(item);
        }
    }
    prefix_matches.extend(fuzzy_matches);
    prefix_matches
}

fn default_image_names(image_count: usize) -> Vec<String> {
    if image_count == 0 {
        vec!["el2".into(), "guest0".into(), "guest1".into()]
    } else {
        (0..image_count)
            .map(|i| rustvisor_debug::fdx::image_name(i.checked_sub(1)))
            .collect()
    }
}

/// Tab completion candidates for normal command mode (commands, flags, `@` FDX, ELF symbols, paths, values).
#[allow(clippy::too_many_arguments)]
pub(super) fn command_tab_candidates(
    text: &str,
    cursor: usize,
    specs: &[CommandSpec],
    symbols: &SymbolCache,
    link_names: &[String],
    target_words: &[String],
    image_count: usize,
    root: &Path,
    cwd: &Path,
) -> Vec<Candidate> {
    let clamped = cursor.min(text.len());
    let line_start = text[..clamped].rfind('\n').map_or(0, |i| i + 1);
    let before = &text[line_start..clamped];
    if line_start == 0 && before.starts_with('/') {
        return slash_candidates(before, specs);
    }
    let effective_links = if link_names.is_empty() {
        default_image_names(image_count)
    } else {
        link_names.to_vec()
    };
    if before.starts_with('@') {
        let mut items = fdx_tab_candidates(
            before,
            before.len(),
            specs,
            symbols,
            &effective_links,
            target_words,
            image_count,
            root,
            cwd,
        );
        for item in &mut items {
            item.replace_start += line_start;
            item.replace_end += line_start;
        }
        return items;
    }

    let word_start_rel = before.rfind(char::is_whitespace).map_or(0, |idx| idx + 1);
    let word = &before[word_start_rel..];
    let head = before[..word_start_rel].trim();
    let word_start = line_start + word_start_rel;
    let abs_cursor = clamped;

    if head.is_empty() {
        return specs
            .iter()
            .filter(|s| s.name.starts_with(word))
            .map(|s| Candidate {
                insert: s.name.clone(),
                label: s.name.clone(),
                detail: s.help.clone(),
                unavailable: s.unavailable,
                run_on_enter: line_start == 0 && !s.requires_args && s.unavailable.is_none(),
                replace_start: word_start,
                replace_end: abs_cursor,
                add_space: s.requires_args,
            })
            .collect();
    }

    let tokens: Vec<&str> = head.split_whitespace().collect();
    let Some(&cmd) = tokens.first() else {
        return Vec::new();
    };
    let prev = tokens.last().copied().unwrap_or("");

    // Helper to collect unused `--flags` for the active subcommand.
    let flag_candidates = |prefix: &str| -> Vec<Candidate> {
        let Some(spec) = specs.iter().find(|s| s.name == cmd) else {
            return Vec::new();
        };
        spec.flags
            .iter()
            .filter(|f| f.flag.starts_with(prefix) && !tokens.contains(&f.flag.as_str()))
            .map(|f| Candidate {
                insert: f.flag.clone(),
                label: f.flag.clone(),
                detail: f.help.clone(),
                unavailable: None,
                run_on_enter: false,
                replace_start: word_start,
                replace_end: abs_cursor,
                add_space: true,
            })
            .collect()
    };

    // Complete `--flags` for the active subcommand.
    if word.starts_with('-') {
        return flag_candidates(word);
    }

    let mut out = match cmd {
        "backend" => ["trace32", "openocd"]
            .into_iter()
            .filter(|v| v.starts_with(word))
            .map(|v| Candidate {
                insert: v.to_string(),
                label: v.to_string(),
                detail: if v == "trace32" {
                    "Lauterbach TRACE32 PowerView".into()
                } else {
                    "StellarLINK via openocd-stellar".into()
                },
                unavailable: None,
                run_on_enter: false,
                replace_start: word_start,
                replace_end: abs_cursor,
                add_space: false,
            })
            .collect(),
        "help" => specs
            .iter()
            .filter(|s| !s.is_tui_action && s.name.starts_with(word))
            .map(|s| Candidate {
                insert: s.name.clone(),
                label: s.name.clone(),
                detail: s.help.clone(),
                unavailable: None,
                run_on_enter: false,
                replace_start: word_start,
                replace_end: abs_cursor,
                add_space: false,
            })
            .collect(),
        "swtrace" if prev == "--from" => {
            let mut items = Vec::new();
            if "latest".starts_with(word) {
                items.push(Candidate {
                    insert: "latest".into(),
                    label: "latest".into(),
                    detail: "Auto-saved .rvdbg/swtrace/latest.csv".into(),
                    unavailable: None,
                    run_on_enter: false,
                    replace_start: word_start,
                    replace_end: abs_cursor,
                    add_space: true,
                });
            }
            items.extend(path_candidates(word, word_start, abs_cursor, cwd, false));
            items
        }
        "fdx" | "swtrace" if prev == "--guest" => effective_links
            .into_iter()
            .chain(["0..2".into(), "guests".into(), "all".into()])
            .filter(|name| name.starts_with(word))
            .map(|name| Candidate {
                insert: name.clone(),
                label: name,
                detail: "Image link / trace target".into(),
                unavailable: None,
                run_on_enter: false,
                replace_start: word_start,
                replace_end: abs_cursor,
                add_space: true,
            })
            .collect(),
        "ram" if prev == "=" => ["GOGO", "DONE", "0x0"]
            .into_iter()
            .filter(|v| v.starts_with(word))
            .map(|v| Candidate {
                insert: v.to_string(),
                label: v.to_string(),
                detail: if v == "GOGO" {
                    "4-byte ASCII start tag (0x474F474F)".into()
                } else if v == "DONE" {
                    "4-byte ASCII done tag (0x444F4E45)".into()
                } else {
                    "32-bit word value".into()
                },
                unavailable: None,
                run_on_enter: false,
                replace_start: word_start,
                replace_end: abs_cursor,
                add_space: false,
            })
            .collect(),
        "ram" if tokens.len() >= 2 && !prev.starts_with('-') => {
            let mut items = Vec::new();
            if "=".starts_with(word) {
                items.push(Candidate {
                    insert: "=".into(),
                    label: "= <VALUE>".into(),
                    detail: "Write 32-bit word(s) or 4-byte ASCII tag (e.g. GOGO)".into(),
                    unavailable: None,
                    run_on_enter: false,
                    replace_start: word_start,
                    replace_end: abs_cursor,
                    add_space: true,
                });
            }
            items.extend(flag_candidates(word));
            items
        }
        "mem" | "ram" | "run-until" => {
            if let Some((scope, sym_prefix)) = word.split_once(':') {
                let matches = if scope.is_empty() {
                    symbols.matches(sym_prefix, 32)
                } else {
                    symbols.scoped_matches(scope, sym_prefix, 32)
                };
                return matches
                    .into_iter()
                    .map(|(name, addr)| {
                        let full = format!("{scope}:{name}");
                        Candidate {
                            insert: full.clone(),
                            label: full,
                            detail: format!("0x{addr:08X}"),
                            unavailable: None,
                            run_on_enter: false,
                            replace_start: word_start,
                            replace_end: abs_cursor,
                            add_space: true,
                        }
                    })
                    .collect();
            }
            let mut out: Vec<Candidate> = symbols
                .matches(word, 32)
                .into_iter()
                .map(|(name, addr)| Candidate {
                    insert: name.to_string(),
                    label: name.to_string(),
                    detail: format!("0x{addr:08X}"),
                    unavailable: None,
                    run_on_enter: false,
                    replace_start: word_start,
                    replace_end: abs_cursor,
                    add_space: true,
                })
                .collect();
            if matches!(cmd, "ram" | "mem") {
                for name in default_image_names(image_count) {
                    let prefix = format!("{name}:");
                    if prefix.starts_with(word) {
                        out.push(Candidate {
                            insert: prefix.clone(),
                            label: prefix,
                            detail: format!("Scope symbol to {name}"),
                            unavailable: None,
                            run_on_enter: false,
                            replace_start: word_start,
                            replace_end: abs_cursor,
                            add_space: false,
                        });
                    }
                }
            }
            if cmd == "mem" {
                for (prefix, detail) in [
                    ("AD:0x", "Physical RAM / MMIO address"),
                    ("C15:0x", "AArch32 CP15 system register"),
                ] {
                    if prefix.starts_with(word) {
                        out.push(Candidate {
                            insert: prefix.into(),
                            label: prefix.into(),
                            detail: detail.into(),
                            unavailable: None,
                            run_on_enter: false,
                            replace_start: word_start,
                            replace_end: abs_cursor,
                            add_space: false,
                        });
                    }
                }
            }
            out
        }
        "reg" => [
            ("PC", "Program counter"),
            ("CPSR", "Current program status register"),
            ("SP", "Stack pointer"),
            ("LR", "Link register"),
            ("SCTLR", "System control register (EL1)"),
            ("HSCTLR", "Hyp system control register (EL2)"),
            ("HACTLR", "Hyp auxiliary control register"),
            ("HCR", "Hyp configuration register"),
            ("VSCTLR", "Virtualization system control (VMID)"),
            ("ELR_HYP", "Exception link register (EL2)"),
            ("SPSR_HYP", "Saved program status (EL2)"),
            ("HSR", "Hyp syndrome register"),
            ("HDFAR", "Hyp data fault address register"),
            ("HIFAR", "Hyp instruction fault address register"),
        ]
        .into_iter()
        .filter(|(reg, _)| {
            reg.to_ascii_lowercase()
                .starts_with(&word.to_ascii_lowercase())
        })
        .map(|(reg, help)| Candidate {
            insert: reg.into(),
            label: reg.into(),
            detail: help.into(),
            unavailable: None,
            run_on_enter: false,
            replace_start: word_start,
            replace_end: abs_cursor,
            add_space: true,
        })
        .collect(),
        "clock" => {
            let mut items = Vec::new();
            if prev == "--tree" {
                items.extend(path_candidates(word, word_start, abs_cursor, cwd, false));
            } else if "XOSC=40MHz"
                .to_ascii_lowercase()
                .starts_with(&word.to_ascii_lowercase())
                && !tokens.iter().any(|t| t.starts_with("XOSC="))
            {
                items.push(Candidate {
                    insert: "XOSC=40MHz".into(),
                    label: "XOSC=40MHz".into(),
                    detail: "Board external oscillator frequency".into(),
                    unavailable: None,
                    run_on_enter: false,
                    replace_start: word_start,
                    replace_end: abs_cursor,
                    add_space: true,
                });
            }
            items
        }
        "copy" => [
            ("1", "Copy latest command output"),
            ("2", "Copy 2nd latest command output"),
        ]
        .into_iter()
        .filter(|(n, _)| n.starts_with(word))
        .map(|(n, help)| Candidate {
            insert: n.into(),
            label: n.into(),
            detail: help.into(),
            unavailable: None,
            run_on_enter: false,
            replace_start: word_start,
            replace_end: abs_cursor,
            add_space: false,
        })
        .collect(),
        "images" => {
            let mut out = Vec::new();
            let discovered = config::discover_image_dirs(root, cwd);
            if "latest".starts_with(word) && !discovered.is_empty() {
                out.push(Candidate {
                    insert: "latest".into(),
                    label: "latest".into(),
                    detail: "Newest built image directory".into(),
                    unavailable: None,
                    run_on_enter: false,
                    replace_start: word_start,
                    replace_end: abs_cursor,
                    add_space: false,
                });
            }
            for item in discovered {
                let rel = display_image_dir(&item.dir, root, cwd);
                let leaf = item.dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if rel.starts_with(word) || leaf.starts_with(word) {
                    let insert = if rel.contains(char::is_whitespace) {
                        format!("'{rel}'")
                    } else {
                        rel.clone()
                    };
                    out.push(Candidate {
                        insert,
                        label: rel,
                        detail: format!("{} ELF(s){}", item.count, format_age(item.modified)),
                        unavailable: None,
                        run_on_enter: false,
                        replace_start: word_start,
                        replace_end: abs_cursor,
                        add_space: false,
                    });
                }
            }
            out.extend(path_candidates(word, word_start, abs_cursor, cwd, true));
            out
        }
        "source" | "check" | "do" | "verify" => {
            path_candidates(word, word_start, abs_cursor, cwd, false)
        }
        _ => Vec::new(),
    };

    // After pressing Space (`word.is_empty()`), surface the command's available
    // `--flags` as well (like PowerView's next-token suggestions).
    if word.is_empty() && !matches!(cmd, "ram" if prev == "=") {
        for flag in flag_candidates("") {
            if !out.iter().any(|existing| existing.insert == flag.insert) {
                out.push(flag);
            }
        }
    }
    out
}

/// Completion & live suggestion candidates for FDX mode (`@link`, `:to`, `:cmd`, `!rvdbg`, target words).
#[allow(clippy::too_many_arguments)]
pub(super) fn fdx_tab_candidates(
    text: &str,
    cursor: usize,
    specs: &[CommandSpec],
    symbols: &SymbolCache,
    link_names: &[String],
    target_words: &[String],
    image_count: usize,
    root: &Path,
    cwd: &Path,
) -> Vec<Candidate> {
    let clamped = cursor.min(text.len());
    let line_start = text[..clamped].rfind('\n').map_or(0, |i| i + 1);
    let before = &text[line_start..clamped];
    let effective_links = if link_names.is_empty() {
        default_image_names(image_count)
    } else {
        link_names.to_vec()
    };

    // `!cmd ...` escapes to full rvdbg command + argument completion without leaving FDX mode.
    if let Some(escaped) = before.strip_prefix('!') {
        let mut items = if !escaped.is_empty()
            && !escaped.starts_with('@')
            && !escaped.contains(char::is_whitespace)
        {
            slash_candidates(escaped, specs)
        } else {
            command_tab_candidates(
                escaped,
                escaped.len(),
                specs,
                symbols,
                &effective_links,
                target_words,
                image_count,
                root,
                cwd,
            )
        };
        for item in &mut items {
            item.run_on_enter = false;
            item.replace_start += line_start + 1;
            item.replace_end += line_start + 1;
        }
        return items;
    }

    let word_start_rel = before.rfind(' ').map_or(0, |idx| idx + 1);
    let word = &before[word_start_rel..];
    let word_start = line_start + word_start_rel;

    if word_start_rel == 0 && word.starts_with('@') {
        let mut items: Vec<(String, String)> = effective_links
            .iter()
            .map(|name| (format!("@{name}"), format!("Send packet to {name}")))
            .collect();
        if word.len() >= 2 && word.as_bytes()[1].is_ascii_digit() {
            for idx in 0..effective_links.len().saturating_sub(1) {
                items.push((format!("@{idx}"), format!("Short selector for guest{idx}")));
            }
        }
        if effective_links.len() > 2 {
            if effective_links.len() >= 4 {
                items.push(("@0..2".into(), "Send to range guest0..guest2".into()));
            }
            items.push(("@guests".into(), "Broadcast to every guest link".into()));
            items.push(("@all".into(), "Broadcast to EL2 + every guest".into()));
        }
        return items
            .into_iter()
            .filter(|(cand, _)| cand.starts_with(word))
            .map(|(cand, detail)| Candidate {
                insert: cand.clone(),
                label: cand,
                detail,
                unavailable: None,
                run_on_enter: false,
                replace_start: line_start,
                replace_end: clamped,
                add_space: true,
            })
            .collect();
    }
    if before.starts_with(":to ") && word_start_rel == 4 {
        return effective_links
            .iter()
            .filter(|name| name.starts_with(word))
            .map(|name| Candidate {
                insert: name.clone(),
                label: name.clone(),
                detail: "Switch active FDX link".into(),
                unavailable: None,
                run_on_enter: false,
                replace_start: word_start,
                replace_end: clamped,
                add_space: false,
            })
            .collect();
    }
    let local_cmds = [
        (":q", "Close FDX panel and return to rvdbg"),
        (":to", "Switch active FDX link (:to <name>)"),
        (":last", "Show recent packets on this link"),
        (":send", "Send a literal packet bypassing command guard"),
        (":help", "Show local FDX commands"),
    ];
    if word_start_rel == 0 && word.starts_with(':') {
        return local_cmds
            .into_iter()
            .filter(|(cmd, _)| cmd.starts_with(word))
            .map(|(cmd, help)| Candidate {
                insert: cmd.to_string(),
                label: cmd.to_string(),
                detail: help.to_string(),
                unavailable: None,
                run_on_enter: false,
                replace_start: line_start,
                replace_end: clamped,
                add_space: matches!(cmd, ":to" | ":send"),
            })
            .collect();
    }

    let mut out = Vec::new();
    // Pressing Tab on an empty FDX line shows targets, local `:commands`, `!cmd`, and target words.
    if before.is_empty() {
        for name in &effective_links {
            let at = format!("@{name}");
            out.push(Candidate {
                insert: at.clone(),
                label: at,
                detail: format!("Send packet to {name}"),
                unavailable: None,
                run_on_enter: false,
                replace_start: line_start,
                replace_end: clamped,
                add_space: true,
            });
        }
        for (cmd, help) in local_cmds {
            out.push(Candidate {
                insert: cmd.into(),
                label: cmd.into(),
                detail: help.into(),
                unavailable: None,
                run_on_enter: false,
                replace_start: line_start,
                replace_end: clamped,
                add_space: matches!(cmd, ":to" | ":send"),
            });
        }
        out.push(Candidate {
            insert: "!".into(),
            label: "!<cmd>".into(),
            detail: "Run an rvdbg command without leaving FDX".into(),
            unavailable: None,
            run_on_enter: false,
            replace_start: line_start,
            replace_end: clamped,
            add_space: false,
        });
    }

    let mut words: Vec<(&str, &str)> = target_words
        .iter()
        .map(|w| (w.as_str(), "Target FDX word"))
        .collect();
    for builtin in [
        ("help", "List commands supported by this target"),
        ("ping", "Ping target FDX agent"),
        ("run", "Start a guest scenario (e.g. run timer)"),
        ("lab", "Configure EL2 lab scenario"),
    ] {
        if !words.iter().any(|(w, _)| *w == builtin.0) {
            words.push(builtin);
        }
    }
    words.sort_unstable_by(|a, b| a.0.cmp(b.0));
    for (w, detail) in words {
        if w.starts_with(word) {
            out.push(Candidate {
                insert: w.to_string(),
                label: w.to_string(),
                detail: detail.to_string(),
                unavailable: None,
                run_on_enter: false,
                replace_start: word_start,
                replace_end: clamped,
                add_space: true,
            });
        }
    }
    out
}

pub(super) fn backend_picker(current: Option<Backend>) -> Picker {
    let mark = |b: Option<Backend>, label: &str| {
        if current == b {
            format!("{label} (current)")
        } else {
            label.to_string()
        }
    };
    Picker {
        title: "Select Backend".into(),
        items: vec![
            PickerItem {
                label: mark(Some(Backend::Trace32), "trace32"),
                detail: "Lauterbach TRACE32 PowerView".into(),
                command: "backend trace32".into(),
            },
            PickerItem {
                label: mark(Some(Backend::OpenOcd), "openocd"),
                detail: "StellarLINK via openocd-stellar".into(),
                command: "backend openocd".into(),
            },
            PickerItem {
                label: "reconnect".into(),
                detail: "Auto-detect connected probe and reconnect".into(),
                command: "reconnect".into(),
            },
        ],
        selected: 0,
    }
}

pub(super) fn fdx_picker(link_names: &[String], active: bool) -> Picker {
    let mut items = vec![PickerItem {
        label: "fdx --all".into(),
        detail: "Watch every image's FDX link at once".into(),
        command: "fdx --all".into(),
    }];
    if link_names.is_empty() {
        items.push(PickerItem {
            label: "fdx".into(),
            detail: "Watch EL2 (hypervisor) FDX link".into(),
            command: "fdx".into(),
        });
    } else {
        for name in link_names {
            let cmd = if name == "el2" {
                "fdx".to_string()
            } else {
                format!("fdx --guest {name}")
            };
            items.push(PickerItem {
                label: name.clone(),
                detail: format!("Watch {name} FDX link"),
                command: cmd,
            });
        }
    }
    if active {
        items.push(PickerItem {
            label: "close".into(),
            detail: "Close the FDX panel".into(),
            command: "/fdx-close".into(),
        });
    }
    Picker {
        title: "FDX Links".into(),
        items,
        selected: 0,
    }
}

fn display_image_dir(dir: &Path, root: &Path, cwd: &Path) -> String {
    dir.strip_prefix(cwd)
        .or_else(|_| dir.strip_prefix(root))
        .unwrap_or(dir)
        .display()
        .to_string()
}

fn format_age(modified: Option<SystemTime>) -> String {
    let Some(mtime) = modified else {
        return String::new();
    };
    let Ok(elapsed) = SystemTime::now().duration_since(mtime) else {
        return String::new();
    };
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!(" · {secs}s ago")
    } else if secs < 3600 {
        format!(" · {}m ago", secs / 60)
    } else if secs < 86400 {
        format!(" · {}h ago", secs / 3600)
    } else {
        format!(" · {}d ago", secs / 86400)
    }
}

pub(super) fn images_picker(config: &Config, cwd: &Path) -> Picker {
    let found = config::discover_image_dirs(&config.root, cwd);
    let mut items: Vec<PickerItem> = found
        .into_iter()
        .take(24)
        .map(|item| {
            let display = display_image_dir(&item.dir, &config.root, cwd);
            let quoted = if display.contains(char::is_whitespace) {
                format!("images '{display}'")
            } else {
                format!("images {display}")
            };
            PickerItem {
                label: display,
                detail: format!("{} ELF image(s){}", item.count, format_age(item.modified)),
                command: quoted,
            }
        })
        .collect();
    if items.is_empty() {
        items.push(PickerItem {
            label: "images".into(),
            detail: "Show currently configured images".into(),
            command: "images".into(),
        });
    }
    Picker {
        title: "Switch Image Directory".into(),
        items,
        selected: 0,
    }
}

fn path_candidates(
    word: &str,
    word_start: usize,
    cursor: usize,
    cwd: &Path,
    dirs_only: bool,
) -> Vec<Candidate> {
    let (dir_part, file_prefix) = match word.rfind('/') {
        Some(idx) => (&word[..=idx], &word[idx + 1..]),
        None => ("", word),
    };
    let search_dir = if dir_part.is_empty() {
        cwd.to_path_buf()
    } else {
        cwd.join(dir_part)
    };
    let Ok(entries) = std::fs::read_dir(&search_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(file_prefix) {
            continue;
        }
        if !file_prefix.starts_with('.') && name.starts_with('.') {
            continue;
        }
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if dirs_only && !is_dir {
            continue;
        }
        let suffix = if is_dir { "/" } else { "" };
        let full = format!("{dir_part}{name}{suffix}");
        out.push(Candidate {
            insert: full.clone(),
            label: full,
            detail: if is_dir { "directory" } else { "file" }.into(),
            unavailable: None,
            run_on_enter: false,
            replace_start: word_start,
            replace_end: cursor,
            add_space: !is_dir,
        });
    }
    out.sort_unstable_by(|a, b| a.label.cmp(&b.label));
    out.truncate(24);
    out
}

fn fuzzy_match(haystack: &str, needle: &str) -> bool {
    let mut chars = needle.chars();
    let Some(mut target) = chars.next() else {
        return true;
    };
    for ch in haystack.chars() {
        if ch == target {
            match chars.next() {
                Some(next) => target = next,
                None => return true,
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_menu_runs_zero_arg_commands_and_fills_arg_commands() {
        let specs = build_specs(Some(Backend::OpenOcd));
        let stat = slash_candidates("/stat", &specs);
        assert!(!stat.is_empty());
        assert_eq!(stat[0].insert, "status");
        assert!(stat[0].run_on_enter);

        let mem = slash_candidates("/mem", &specs);
        assert_eq!(mem.len(), 1);
        assert_eq!(mem[0].insert, "mem");
        assert!(!mem[0].run_on_enter);
        assert!(mem[0].add_space);

        // Bare first word (no leading `/`) opens the exact same command palette.
        let bare = slash_candidates("stat", &specs);
        assert!(!bare.is_empty());
        assert_eq!(bare[0].label, "status");
        assert_eq!(bare[0].insert, "status");
        assert!(bare[0].run_on_enter);
        assert!(slash_candidates("@el2", &specs).is_empty());
        assert!(slash_candidates("", &specs).is_empty());
    }

    #[test]
    fn ghost_text_suggests_available_command() {
        let specs = build_specs(Some(Backend::OpenOcd));
        assert_eq!(ghost_suffix("sta", 3, &specs).as_deref(), Some("tus"));
        assert_eq!(ghost_suffix("status", 6, &specs), None);
        assert_eq!(ghost_suffix("/sta", 4, &specs), None);
    }

    #[test]
    fn flag_completion_extracts_clap_flags() {
        let specs = build_specs(Some(Backend::OpenOcd));
        let syms = SymbolCache::default();
        let items = command_tab_candidates(
            "flash --no-",
            11,
            &specs,
            &syms,
            &[],
            &[],
            1,
            Path::new("."),
            Path::new("."),
        );
        let names: Vec<&str> = items.iter().map(|c| c.label.as_str()).collect();
        assert!(names.contains(&"--no-verify"), "{names:?}");
        assert!(names.contains(&"--no-run"), "{names:?}");
    }

    #[test]
    fn multiline_ghost_and_tab_complete_work_on_later_lines() {
        let specs = build_specs(Some(Backend::OpenOcd));
        let syms = SymbolCache::default();
        let text = "reset\nsta";
        assert_eq!(
            ghost_suffix(text, text.len(), &specs).as_deref(),
            Some("tus")
        );
        let items = command_tab_candidates(
            "reset\nsta",
            9,
            &specs,
            &syms,
            &[],
            &[],
            2,
            Path::new("."),
            Path::new("."),
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].insert, "status");
        assert_eq!((items[0].replace_start, items[0].replace_end), (6, 9));
        let at_items = command_tab_candidates(
            "go\n@g",
            5,
            &specs,
            &syms,
            &[],
            &[],
            3,
            Path::new("."),
            Path::new("."),
        );
        let labels: Vec<&str> = at_items.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["@guest0", "@guest1", "@guests"]);
        assert_eq!((at_items[0].replace_start, at_items[0].replace_end), (3, 5));
    }

    #[test]
    fn post_space_and_fdx_suggestions_cover_flags_registers_and_escape() {
        let specs = build_specs(Some(Backend::Trace32));
        let syms = SymbolCache::default();

        // Post-space on `swtrace ` immediately surfaces unused `--flags`.
        let sw = command_tab_candidates(
            "swtrace ",
            8,
            &specs,
            &syms,
            &[],
            &[],
            6,
            Path::new("."),
            Path::new("."),
        );
        let sw_labels: Vec<&str> = sw.iter().map(|c| c.insert.as_str()).collect();
        assert!(sw_labels.contains(&"--html") && sw_labels.contains(&"--from"));

        // `swtrace --from ` suggests `latest`.
        let from = command_tab_candidates(
            "swtrace --from ",
            15,
            &specs,
            &syms,
            &[],
            &[],
            6,
            Path::new("."),
            Path::new("."),
        );
        assert_eq!(from[0].insert, "latest");

        // `reg ` suggests R52 / EL2 registers.
        let regs = command_tab_candidates(
            "reg HS",
            6,
            &specs,
            &syms,
            &[],
            &[],
            1,
            Path::new("."),
            Path::new("."),
        );
        let reg_names: Vec<&str> = regs.iter().map(|c| c.insert.as_str()).collect();
        assert!(reg_names.contains(&"HSCTLR") && reg_names.contains(&"HSR"));

        // FDX `!` escape reuses full rvdbg command + argument suggestions.
        let esc = fdx_tab_candidates(
            "!reset ",
            7,
            &specs,
            &syms,
            &["el2".into(), "guest0".into()],
            &[],
            2,
            Path::new("."),
            Path::new("."),
        );
        let esc_names: Vec<&str> = esc.iter().map(|c| c.insert.as_str()).collect();
        assert!(esc_names.contains(&"--go"));
        assert_eq!(esc[0].replace_start, 7);
        assert_eq!(
            fdx_ghost_suffix("!sta", 4, &specs, &[]).as_deref(),
            Some("tus")
        );
        assert_eq!(
            fdx_ghost_suffix(":h", 2, &specs, &[]).as_deref(),
            Some("elp")
        );
    }
}
