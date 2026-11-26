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
        .filter(|spec| !spec.is_tui_action && spec.unavailable.is_none())
        .find(|spec| spec.name.starts_with(line) && spec.name.len() > line.len())
        .map(|spec| spec.name[line.len()..].to_string())
}

/// Slash menu candidates when text begins with `/` (no spaces yet).
pub(super) fn slash_candidates(text: &str, specs: &[CommandSpec]) -> Vec<Candidate> {
    let Some(query) = text
        .strip_prefix('/')
        .filter(|s| !s.contains(char::is_whitespace))
    else {
        return Vec::new();
    };
    let needle = query.to_lowercase();
    let mut prefix_matches = Vec::new();
    let mut fuzzy_matches = Vec::new();
    for spec in specs {
        let lower = spec.name.to_lowercase();
        let item = Candidate {
            insert: if spec.is_tui_action {
                format!("/{}", spec.name)
            } else {
                spec.name.clone()
            },
            label: format!("/{}", spec.name),
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
        let mut items = fdx_tab_candidates(before, before.len(), &effective_links, target_words);
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
            .filter(|s| !s.is_tui_action && s.name.starts_with(word))
            .map(|s| Candidate {
                insert: s.name.clone(),
                label: s.name.clone(),
                detail: s.help.clone(),
                unavailable: s.unavailable,
                run_on_enter: false,
                replace_start: word_start,
                replace_end: abs_cursor,
                add_space: true,
            })
            .collect();
    }

    let tokens: Vec<&str> = head.split_whitespace().collect();
    let Some(&cmd) = tokens.first() else {
        return Vec::new();
    };
    let prev = tokens.last().copied().unwrap_or("");

    // Complete `--flags` for the active subcommand.
    if word.starts_with('-') {
        let Some(spec) = specs.iter().find(|s| s.name == cmd) else {
            return Vec::new();
        };
        return spec
            .flags
            .iter()
            .filter(|f| f.flag.starts_with(word))
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
            .collect();
    }

    match cmd {
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
        "fdx" | "swtrace" if prev == "--guest" => effective_links
            .into_iter()
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
            out
        }
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
        "source" | "check" | "do" => path_candidates(word, word_start, abs_cursor, cwd, false),
        _ => Vec::new(),
    }
}

/// Tab completion candidates for FDX input mode (`@link`, `:to`, `:cmd`, target `help` words).
pub(super) fn fdx_tab_candidates(
    text: &str,
    cursor: usize,
    link_names: &[String],
    target_words: &[String],
) -> Vec<Candidate> {
    let clamped = cursor.min(text.len());
    let line_start = text[..clamped].rfind('\n').map_or(0, |i| i + 1);
    let before = &text[line_start..clamped];
    let word_start_rel = before.rfind(' ').map_or(0, |idx| idx + 1);
    let word = &before[word_start_rel..];
    let word_start = line_start + word_start_rel;
    if word_start_rel == 0 && word.starts_with('@') {
        return link_names
            .iter()
            .map(|name| format!("@{name}"))
            .filter(|candidate| candidate.starts_with(word))
            .map(|candidate| Candidate {
                insert: candidate.clone(),
                label: candidate,
                detail: "Send one packet to this link".into(),
                unavailable: None,
                run_on_enter: false,
                replace_start: line_start,
                replace_end: clamped,
                add_space: true,
            })
            .collect();
    }
    if before.starts_with(":to ") && word_start_rel == 4 {
        return link_names
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
    if word_start_rel == 0 && word.starts_with(':') {
        return [
            (":q", "Close FDX panel and return to rvdbg"),
            (":to", "Switch active FDX link (:to <name>)"),
            (":last", "Show recent packets on this link"),
            (":send", "Send a literal packet to the current link"),
            (":help", "Show local FDX commands"),
        ]
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
    let mut matches: Vec<Candidate> = target_words
        .iter()
        .filter(|w| w.starts_with(word))
        .map(|w| Candidate {
            insert: w.clone(),
            label: w.clone(),
            detail: "Target FDX command".into(),
            unavailable: None,
            run_on_enter: false,
            replace_start: word_start,
            replace_end: clamped,
            add_space: true,
        })
        .collect();
    matches.sort_unstable_by(|a, b| a.label.cmp(&b.label));
    matches.dedup_by(|a, b| a.label == b.label);
    matches
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
        assert_eq!(labels, ["@guest0", "@guest1"]);
        assert_eq!((at_items[0].replace_start, at_items[0].replace_end), (3, 5));
    }
}
