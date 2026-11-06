//! `check <file>`: a data-driven acceptance check. The check file lives in
//! the project; the engine only executes it and knows no board facts.
//! `reg` and `expr` reads are backend-specific (TRACE32: the PER file and
//! PRACTICE) and go through [`Target::prepare_read`].
//!
//! ```toml
//! description = "optional"
//!
//! [[check]]
//! name = "HSCTLR"
//! read = { reg = "HSCTLR" }        # or addr / core / expr
//! expect = { eq = 0x30C5083A }     # eq/ne (+ mask), nonzero, range, in_symbol, one_of
//! variants = ["default"]           # optional
//!
//! [[check]]
//! name = "core clock"
//! read = { clock = "CLUSTER0_CORE0" }  # a clock of `debug clock`'s tree
//! expect = { hz = 400_000_000, source = "SYS_CLK_CGM0" }  # any of state, hz, source, warnings
//! ```

use std::path::Path;

use toml::{Table, Value};

use crate::clock;
use crate::style::Style;
use crate::target::{BackendRead, Error, Result as TargetResult, Target, TargetAddress, hex32};
use crate::text::parse_int_auto;

/// Exit code when a check fails (1 means the command itself could not run,
/// 2 is a usage error).
pub const EXIT_FAILED: i32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckFile {
    pub description: Option<String>,
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub read: ReadSpec,
    pub expect: Expect,
    pub variants: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadSpec {
    /// `PER.VALUE(".<name>")` or a full PER path.
    Reg(String),
    /// `Data.Long(<address>)`.
    Addr(TargetAddress),
    /// `Register(<name>)`.
    Core(String),
    /// Any PRACTICE expression.
    Expr(String),
    /// A clock of the clock tree description (`debug clock`).
    Clock(String),
}

impl ReadSpec {
    /// `(kind, argument)` of a read that only the backend can do.
    pub fn backend_read(&self) -> Option<(&'static str, &str)> {
        match self {
            ReadSpec::Reg(name) => Some(("reg", name)),
            ReadSpec::Expr(expression) => Some(("expr", expression)),
            _ => None,
        }
    }
}

/// A number, or a symbol address plus an offset (`"sym:<name>[+off]"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operand {
    Int(u64),
    Symbol { name: String, offset: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    Eq {
        value: Operand,
        mask: Option<u64>,
    },
    Ne {
        value: Operand,
        mask: Option<u64>,
    },
    NonZero,
    Range(u64, u64),
    InSymbol(String),
    OneOf(Vec<Operand>),
    /// The expectations of a `read.clock` check.
    Clock(ClockExpect),
}

/// A frequency in Hz. Never NaN (checked when parsed), so `Eq` holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hz(pub f64);

impl Eq for Hz {}

/// What a `read.clock` check expects; every part is optional, at least one
/// is given. The same judgement as `debug clock`'s JSON gives: `hz` within
/// half a hertz (and so "on"), `source` the clock it runs from, `warnings`
/// the exact set.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClockExpect {
    /// `state = "on"` (true) or `"off"` (false).
    pub on: Option<bool>,
    pub hz: Option<Hz>,
    pub source: Option<String>,
    pub warnings: Option<Vec<String>>,
}

impl ClockExpect {
    fn describe(&self) -> String {
        let mut parts = Vec::new();
        match (self.hz, self.on) {
            (Some(Hz(hz)), _) => parts.push(clock::format_frequency(hz)),
            (None, Some(true)) => parts.push("on".to_string()),
            (None, Some(false)) => parts.push("off".to_string()),
            (None, None) => {}
        }
        if let Some(source) = &self.source {
            parts.push(format!("from {source}"));
        }
        match &self.warnings {
            Some(warnings) if warnings.is_empty() => parts.push("no warnings".to_string()),
            Some(warnings) => parts.push(format!("warnings [{}]", warnings.join(", "))),
            None => {}
        }
        parts.join(", ")
    }

    /// What does not match `clock`; empty when it holds.
    fn mismatches(&self, clock: &clock::Evaluated) -> Vec<String> {
        let (state, hz) = clock_state(clock);
        let mut found = Vec::new();
        let want_on = if self.hz.is_some() {
            Some(true)
        } else {
            self.on
        };
        if let Some(on) = want_on {
            let want = if on { "on" } else { "off" };
            if state != want {
                found.push(format!("state {state}, expected {want}"));
            }
        }
        if let Some(Hz(expected)) = self.hz {
            if hz.is_none_or(|hz| (hz - expected).abs() > 0.5) {
                found.push(format!("expected {}", clock::format_frequency(expected)));
            }
        }
        if let Some(source) = &self.source {
            if clock.parent.as_deref() != Some(source.as_str()) {
                found.push(format!("expected from {source}"));
            }
        }
        if let Some(warnings) = &self.warnings {
            let mut expected = warnings.clone();
            let mut actual = clock.warnings.clone();
            expected.sort();
            actual.sort();
            if expected != actual {
                found.push(format!("expected warnings [{}]", warnings.join(", ")));
            }
        }
        found
    }
}

/// The state word and frequency of an evaluated clock.
fn clock_state(clock: &clock::Evaluated) -> (&'static str, Option<f64>) {
    match &clock.state {
        clock::State::Hz(hz) => ("on", Some(*hz)),
        clock::State::Off(_) => ("off", None),
        clock::State::Unknown(_) => ("unknown", None),
        clock::State::Error(_) => ("error", None),
    }
}

/// An evaluated clock as text: `on 400 MHz from PLL1_D0_PHI_HP ! not locked`.
fn describe_clock(clock: &clock::Evaluated) -> String {
    let (state, hz) = clock_state(clock);
    let mut parts = vec![state.to_string()];
    if let Some(hz) = hz {
        parts.push(clock::format_frequency(hz));
    }
    if let Some(parent) = &clock.parent {
        parts.push(format!("from {parent}"));
    }
    if !clock.warnings.is_empty() {
        parts.push(format!("! {}", clock.warnings.join(", ")));
    }
    match &clock.state {
        clock::State::Off(reason) | clock::State::Unknown(reason) => {
            parts.push(format!("({reason})"))
        }
        _ => {}
    }
    parts.join(" ")
}

impl Check {
    /// Checks without `variants` run for every variant; the others only when
    /// `--variant` names one of them.
    pub fn selected(&self, variant: Option<&str>) -> bool {
        match (&self.variants, variant) {
            (None, _) => true,
            (Some(variants), Some(variant)) => variants.iter().any(|v| v == variant),
            (Some(_), None) => false,
        }
    }
}

// ---------------------------------------------------------------- parsing

pub fn load(path: &Path) -> TargetResult<CheckFile> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| Error::new(format!("cannot read {}: {error}", path.display())))?;
    parse(&text).map_err(|error| error.context(path.display()))
}

pub fn parse(text: &str) -> TargetResult<CheckFile> {
    let document: Table = text
        .parse()
        .map_err(|error: toml::de::Error| Error::new(error.message().trim_end()))?;
    for key in document.keys() {
        if key != "description" && key != "check" {
            return Err(Error::new(format!(
                "unknown key '{key}' (expected description and [[check]])"
            )));
        }
    }
    let description = match document.get("description") {
        None => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => return Err(Error::new("description must be a string")),
    };
    let entries = match document.get("check") {
        None => return Err(Error::new("no [[check]] entries")),
        Some(Value::Array(entries)) => entries,
        Some(_) => {
            return Err(Error::new("check must be an array of tables ([[check]])"));
        }
    };
    let mut checks = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let Value::Table(table) = entry else {
            return Err(Error::new(format!("check #{} must be a table", index + 1)));
        };
        let label = match table.get("name") {
            Some(Value::String(name)) => format!("check #{} ({name})", index + 1),
            _ => format!("check #{}", index + 1),
        };
        checks.push(parse_check(table).map_err(|error| error.context(&label))?);
    }
    Ok(CheckFile {
        description,
        checks,
    })
}

fn parse_check(table: &Table) -> TargetResult<Check> {
    for key in table.keys() {
        if !matches!(key.as_str(), "name" | "read" | "expect" | "variants") {
            return Err(Error::new(format!("unknown key '{key}'")));
        }
    }
    let name = match table.get("name") {
        Some(Value::String(name)) if !name.is_empty() => name.clone(),
        Some(_) => return Err(Error::new("name must be a non-empty string")),
        None => return Err(Error::new("name is missing")),
    };
    let read = match table.get("read") {
        Some(Value::Table(read)) => parse_read(read)?,
        Some(_) => return Err(Error::new("read must be a table")),
        None => return Err(Error::new("read is missing")),
    };
    let expect = match table.get("expect") {
        Some(Value::Table(expect)) if matches!(read, ReadSpec::Clock(_)) => {
            Expect::Clock(parse_clock_expect(expect)?)
        }
        Some(Value::Table(expect)) => parse_expect(expect)?,
        Some(_) => return Err(Error::new("expect must be a table")),
        None => return Err(Error::new("expect is missing")),
    };
    let variants = match table.get("variants") {
        None => None,
        Some(Value::Array(items)) => Some(
            items
                .iter()
                .map(|item| match item {
                    Value::String(name) => Ok(name.clone()),
                    _ => Err(Error::new("variants must be an array of strings")),
                })
                .collect::<TargetResult<Vec<_>>>()?,
        ),
        Some(_) => return Err(Error::new("variants must be an array of strings")),
    };
    Ok(Check {
        name,
        read,
        expect,
        variants,
    })
}

fn parse_read(table: &Table) -> TargetResult<ReadSpec> {
    let exactly_one = || Error::new("read must have exactly one of reg, addr, core, expr, clock");
    if table.len() != 1 {
        return Err(exactly_one());
    }
    let (key, value) = table.iter().next().unwrap();
    let Value::String(text) = value else {
        return Err(Error::new(format!("read.{key} must be a string")));
    };
    if text.trim().is_empty() {
        return Err(Error::new(format!("read.{key} is empty")));
    }
    Ok(match key.as_str() {
        "reg" => ReadSpec::Reg(text.clone()),
        "addr" => ReadSpec::Addr(TargetAddress::parse(text).ok_or_else(|| {
            Error::new(format!(
                "read.addr {text:?} must be <access class>:<address>, e.g. AD:0x20000000"
            ))
        })?),
        "core" => ReadSpec::Core(text.clone()),
        "expr" => ReadSpec::Expr(text.clone()),
        "clock" => ReadSpec::Clock(text.trim().to_string()),
        _ => return Err(exactly_one()),
    })
}

fn parse_operand(value: &Value, key: &str) -> TargetResult<Operand> {
    match value {
        Value::Integer(number) if *number >= 0 => Ok(Operand::Int(*number as u64)),
        Value::Integer(_) => Err(Error::new(format!("{key} must not be negative"))),
        Value::String(text) => {
            if let Some(symbol) = text.strip_prefix("sym:") {
                return parse_symbol(symbol)
                    .ok_or_else(|| Error::new(format!("{key}: invalid symbol {text:?}")));
            }
            parse_int_auto(text)
                .and_then(|number| u64::try_from(number).ok())
                .map(Operand::Int)
                .ok_or_else(|| {
                    Error::new(format!(
                        "{key}: {text:?} is neither an integer nor \"sym:<name>[+offset]\""
                    ))
                })
        }
        _ => Err(Error::new(format!(
            "{key} must be an integer or \"sym:<name>[+offset]\""
        ))),
    }
}

/// `<name>`, `<name>+<offset>` or `<name>-<offset>`.
fn parse_symbol(text: &str) -> Option<Operand> {
    let text = text.trim();
    let split = text.rfind(['+', '-']).filter(|&index| index > 0);
    let (name, offset) = match split {
        Some(index) => {
            let magnitude = parse_int_auto(&text[index + 1..])?;
            let magnitude = i64::try_from(magnitude).ok()?;
            let offset = if text.as_bytes()[index] == b'-' {
                -magnitude
            } else {
                magnitude
            };
            (text[..index].trim(), offset)
        }
        None => (text, 0),
    };
    if name.is_empty() {
        return None;
    }
    Some(Operand::Symbol {
        name: name.to_string(),
        offset,
    })
}

fn parse_mask(table: &Table) -> TargetResult<Option<u64>> {
    match table.get("mask") {
        None => Ok(None),
        Some(value) => match parse_operand(value, "expect.mask")? {
            Operand::Int(mask) => Ok(Some(mask)),
            Operand::Symbol { .. } => Err(Error::new("expect.mask must be an integer")),
        },
    }
}

const CLOCK_FORMS: &str = "state, hz, source, warnings";

fn parse_clock_expect(table: &Table) -> TargetResult<ClockExpect> {
    let mut expect = ClockExpect::default();
    if table.is_empty() {
        return Err(Error::new(format!(
            "expect must have at least one of {CLOCK_FORMS} for read.clock"
        )));
    }
    for (key, value) in table {
        match (key.as_str(), value) {
            ("state", Value::String(state)) if state == "on" || state == "off" => {
                expect.on = Some(state == "on");
            }
            ("state", _) => return Err(Error::new("expect.state must be \"on\" or \"off\"")),
            ("hz", Value::Integer(hz)) if *hz >= 0 => expect.hz = Some(Hz(*hz as f64)),
            ("hz", Value::Float(hz)) if hz.is_finite() && *hz >= 0.0 => expect.hz = Some(Hz(*hz)),
            ("hz", _) => return Err(Error::new("expect.hz must be a frequency in Hz")),
            ("source", Value::String(source)) if !source.trim().is_empty() => {
                expect.source = Some(source.trim().to_string());
            }
            ("source", _) => return Err(Error::new("expect.source must be a clock name")),
            ("warnings", Value::Array(items)) => {
                expect.warnings = Some(
                    items
                        .iter()
                        .map(|item| match item {
                            Value::String(text) => Ok(text.clone()),
                            _ => Err(Error::new("expect.warnings must be an array of strings")),
                        })
                        .collect::<TargetResult<Vec<_>>>()?,
                );
            }
            ("warnings", _) => {
                return Err(Error::new("expect.warnings must be an array of strings"));
            }
            (other, _) => {
                return Err(Error::new(format!(
                    "unknown expectation '{other}' for read.clock (expected {CLOCK_FORMS})"
                )));
            }
        }
    }
    if expect.hz.is_some() && expect.on == Some(false) {
        return Err(Error::new("expect.hz with state \"off\""));
    }
    Ok(expect)
}

fn parse_expect(table: &Table) -> TargetResult<Expect> {
    const FORMS: &str = "eq, ne, nonzero, range, in_symbol, one_of";
    if let Some(key) = table
        .keys()
        .find(|key| matches!(key.as_str(), "state" | "hz" | "source" | "warnings"))
    {
        return Err(Error::new(format!(
            "expect.{key} only works with read.clock"
        )));
    }
    let forms: Vec<&String> = table.keys().filter(|key| key.as_str() != "mask").collect();
    if forms.len() != 1 {
        return Err(Error::new(format!(
            "expect must have exactly one of {FORMS} (plus mask with eq/ne)"
        )));
    }
    let key = forms[0].as_str();
    let value = &table[key];
    if table.contains_key("mask") && !matches!(key, "eq" | "ne") {
        return Err(Error::new("expect.mask only works with eq and ne"));
    }
    Ok(match key {
        "eq" => Expect::Eq {
            value: parse_operand(value, "expect.eq")?,
            mask: parse_mask(table)?,
        },
        "ne" => Expect::Ne {
            value: parse_operand(value, "expect.ne")?,
            mask: parse_mask(table)?,
        },
        "nonzero" => match value {
            Value::Boolean(true) => Expect::NonZero,
            _ => return Err(Error::new("expect.nonzero must be true")),
        },
        "range" => {
            let bounds = match value {
                Value::Array(items) if items.len() == 2 => items
                    .iter()
                    .map(|item| match parse_operand(item, "expect.range")? {
                        Operand::Int(number) => Ok(number),
                        Operand::Symbol { .. } => {
                            Err(Error::new("expect.range bounds must be integers"))
                        }
                    })
                    .collect::<TargetResult<Vec<u64>>>()?,
                _ => return Err(Error::new("expect.range must be [low, high]")),
            };
            if bounds[0] > bounds[1] {
                return Err(Error::new("expect.range: low is greater than high"));
            }
            Expect::Range(bounds[0], bounds[1])
        }
        "in_symbol" => match value {
            Value::String(name) if !name.trim().is_empty() => Expect::InSymbol(name.clone()),
            _ => return Err(Error::new("expect.in_symbol must be a symbol name")),
        },
        "one_of" => match value {
            Value::Array(items) if !items.is_empty() => Expect::OneOf(
                items
                    .iter()
                    .map(|item| parse_operand(item, "expect.one_of"))
                    .collect::<TargetResult<Vec<_>>>()?,
            ),
            _ => return Err(Error::new("expect.one_of must be a non-empty array")),
        },
        other => {
            return Err(Error::new(format!(
                "unknown expectation '{other}' (expected one of {FORMS})"
            )));
        }
    })
}

// -------------------------------------------------------------- execution

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub variant: Option<String>,
    pub halt: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pass,
    Fail,
    Error,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Pass => "ok",
            Status::Fail => "FAIL",
            Status::Error => "ERROR",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    pub name: String,
    pub status: Status,
    /// What was (or would be) read, e.g. `reg .HSCTLR (C15:0x4001)`.
    pub read: String,
    pub value: Option<u64>,
    /// The value as text, for reads that are not a number (`read.clock`).
    pub text: Option<String>,
    /// The expectation with symbols resolved.
    pub expect: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub description: Option<String>,
    pub variant: Option<String>,
    pub dry_run: bool,
    /// `--halt` stopped the core.
    pub halted: bool,
    pub skipped: usize,
    pub results: Vec<CheckResult>,
}

impl Report {
    fn count(&self, status: Status) -> usize {
        self.results.iter().filter(|r| r.status == status).count()
    }

    pub fn passed(&self) -> bool {
        self.results.iter().all(|r| r.status == Status::Pass)
    }

    /// 0 when every selected check passed, 3 otherwise.
    pub fn exit_code(&self) -> i32 {
        if self.passed() { 0 } else { EXIT_FAILED }
    }

    pub fn summary(&self) -> String {
        let mut text = if self.dry_run {
            format!(
                "dry run: {} resolved, {} errors",
                self.count(Status::Pass),
                self.count(Status::Error)
            )
        } else {
            format!(
                "{} passed, {} failed, {} errors",
                self.count(Status::Pass),
                self.count(Status::Fail),
                self.count(Status::Error)
            )
        };
        if self.skipped > 0 {
            let variant = match &self.variant {
                Some(variant) => format!("not in variant {variant:?}"),
                None => "variant-specific; pass --variant".to_string(),
            };
            text.push_str(&format!(", {} skipped ({variant})", self.skipped));
        }
        text
    }

    pub fn human(&self, style: Style) -> String {
        let mut lines = Vec::new();
        if let Some(description) = &self.description {
            lines.push(style.value(description));
        }
        if self.halted {
            lines.push(
                style.warn("core halted (Break) for CP15/core register reads; it stays halted"),
            );
        }
        let width = self.results.iter().map(|r| r.name.len()).max().unwrap_or(0);
        for result in &self.results {
            let status = format!("{:<5}", result.status.label());
            let status = match result.status {
                Status::Pass => style.good(status),
                Status::Fail | Status::Error => style.bad(status),
            };
            let value = match (&result.text, result.value) {
                (Some(text), _) => style.value(format!("{text:<10}")),
                (None, Some(value)) => style.value(format!("{:<10}", hex32(value))),
                (None, None) if self.dry_run => format!("{:<10}", result.read),
                (None, None) => style.dim(format!("{:<10}", "-")),
            };
            let mut line = format!(
                "  {status} {}  {value}  {}",
                style.label(format!("{:<width$}", result.name)),
                result.expect
            );
            if let Some(error) = &result.error {
                line.push_str(&format!("  ({error})"));
            }
            lines.push(line.trim_end().to_string());
        }
        lines.push(if self.passed() {
            style.good(self.summary())
        } else {
            style.bad(self.summary())
        });
        lines.join("\n")
    }

    pub fn to_json(&self) -> serde_json::Value {
        let results: Vec<_> = self
            .results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name,
                    "status": r.status.label().to_ascii_lowercase(),
                    "read": r.read,
                    "value": r.value,
                    "hex": if r.text.is_some() { None } else { r.value.map(hex32) },
                    "text": r.text,
                    "expect": r.expect,
                    "error": r.error,
                })
            })
            .collect();
        serde_json::json!({
            "description": self.description,
            "variant": self.variant,
            "dry_run": self.dry_run,
            "halted": self.halted,
            "passed": self.passed(),
            "summary": {
                "passed": self.count(Status::Pass),
                "failed": self.count(Status::Fail),
                "errors": self.count(Status::Error),
                "skipped": self.skipped,
            },
            "results": results,
        })
    }
}

/// An expectation with its symbols resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Resolved {
    Eq(u64, Option<u64>),
    Ne(u64, Option<u64>),
    NonZero,
    Range(u64, u64),
    OneOf(Vec<u64>),
    /// Judged against the evaluated clock tree, not a number.
    Clock(ClockExpect),
}

impl Resolved {
    fn holds(&self, value: u64) -> bool {
        match self {
            Resolved::Eq(expected, mask) => value & mask.unwrap_or(u64::MAX) == *expected,
            Resolved::Ne(expected, mask) => value & mask.unwrap_or(u64::MAX) != *expected,
            Resolved::NonZero => value != 0,
            Resolved::Range(low, high) => (*low..=*high).contains(&value),
            Resolved::OneOf(values) => values.contains(&value),
            Resolved::Clock(_) => false,
        }
    }
}

fn describe_operand(operand: &Operand, value: u64) -> String {
    match operand {
        Operand::Int(_) => hex32(value),
        Operand::Symbol { name, offset: 0 } => format!("{name} ({})", hex32(value)),
        Operand::Symbol { name, offset } if *offset > 0 => {
            format!("{name}+0x{offset:X} ({})", hex32(value))
        }
        Operand::Symbol { name, offset } => {
            format!("{name}-0x{:X} ({})", offset.unsigned_abs(), hex32(value))
        }
    }
}

fn resolve_operand(target: &mut dyn Target, operand: &Operand) -> TargetResult<u64> {
    match operand {
        Operand::Int(value) => Ok(*value),
        Operand::Symbol { name, offset } => {
            let base = target.symbol_address(name)?;
            Ok(base.wrapping_add_signed(*offset))
        }
    }
}

fn resolve_expect(target: &mut dyn Target, expect: &Expect) -> TargetResult<(Resolved, String)> {
    let masked = |mask: &Option<u64>| match mask {
        Some(mask) => format!("& {} ", hex32(*mask)),
        None => String::new(),
    };
    Ok(match expect {
        Expect::Eq { value, mask } => {
            let resolved = resolve_operand(target, value)?;
            (
                Resolved::Eq(resolved, *mask),
                format!("{}== {}", masked(mask), describe_operand(value, resolved)),
            )
        }
        Expect::Ne { value, mask } => {
            let resolved = resolve_operand(target, value)?;
            (
                Resolved::Ne(resolved, *mask),
                format!("{}!= {}", masked(mask), describe_operand(value, resolved)),
            )
        }
        Expect::NonZero => (Resolved::NonZero, "!= 0".into()),
        Expect::Range(low, high) => (
            Resolved::Range(*low, *high),
            format!("in [{}, {}]", hex32(*low), hex32(*high)),
        ),
        Expect::InSymbol(name) => {
            let (begin, end) = target.symbol_range(name)?;
            (
                Resolved::Range(begin, end),
                format!("in {name} [{}..{}]", hex32(begin), hex32(end)),
            )
        }
        Expect::OneOf(operands) => {
            let mut values = Vec::new();
            let mut texts = Vec::new();
            for operand in operands {
                let value = resolve_operand(target, operand)?;
                values.push(value);
                texts.push(describe_operand(operand, value));
            }
            (
                Resolved::OneOf(values),
                format!("one of [{}]", texts.join(", ")),
            )
        }
        Expect::Clock(expect) => (Resolved::Clock(expect.clone()), expect.describe()),
    })
}

/// A check after the resolve phase.
struct Prepared<'a> {
    check: &'a Check,
    /// A resolved `reg` or `expr` read.
    backend: Option<BackendRead>,
    read: String,
    expect: Option<(Resolved, String)>,
    error: Option<Error>,
}

impl Prepared<'_> {
    /// CP15/CP14 and core registers can only be read from a halted core.
    fn needs_halt(&self) -> bool {
        match &self.check.read {
            ReadSpec::Reg(_) | ReadSpec::Expr(_) => {
                self.backend.as_ref().is_some_and(|read| read.needs_halt)
            }
            ReadSpec::Addr(address) => address.is_coprocessor(),
            ReadSpec::Core(_) => true,
            ReadSpec::Clock(_) => false,
        }
    }
}

/// Keep a lost connection fatal; everything else fails only this check.
fn per_check<T>(result: TargetResult<T>) -> TargetResult<Result<T, Error>> {
    match result {
        Err(error) if error.lost => Err(error),
        other => Ok(other),
    }
}

/// The clock tree for `read.clock` checks: its description and the board's
/// frequencies (from the tool's configuration).
pub struct Clocks<'a> {
    pub tree: &'a clock::Tree,
    pub inputs: &'a [(String, f64)],
}

impl CheckFile {
    /// Whether a check selected for `variant` reads a clock.
    pub fn reads_clocks(&self, variant: Option<&str>) -> bool {
        self.checks
            .iter()
            .any(|check| check.selected(variant) && matches!(check.read, ReadSpec::Clock(_)))
    }
}

pub fn run(
    target: &mut dyn Target,
    file: &CheckFile,
    options: &Options,
    clocks: Option<&Clocks>,
) -> TargetResult<Report> {
    let selected: Vec<&Check> = file
        .checks
        .iter()
        .filter(|check| check.selected(options.variant.as_deref()))
        .collect();
    let skipped = file.checks.len() - selected.len();

    let mut kinds = Vec::new();
    for check in &selected {
        if let Some((kind, _)) = check.read.backend_read() {
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
    }
    if !kinds.is_empty() {
        target.begin_reads(&kinds)?;
    }

    // Resolve register names and symbols; nothing is read from the target.
    let mut prepared = Vec::new();
    for check in selected {
        let mut item = Prepared {
            check,
            backend: None,
            read: String::new(),
            expect: None,
            error: None,
        };
        match &check.read {
            ReadSpec::Reg(_) | ReadSpec::Expr(_) => {
                let (kind, argument) = check.read.backend_read().expect("reg or expr");
                match per_check(target.prepare_read(kind, argument))? {
                    Ok(read) => {
                        item.read = read.label.clone();
                        item.backend = Some(read);
                    }
                    Err(error) => {
                        item.read = format!("{kind} {argument}");
                        item.error = Some(error);
                    }
                }
            }
            ReadSpec::Addr(address) => item.read = format!("addr {address}"),
            ReadSpec::Core(name) => item.read = format!("core {name}"),
            ReadSpec::Clock(name) => {
                item.read = format!("clock {name}");
                match clocks {
                    None => {
                        item.error = Some(Error::new("read.clock needs a clock tree description"));
                    }
                    Some(clocks) if !clocks.tree.clocks.iter().any(|c| c.name == *name) => {
                        item.error = Some(Error::new(format!(
                            "clock {name} is not in the clock tree description"
                        )));
                    }
                    Some(_) => {}
                }
            }
        }
        match per_check(resolve_expect(target, &check.expect))? {
            Ok(expect) => item.expect = Some(expect),
            Err(error) => {
                item.error.get_or_insert(error);
            }
        }
        prepared.push(item);
    }

    let mut halted = false;
    if !options.dry_run {
        let halting: Vec<&str> = prepared
            .iter()
            .filter(|item| item.needs_halt())
            .map(|item| item.check.name.as_str())
            .collect();
        if !halting.is_empty() && target.is_running()? {
            if !options.halt {
                return Err(Error::new(format!(
                    "the core is running, and {} check(s) read CP15 or core registers, which \
                     needs a halted core ({}); stop it with '{}' or pass --halt",
                    halting.len(),
                    halting.join(", "),
                    target.break_command()
                )));
            }
            target.halt()?;
            halted = true;
        }
    }

    // The clock tree is evaluated once, from memory-mapped registers, after
    // the core is stopped (when --halt stops it).
    let wants_clocks = prepared
        .iter()
        .any(|item| item.error.is_none() && matches!(item.check.read, ReadSpec::Clock(_)));
    let clock_report = match clocks {
        Some(clocks) if wants_clocks && !options.dry_run => Some(per_check(clock::evaluate(
            target,
            clocks.tree,
            clocks.inputs,
        ))?),
        _ => None,
    };

    let mut results = Vec::new();
    for item in prepared {
        let expect_text = item
            .expect
            .as_ref()
            .map(|(_, text)| text.clone())
            .unwrap_or_else(|| describe_expect(&item.check.expect));
        let mut result = CheckResult {
            name: item.check.name.clone(),
            status: Status::Error,
            read: item.read.clone(),
            value: None,
            text: None,
            expect: expect_text,
            error: None,
        };
        if let Some(error) = &item.error {
            result.error = Some(error.message.clone());
            results.push(result);
            continue;
        }
        if options.dry_run {
            result.status = Status::Pass;
            results.push(result);
            continue;
        }
        if let (ReadSpec::Clock(name), Some(Expect::Clock(expect))) =
            (&item.check.read, Some(&item.check.expect))
        {
            judge_clock(&mut result, name, expect, clock_report.as_ref());
            results.push(result);
            continue;
        }
        let value = per_check(read_value(target, &item))?;
        match value {
            Ok(value) => {
                result.value = Some(value);
                let (resolved, _) = item.expect.as_ref().expect("resolved without error");
                result.status = if resolved.holds(value) {
                    Status::Pass
                } else {
                    Status::Fail
                };
            }
            Err(error) => result.error = Some(error.message),
        }
        results.push(result);
    }

    Ok(Report {
        description: file.description.clone(),
        variant: options.variant.clone(),
        dry_run: options.dry_run,
        halted,
        skipped,
        results,
    })
}

/// Fill in the result of a `read.clock` check from the evaluated tree.
fn judge_clock(
    result: &mut CheckResult,
    name: &str,
    expect: &ClockExpect,
    report: Option<&Result<clock::Report, Error>>,
) {
    let clock = match report {
        Some(Ok(report)) => report.clocks.iter().find(|clock| clock.name == name),
        Some(Err(error)) => {
            result.error = Some(format!(
                "the clock tree could not be read: {}",
                error.message
            ));
            return;
        }
        None => None,
    };
    let Some(clock) = clock else {
        result.error = Some(format!("clock {name} was not evaluated"));
        return;
    };
    result.text = Some(describe_clock(clock));
    if let clock::State::Error(reason) = &clock.state {
        result.error = Some(reason.clone());
        return;
    }
    if let clock::State::Hz(hz) = clock.state {
        result.value = Some(hz.round() as u64);
    }
    result.status = if expect.mismatches(clock).is_empty() {
        Status::Pass
    } else {
        Status::Fail
    };
}

fn read_value(target: &mut dyn Target, item: &Prepared) -> TargetResult<u64> {
    match &item.check.read {
        ReadSpec::Reg(_) | ReadSpec::Expr(_) => {
            target.read_prepared(item.backend.as_ref().expect("resolved without error"))
        }
        ReadSpec::Addr(address) => target.read_u32(address).map(u64::from),
        ReadSpec::Core(name) => target.read_core_register(name),
        ReadSpec::Clock(_) => unreachable!("clock checks are judged by judge_clock"),
    }
}

/// The expectation as written, for checks whose symbols did not resolve.
fn describe_expect(expect: &Expect) -> String {
    let operand = |operand: &Operand| match operand {
        Operand::Int(value) => hex32(*value),
        Operand::Symbol { name, offset: 0 } => format!("sym:{name}"),
        Operand::Symbol { name, offset } => format!("sym:{name}{offset:+}"),
    };
    match expect {
        Expect::Eq { value, .. } => format!("== {}", operand(value)),
        Expect::Ne { value, .. } => format!("!= {}", operand(value)),
        Expect::NonZero => "!= 0".into(),
        Expect::Range(low, high) => format!("in [{}, {}]", hex32(*low), hex32(*high)),
        Expect::InSymbol(name) => format!("in {name}"),
        Expect::OneOf(values) => format!(
            "one of [{}]",
            values.iter().map(operand).collect::<Vec<_>>().join(", ")
        ),
        Expect::Clock(expect) => expect.describe(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// A target without backend-specific reads (like OpenOCD would be).
    #[derive(Default)]
    struct FakeTarget {
        words: HashMap<String, u32>,
        core: HashMap<String, u64>,
        symbols: HashMap<String, (u64, u64)>,
        running: bool,
        log: Vec<String>,
    }

    impl Target for FakeTarget {
        fn read_u32(&mut self, address: &TargetAddress) -> TargetResult<u32> {
            self.log.push(format!("read {address}"));
            self.words
                .get(&address.to_string())
                .copied()
                .ok_or_else(|| Error::new("bus error"))
        }
        fn read_core_register(&mut self, name: &str) -> TargetResult<u64> {
            self.log.push(format!("core {name}"));
            Ok(self.core[name])
        }
        fn read_memory(&mut self, _: u64, _: usize) -> TargetResult<Vec<u8>> {
            unreachable!()
        }
        fn is_running(&mut self) -> TargetResult<bool> {
            Ok(self.running)
        }
        fn halt(&mut self) -> TargetResult<()> {
            self.log.push("halt".into());
            self.running = false;
            Ok(())
        }
        fn symbol_range(&mut self, name: &str) -> TargetResult<(u64, u64)> {
            self.symbols
                .get(name)
                .copied()
                .ok_or_else(|| Error::new(format!("symbol {name} not found")))
        }
        fn break_command(&self) -> &str {
            "fake halt"
        }
    }

    const FILE: &str = r#"
        [[check]]
        name = "ctrl"
        read = { addr = "AD:0x100" }
        expect = { eq = 0x5, mask = 0xF }
        [[check]]
        name = "pc"
        read = { core = "PC" }
        expect = { in_symbol = "main" }
        [[check]]
        name = "vector"
        read = { addr = "AD:0x104" }
        expect = { eq = "sym:main+4" }
        [[check]]
        name = "per"
        read = { reg = "HSR" }
        expect = { nonzero = true }
    "#;

    fn target(running: bool) -> FakeTarget {
        let mut target = FakeTarget {
            running,
            ..FakeTarget::default()
        };
        target.words.insert("AD:0x100".into(), 0xA5);
        target.words.insert("AD:0x104".into(), 0x1004);
        target.core.insert("PC".into(), 0x1010);
        target.symbols.insert("main".into(), (0x1000, 0x10FF));
        target
    }

    #[test]
    fn runs_through_the_typed_target() {
        let file = parse(FILE).unwrap();
        let mut target = target(false);
        let report = run(&mut target, &file, &Options::default(), None).unwrap();
        let statuses: Vec<_> = report.results.iter().map(|r| r.status).collect();
        assert_eq!(
            statuses,
            [Status::Pass, Status::Pass, Status::Pass, Status::Error]
        );
        assert_eq!(report.results[1].expect, "in main [0x00001000..0x000010FF]");
        assert_eq!(report.results[2].expect, "== main+0x4 (0x00001004)");
        assert_eq!(report.results[3].read, "reg HSR");
        assert_eq!(
            report.results[3].error.as_deref(),
            Some("read.reg is not supported by this backend")
        );
        assert_eq!(report.exit_code(), EXIT_FAILED);
        assert_eq!(target.log, ["read AD:0x100", "core PC", "read AD:0x104"]);
    }

    #[test]
    fn core_reads_on_a_running_core_name_the_tools_break_command() {
        let file = parse(FILE).unwrap();
        let error = run(&mut target(true), &file, &Options::default(), None).unwrap_err();
        assert!(
            error
                .message
                .ends_with("(pc); stop it with 'fake halt' or pass --halt"),
            "{error}"
        );
        let mut target = target(true);
        let options = Options {
            halt: true,
            ..Options::default()
        };
        let report = run(&mut target, &file, &options, None).unwrap();
        assert!(report.halted);
        assert_eq!(target.log[0], "halt");
    }
}
