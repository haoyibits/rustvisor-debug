//! `clock`: the clock tree with frequencies, computed from the clock
//! registers and a description of the chip's clock tree.
//!
//! The engine knows no chip: the description is a TOML file, chosen by chip
//! name from a directory of descriptions (each tool has its own, e.g.
//! `~/.config/tracebridge/clock/*.toml`) or given with `--tree`. The board
//! only adds the frequencies that no register holds, such as the crystal
//! (`XOSC=40MHz`).
//!
//! ```toml
//! description = "optional"
//! chips = ["MYCHIP*"]                  # matched against flash.chip / target.cpu
//!
//! [reg]                                # 32-bit registers, read with Data.Long()
//! PLLDV = "AD:0x40001008"
//!
//! [[clock]]
//! name = "XOSC"                        # a root: the board gives its frequency
//!
//! [[clock]]
//! name = "IRC"
//! hz = "16MHz"                         # a root with a nominal frequency
//!
//! [[clock]]
//! name = "PLL"
//! select = "CLKSEL[24]"                # or: from = "XOSC"
//! sources = { "0" = "IRC", "1" = "XOSC" }
//! mul = "PLLDV[6:0]"                   # frequency = source * mul / div
//! div = "PLLDV[14:12] * (PLLDV[21:16] + 1)"
//! enable = "PLLCR[8]"                  # 0 means the clock is off
//! warn = [{ when = "PLLSR[2] == 0", text = "not locked" }]
//! note = "optional"
//! group = "optional"                   # the block of the chip it belongs to,
//!                                      # for the diagram of --html
//! ```
//!
//! Expressions take numbers, register fields (`REG[hi:lo]`, `REG[bit]`, `REG`),
//! `+ - * / ^`, `== !=` and parentheses.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use toml::{Table, Value};

use crate::style::Style;
use crate::target::{Error, Result as TargetResult, Target, TargetAddress, hex32};
use crate::text::{glob_match, parse_frequency, parse_int_auto, specificity};

// ------------------------------------------------------------ expressions

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Number(f64),
    Field {
        register: String,
        high: u32,
        low: u32,
    },
    Binary(Box<Node>, char, Box<Node>),
}

/// A parsed expression with its text, for messages.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub text: String,
    node: Node,
}

struct Parser<'a> {
    text: &'a str,
    position: usize,
}

impl<'a> Parser<'a> {
    fn rest(&self) -> &str {
        &self.text[self.position..]
    }

    fn skip_spaces(&mut self) {
        self.position += self.rest().len() - self.rest().trim_start().len();
    }

    fn eat(&mut self, token: &str) -> bool {
        self.skip_spaces();
        if self.rest().starts_with(token) {
            self.position += token.len();
            true
        } else {
            false
        }
    }

    fn fail<T>(&self, message: &str) -> Result<T, String> {
        Err(format!("{message} at column {}", self.position + 1))
    }

    /// The longest prefix whose characters satisfy `keep`.
    fn take(&mut self, keep: impl Fn(char) -> bool) -> &'a str {
        let rest: &'a str = &self.text[self.position..];
        let length = rest.find(|c| !keep(c)).unwrap_or(rest.len());
        self.position += length;
        &rest[..length]
    }

    fn comparison(&mut self) -> Result<Node, String> {
        let left = self.sum()?;
        for (token, operator) in [("==", '='), ("!=", '!')] {
            if self.eat(token) {
                let right = self.sum()?;
                return Ok(Node::Binary(Box::new(left), operator, Box::new(right)));
            }
        }
        Ok(left)
    }

    fn sum(&mut self) -> Result<Node, String> {
        let mut left = self.product()?;
        loop {
            let operator = if self.eat("+") {
                '+'
            } else if self.eat("-") {
                '-'
            } else {
                return Ok(left);
            };
            let right = self.product()?;
            left = Node::Binary(Box::new(left), operator, Box::new(right));
        }
    }

    fn product(&mut self) -> Result<Node, String> {
        let mut left = self.power()?;
        loop {
            let operator = if self.eat("*") {
                '*'
            } else if self.eat("/") {
                '/'
            } else {
                return Ok(left);
            };
            let right = self.power()?;
            left = Node::Binary(Box::new(left), operator, Box::new(right));
        }
    }

    fn power(&mut self) -> Result<Node, String> {
        let base = self.atom()?;
        if self.eat("^") {
            let exponent = self.power()?;
            return Ok(Node::Binary(Box::new(base), '^', Box::new(exponent)));
        }
        Ok(base)
    }

    fn integer(&mut self) -> Result<u32, String> {
        self.skip_spaces();
        let digits = self.take(|c| c.is_ascii_digit());
        match digits.parse() {
            Ok(value) => Ok(value),
            Err(_) => self.fail("expected a bit number"),
        }
    }

    fn atom(&mut self) -> Result<Node, String> {
        self.skip_spaces();
        if self.eat("(") {
            let inner = self.comparison()?;
            if !self.eat(")") {
                return self.fail("expected ')'");
            }
            return Ok(inner);
        }
        let Some(first) = self.rest().chars().next() else {
            return self.fail("expected a number or a register");
        };
        if first.is_ascii_digit() {
            let text = self.take(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_');
            let number = parse_int_auto(text)
                .map(|value| value as f64)
                .or_else(|| text.parse().ok());
            return match number {
                Some(number) => Ok(Node::Number(number)),
                None => self.fail("not a number"),
            };
        }
        if first.is_ascii_alphabetic() || first == '_' {
            let register = self
                .take(|c| c.is_ascii_alphanumeric() || c == '_')
                .to_string();
            if !self.eat("[") {
                return Ok(Node::Field {
                    register,
                    high: 31,
                    low: 0,
                });
            }
            let high = self.integer()?;
            let low = if self.eat(":") { self.integer()? } else { high };
            if !self.eat("]") {
                return self.fail("expected ']'");
            }
            if high < low || high > 31 {
                return self.fail("bits must be [high:low] within 31..0");
            }
            return Ok(Node::Field {
                register,
                high,
                low,
            });
        }
        self.fail("expected a number or a register")
    }
}

impl Expr {
    pub fn parse(text: &str) -> Result<Expr, String> {
        let mut parser = Parser { text, position: 0 };
        let node = parser.comparison()?;
        parser.skip_spaces();
        if !parser.rest().is_empty() {
            return parser.fail("unexpected text");
        }
        Ok(Expr {
            text: text.trim().to_string(),
            node,
        })
    }

    fn registers<'a>(node: &'a Node, names: &mut Vec<&'a str>) {
        match node {
            Node::Number(_) => {}
            Node::Field { register, .. } => names.push(register),
            Node::Binary(left, _, right) => {
                Expr::registers(left, names);
                Expr::registers(right, names);
            }
        }
    }

    fn eval_node(node: &Node, registers: &Registers) -> Result<f64, String> {
        Ok(match node {
            Node::Number(number) => *number,
            Node::Field {
                register,
                high,
                low,
            } => {
                let value = registers.value(register)?;
                let width = high - low + 1;
                ((value >> low) & ((1u64 << width) - 1)) as f64
            }
            Node::Binary(left, operator, right) => {
                let left = Expr::eval_node(left, registers)?;
                let right = Expr::eval_node(right, registers)?;
                match operator {
                    '+' => left + right,
                    '-' => left - right,
                    '*' => left * right,
                    '/' if right == 0.0 => return Err("division by zero".into()),
                    '/' => left / right,
                    '^' => left.powf(right),
                    '=' => f64::from(left == right),
                    _ => f64::from(left != right),
                }
            }
        })
    }

    /// Why the expression turned a clock off: `CTL[2] = 0`, or
    /// `SEL[27:24] != 15 is false` for a comparison.
    fn zero(&self) -> String {
        match self.node {
            Node::Binary(_, '=' | '!', _) => format!("{} is false", self.text),
            _ => format!("{} = 0", self.text),
        }
    }

    fn eval(&self, registers: &Registers) -> Result<f64, String> {
        Expr::eval_node(&self.node, registers).map_err(|error| format!("{}: {error}", self.text))
    }
}

// ------------------------------------------------------------ description

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// The board or the description gives the frequency.
    Root {
        hz: Option<f64>,
    },
    From(String),
    Select {
        field: Expr,
        sources: BTreeMap<u64, String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Warning {
    when: Expr,
    text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Clock {
    pub name: String,
    pub note: Option<String>,
    /// The block of the chip the clock belongs to (a clock generation
    /// module, say); the diagram draws one panel per group.
    pub group: Option<String>,
    pub source: Source,
    mul: Option<Expr>,
    div: Option<Expr>,
    enable: Option<Expr>,
    warn: Vec<Warning>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tree {
    pub description: Option<String>,
    pub chips: Vec<String>,
    pub registers: Vec<(String, TargetAddress)>,
    pub clocks: Vec<Clock>,
}

fn string<'a>(table: &'a Table, key: &str) -> Result<Option<&'a str>, String> {
    match table.get(key) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

/// An expression: a string, or a plain number.
fn expression(table: &Table, key: &str) -> Result<Option<Expr>, String> {
    let text = match table.get(key) {
        None => return Ok(None),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Integer(number)) => number.to_string(),
        Some(Value::Float(number)) => number.to_string(),
        Some(_) => return Err(format!("{key} must be an expression or a number")),
    };
    Expr::parse(&text)
        .map(Some)
        .map_err(|error| format!("{key} = {text:?}: {error}"))
}

fn parse_clock(table: &Table) -> Result<Clock, String> {
    const KEYS: [&str; 11] = [
        "name", "note", "group", "hz", "from", "select", "sources", "mul", "div", "enable", "warn",
    ];
    if let Some(key) = table.keys().find(|key| !KEYS.contains(&key.as_str())) {
        return Err(format!("unknown key '{key}'"));
    }
    let name = match string(table, "name")? {
        Some(name) if is_name(name) => name.to_string(),
        Some(name) => {
            return Err(format!(
                "name {name:?} must be letters, digits and '_' (it is also an argument name)"
            ));
        }
        None => return Err("name is missing".into()),
    };
    let from = string(table, "from")?;
    let select = expression(table, "select")?;
    let hz = match table.get("hz") {
        None => None,
        Some(Value::Integer(number)) if *number > 0 => Some(*number as f64),
        Some(Value::String(text)) => Some(parse_frequency(text).map_err(|e| format!("hz: {e}"))?),
        Some(_) => return Err("hz must be a frequency such as \"16MHz\" or a number of Hz".into()),
    };
    let source = match (from, select) {
        (Some(_), Some(_)) => return Err("from and select exclude each other".into()),
        (Some(from), None) => Source::From(from.to_string()),
        (None, Some(field)) => {
            let Some(Value::Table(entries)) = table.get("sources") else {
                return Err("select needs sources = { \"<value>\" = \"<clock>\", ... }".into());
            };
            let mut sources = BTreeMap::new();
            for (key, value) in entries {
                let number = parse_int_auto(key)
                    .and_then(|number| u64::try_from(number).ok())
                    .ok_or_else(|| format!("sources: {key:?} is not a field value"))?;
                let Value::String(clock) = value else {
                    return Err(format!("sources.{key} must be a clock name"));
                };
                sources.insert(number, clock.clone());
            }
            Source::Select { field, sources }
        }
        (None, None) => Source::Root { hz },
    };
    if !matches!(source, Source::Root { .. }) && hz.is_some() {
        return Err("hz only belongs to a clock without from or select".into());
    }
    if !matches!(source, Source::Select { .. }) && table.contains_key("sources") {
        return Err("sources needs select".into());
    }
    let mut warn = Vec::new();
    match table.get("warn") {
        None => {}
        Some(Value::Array(entries)) => {
            for entry in entries {
                let Value::Table(entry) = entry else {
                    return Err("warn must be [{ when = \"...\", text = \"...\" }]".into());
                };
                match (expression(entry, "when")?, string(entry, "text")?) {
                    (Some(when), Some(text)) if entry.len() == 2 => warn.push(Warning {
                        when,
                        text: text.to_string(),
                    }),
                    _ => return Err("warn entries need exactly when and text".into()),
                }
            }
        }
        Some(_) => return Err("warn must be [{ when = \"...\", text = \"...\" }]".into()),
    }
    Ok(Clock {
        name,
        note: string(table, "note")?.map(str::to_string),
        group: string(table, "group")?.map(str::to_string),
        source,
        mul: expression(table, "mul")?,
        div: expression(table, "div")?,
        enable: expression(table, "enable")?,
        warn,
    })
}

fn is_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl Clock {
    fn expressions(&self) -> impl Iterator<Item = &Expr> {
        let select = match &self.source {
            Source::Select { field, .. } => Some(field),
            _ => None,
        };
        select
            .into_iter()
            .chain(&self.mul)
            .chain(&self.div)
            .chain(&self.enable)
            .chain(self.warn.iter().map(|warning| &warning.when))
    }

    /// The clocks this one may run from.
    fn parents(&self) -> Vec<&str> {
        match &self.source {
            Source::Root { .. } => Vec::new(),
            Source::From(parent) => vec![parent],
            Source::Select { sources, .. } => sources.values().map(String::as_str).collect(),
        }
    }
}

pub fn parse(text: &str) -> Result<Tree, String> {
    let document: Table = text
        .parse()
        .map_err(|error: toml::de::Error| error.message().trim_end().to_string())?;
    for key in document.keys() {
        if !matches!(key.as_str(), "description" | "chips" | "reg" | "clock") {
            return Err(format!(
                "unknown key '{key}' (expected description, chips, [reg] and [[clock]])"
            ));
        }
    }
    let chips = match document.get("chips") {
        None => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::String(pattern) => Ok(pattern.clone()),
                _ => Err("chips must be an array of strings".to_string()),
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("chips must be an array of strings".into()),
    };
    let mut registers = Vec::new();
    match document.get("reg") {
        None => {}
        Some(Value::Table(entries)) => {
            for (name, value) in entries {
                let address = match value {
                    Value::String(text) => TargetAddress::parse(text),
                    _ => None,
                };
                let Some(address) = address else {
                    return Err(format!(
                        "reg.{name} must be <access class>:<address>, e.g. \"AD:0x40001008\""
                    ));
                };
                registers.push((name.clone(), address));
            }
        }
        Some(_) => return Err("[reg] must be a table".into()),
    }
    let entries = match document.get("clock") {
        Some(Value::Array(entries)) if !entries.is_empty() => entries,
        _ => return Err("no [[clock]] entries".into()),
    };
    let mut clocks: Vec<Clock> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let Value::Table(table) = entry else {
            return Err(format!("clock #{} must be a table", index + 1));
        };
        let label = match table.get("name") {
            Some(Value::String(name)) => format!("clock {name}"),
            _ => format!("clock #{}", index + 1),
        };
        let clock = parse_clock(table).map_err(|error| format!("{label}: {error}"))?;
        if clocks.iter().any(|other| other.name == clock.name) {
            return Err(format!("{label}: defined twice"));
        }
        clocks.push(clock);
    }
    for clock in &clocks {
        for parent in clock.parents() {
            if !clocks.iter().any(|other| other.name == parent) {
                return Err(format!(
                    "clock {}: source {parent} is not a clock",
                    clock.name
                ));
            }
        }
        for expression in clock.expressions() {
            let mut names = Vec::new();
            Expr::registers(&expression.node, &mut names);
            if let Some(name) = names
                .into_iter()
                .find(|name| !registers.iter().any(|(register, _)| register == name))
            {
                return Err(format!(
                    "clock {}: {}: {name} is not in [reg]",
                    clock.name, expression.text
                ));
            }
        }
    }
    Ok(Tree {
        description: string(&document, "description")?.map(str::to_string),
        chips,
        registers,
        clocks,
    })
}

pub fn load(path: &Path) -> TargetResult<Tree> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| Error::new(format!("cannot read {}: {error}", path.display())))?;
    parse(&text).map_err(|error| Error::new(format!("{}: {error}", path.display())))
}

// ----------------------------------------------------------------- lookup

/// The description in `directory` whose `chips` match `chip` best.
pub fn find(chip: &str, directory: &Path) -> TargetResult<(PathBuf, Tree)> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"))
        })
        .collect();
    files.sort();
    let mut broken = Vec::new();
    let mut candidates = Vec::new();
    for path in files {
        match load(&path) {
            Ok(tree) => {
                let best = tree
                    .chips
                    .iter()
                    .filter(|pattern| glob_match(pattern, chip))
                    .map(|pattern| specificity(pattern))
                    .max();
                if let Some(rank) = best {
                    candidates.push((rank, path, tree));
                }
            }
            Err(error) => broken.push(error.message),
        }
    }
    let Some(best) = candidates.iter().map(|candidate| candidate.0).max() else {
        let broken = if broken.is_empty() {
            String::new()
        } else {
            format!(" (not usable: {})", broken.join("; "))
        };
        return Err(Error::new(format!(
            "no clock tree description for chip {chip}{broken}; put a file with \
             chips = [\"{chip}\"] into {} or pass --tree <file>",
            directory.display()
        )));
    };
    let mut top: Vec<_> = candidates
        .into_iter()
        .filter(|candidate| candidate.0 == best)
        .collect();
    if top.len() > 1 {
        let list: Vec<String> = top
            .iter()
            .map(|(_, path, _)| path.display().to_string())
            .collect();
        return Err(Error::new(format!(
            "chip {chip} matches several clock tree descriptions equally well: {}",
            list.join(", ")
        )));
    }
    let (_, path, tree) = top.remove(0);
    Ok((path, tree))
}

/// `NAME=<frequency>` arguments.
pub fn parse_inputs(arguments: &[String]) -> TargetResult<Vec<(String, f64)>> {
    arguments
        .iter()
        .map(|argument| {
            let parsed = argument.split_once('=').and_then(|(name, frequency)| {
                Some((name.trim().to_string(), parse_frequency(frequency).ok()?))
            });
            parsed.ok_or_else(|| {
                Error::new(format!(
                    "{argument:?} must be <clock>=<frequency>, e.g. XOSC=40MHz"
                ))
            })
        })
        .collect()
}

// ------------------------------------------------------------- evaluation

/// The registers of a description, each read once.
pub struct Registers {
    values: Vec<(String, TargetAddress, Result<u64, String>)>,
}

impl Registers {
    /// Read the registers that the clocks use, each once.
    pub fn read(target: &mut dyn Target, tree: &Tree) -> TargetResult<Registers> {
        let mut used = Vec::new();
        for clock in &tree.clocks {
            for expression in clock.expressions() {
                Expr::registers(&expression.node, &mut used);
            }
        }
        let mut values = Vec::new();
        for (name, address) in &tree.registers {
            if !used.contains(&name.as_str()) {
                continue;
            }
            let value = match target.read_u32(address) {
                Ok(value) => Ok(u64::from(value)),
                Err(error) if error.lost => return Err(error),
                Err(error) => Err(error.message),
            };
            values.push((name.clone(), address.clone(), value));
        }
        Ok(Registers { values })
    }

    fn value(&self, name: &str) -> Result<u64, String> {
        let (_, address, value) = self
            .values
            .iter()
            .find(|(register, _, _)| register == name)
            .expect("checked by parse");
        value
            .clone()
            .map_err(|error| format!("cannot read {name} ({address}): {error}"))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Hz(f64),
    Off(String),
    /// The frequency cannot be told: an input is missing or a source is not
    /// described.
    Unknown(String),
    Error(String),
}

/// What a selector shows: the field, its value and the sources it can pick.
#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    pub field: String,
    pub value: u64,
    pub options: Vec<(u64, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Evaluated {
    pub name: String,
    pub note: Option<String>,
    pub group: Option<String>,
    /// A source clock: it runs from no other clock of the tree.
    pub root: bool,
    /// The clock it runs from, when that is known.
    pub parent: Option<String>,
    pub selection: Option<Selection>,
    pub state: State,
    /// What happens to the source frequency: `x20`, `/2`; `given` or
    /// `nominal` for a source clock.
    pub steps: Vec<String>,
    pub warnings: Vec<String>,
}

impl Evaluated {
    /// How the frequency comes about: `CLKSEL[24]=1 x20 /2`.
    pub fn how(&self) -> String {
        let selection = self
            .selection
            .as_ref()
            .map(|selection| format!("{}={}", selection.field, selection.value));
        let parts: Vec<String> = selection.into_iter().chain(self.steps.clone()).collect();
        parts.join(" ")
    }
}

pub struct Report {
    pub clocks: Vec<Evaluated>,
    pub registers: Registers,
    /// Roots without a frequency.
    pub missing: Vec<String>,
    /// Where the tool's configuration takes those frequencies, for the hint
    /// in the text output (tracebridge: `[clock] in trace32.toml`).
    pub inputs_hint: String,
}

/// A factor or a field value: integers without decimals.
fn number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.0}")
    } else {
        let text = format!("{value:.6}");
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// `400 MHz`, `6.048 MHz`, `32.768 kHz`, `50 Hz`.
pub fn format_frequency(hz: f64) -> String {
    let (scaled, unit) = if hz >= 1e9 {
        (hz / 1e9, "GHz")
    } else if hz >= 1e6 {
        (hz / 1e6, "MHz")
    } else if hz >= 1e3 {
        (hz / 1e3, "kHz")
    } else {
        (hz, "Hz")
    };
    format!("{} {unit}", number(scaled))
}

struct Evaluator<'a> {
    tree: &'a Tree,
    registers: &'a Registers,
    inputs: &'a [(String, f64)],
    done: HashMap<usize, Evaluated>,
    visiting: Vec<usize>,
}

impl Evaluator<'_> {
    fn index(&self, name: &str) -> usize {
        self.tree
            .clocks
            .iter()
            .position(|clock| clock.name == name)
            .expect("checked by parse")
    }

    fn evaluate(&mut self, index: usize) -> Evaluated {
        if let Some(done) = self.done.get(&index) {
            return done.clone();
        }
        let clock = &self.tree.clocks[index];
        let mut result = Evaluated {
            name: clock.name.clone(),
            note: clock.note.clone(),
            group: clock.group.clone(),
            root: matches!(clock.source, Source::Root { .. }),
            parent: None,
            selection: None,
            state: State::Unknown(String::new()),
            steps: Vec::new(),
            warnings: Vec::new(),
        };
        if self.visiting.contains(&index) {
            result.state = State::Error("the clock is its own source".into());
            return result;
        }
        self.visiting.push(index);
        result.state = self.state(clock, &mut result);
        self.visiting.pop();
        if matches!(result.state, State::Hz(_)) {
            for warning in &clock.warn {
                match warning.when.eval(self.registers) {
                    Ok(value) if value != 0.0 => result.warnings.push(warning.text.clone()),
                    Ok(_) => {}
                    Err(error) => result
                        .warnings
                        .push(format!("{}: unknown ({error})", warning.text)),
                }
            }
        }
        self.done.insert(index, result.clone());
        result
    }

    fn state(&mut self, clock: &Clock, result: &mut Evaluated) -> State {
        let Evaluated {
            parent,
            selection,
            steps,
            ..
        } = result;
        // The source first, so that a clock that is off still has its place
        // in the tree.
        let mut source = match &clock.source {
            Source::Root { hz } => {
                let given = self
                    .inputs
                    .iter()
                    .rev()
                    .find(|(name, _)| name.eq_ignore_ascii_case(&clock.name));
                match (given, hz) {
                    (Some((_, hz)), _) => {
                        steps.push("given".to_string());
                        State::Hz(*hz)
                    }
                    (None, Some(hz)) => {
                        steps.push("nominal".to_string());
                        State::Hz(*hz)
                    }
                    (None, None) => State::Unknown("frequency not given".into()),
                }
            }
            Source::From(name) => {
                *parent = Some(name.clone());
                State::Hz(0.0)
            }
            Source::Select { field, sources } => match field.eval(self.registers) {
                Err(error) => State::Error(error),
                Ok(value) => {
                    // A field value: a non-negative integer.
                    let value = value.max(0.0) as u64;
                    *selection = Some(Selection {
                        field: field.text.clone(),
                        value,
                        options: sources
                            .iter()
                            .map(|(key, name)| (*key, name.clone()))
                            .collect(),
                    });
                    match sources.get(&value) {
                        Some(name) => {
                            *parent = Some(name.clone());
                            State::Hz(0.0)
                        }
                        None => State::Unknown("this source is not described".into()),
                    }
                }
            },
        };
        if let (State::Hz(_), Some(name)) = (&source, parent.as_deref()) {
            let index = self.index(name);
            source = match self.evaluate(index).state {
                State::Hz(hz) => State::Hz(hz),
                State::Off(_) => State::Off(format!("{name} is off")),
                State::Unknown(_) => State::Unknown(format!("{name} is unknown")),
                State::Error(_) => State::Unknown(format!("{name} failed")),
            };
        }
        self.derive(clock, source, steps)
    }

    /// Apply `enable`, `mul` and `div` to the source.
    fn derive(&self, clock: &Clock, source: State, steps: &mut Vec<String>) -> State {
        if let Some(enable) = &clock.enable {
            match enable.eval(self.registers) {
                Err(error) => return State::Error(error),
                Ok(0.0) => return State::Off(enable.zero()),
                Ok(_) => {}
            }
        }
        // A divider of a clock that is off has nothing to say.
        if let State::Off(_) = source {
            return source;
        }
        let mut factor = 1.0;
        if let Some(mul) = &clock.mul {
            match mul.eval(self.registers) {
                Err(error) => return State::Error(error),
                Ok(value) => {
                    steps.push(format!("x{}", number(value)));
                    factor *= value;
                }
            }
        }
        if let Some(div) = &clock.div {
            match div.eval(self.registers) {
                Err(error) => return State::Error(error),
                Ok(0.0) => return State::Off(div.zero()),
                Ok(value) => {
                    steps.push(format!("/{}", number(value)));
                    factor /= value;
                }
            }
        }
        match source {
            State::Hz(hz) => State::Hz(hz * factor),
            other => other,
        }
    }
}

/// Read the registers and compute every clock.
pub fn evaluate(
    target: &mut dyn Target,
    tree: &Tree,
    inputs: &[(String, f64)],
) -> TargetResult<Report> {
    for (name, _) in inputs {
        let root = tree.clocks.iter().any(|clock| {
            clock.name.eq_ignore_ascii_case(name) && matches!(clock.source, Source::Root { .. })
        });
        if !root {
            let roots: Vec<&str> = tree
                .clocks
                .iter()
                .filter(|clock| matches!(clock.source, Source::Root { .. }))
                .map(|clock| clock.name.as_str())
                .collect();
            return Err(Error::new(format!(
                "{name} is not a source clock of this tree; the sources are {}",
                roots.join(", ")
            )));
        }
    }
    let registers = Registers::read(target, tree)?;
    let mut evaluator = Evaluator {
        tree,
        registers: &registers,
        inputs,
        done: HashMap::new(),
        visiting: Vec::new(),
    };
    let clocks: Vec<Evaluated> = (0..tree.clocks.len())
        .map(|index| evaluator.evaluate(index))
        .collect();
    let missing = tree
        .clocks
        .iter()
        .zip(&clocks)
        .filter(|(clock, evaluated)| {
            matches!(clock.source, Source::Root { .. })
                && matches!(evaluated.state, State::Unknown(_))
        })
        .map(|(clock, _)| clock.name.clone())
        .collect();
    Ok(Report {
        clocks,
        registers,
        missing,
        inputs_hint: "[clock] in the project configuration".to_string(),
    })
}

// -------------------------------------------------------------- rendering

impl Report {
    /// 1 when a clock could not be computed because a register or an
    /// expression failed.
    pub fn exit_code(&self) -> i32 {
        i32::from(
            self.clocks
                .iter()
                .any(|clock| matches!(clock.state, State::Error(_))),
        )
    }

    /// The clocks that run from `parent` (the source clocks for `None`), in
    /// the order of the description.
    pub fn children<'a>(&'a self, parent: Option<&'a str>) -> impl Iterator<Item = &'a Evaluated> {
        self.clocks
            .iter()
            .filter(move |clock| clock.parent.as_deref() == parent)
    }

    /// The clocks in tree order, every clock under the clock it runs from,
    /// each with the branch lines that lead to it (`│  └─ `).
    fn rows(&self) -> Vec<(String, &Evaluated)> {
        fn visit<'a>(
            report: &'a Report,
            parent: &'a Evaluated,
            lines: &str,
            rows: &mut Vec<(String, &'a Evaluated)>,
        ) {
            let children: Vec<&Evaluated> = report.children(Some(&parent.name)).collect();
            for (index, child) in children.iter().enumerate() {
                let last = index + 1 == children.len();
                let branch = if last { "└─ " } else { "├─ " };
                rows.push((format!("{lines}{branch}"), child));
                let below = if last { "   " } else { "│  " };
                visit(report, child, &format!("{lines}{below}"), rows);
            }
        }
        let mut rows = Vec::new();
        for root in self.children(None) {
            rows.push((String::new(), root));
            visit(self, root, "", &mut rows);
        }
        rows
    }

    pub fn human(&self, style: Style) -> String {
        let rows = self.rows();
        let name_width = rows
            .iter()
            .map(|(lines, clock)| lines.chars().count() + clock.name.len())
            .max()
            .unwrap_or(0);
        let values: Vec<String> = rows
            .iter()
            .map(|(_, clock)| match &clock.state {
                State::Hz(hz) => format_frequency(*hz),
                State::Off(_) => "off".to_string(),
                State::Unknown(_) => "?".to_string(),
                State::Error(_) => "error".to_string(),
            })
            .collect();
        let value_width = values.iter().map(String::len).max().unwrap_or(0);
        let mut lines = Vec::new();
        for ((branches, clock), value) in rows.iter().zip(&values) {
            let width = name_width - branches.chars().count();
            let name = format!("{:<width$}", clock.name);
            let padded = format!("{value:<value_width$}");
            let (value, reason) = match &clock.state {
                State::Hz(_) => (style.value(padded), None),
                State::Off(reason) => (style.dim(padded), Some(style.dim(format!("({reason})")))),
                State::Unknown(reason) => {
                    (style.warn(padded), Some(style.warn(format!("({reason})"))))
                }
                State::Error(reason) => (style.bad(padded), Some(reason.clone())),
            };
            let mut line = format!("{}{}  {value}", style.dim(branches), style.label(name));
            let warnings = clock
                .warnings
                .iter()
                .map(|warning| style.warn(format!("! {warning}")));
            let note = clock
                .note
                .as_ref()
                .map(|note| style.dim(format!("; {note}")));
            let how = Some(clock.how()).filter(|how| !how.is_empty());
            for part in how.into_iter().chain(reason).chain(warnings).chain(note) {
                line.push_str("  ");
                line.push_str(&part);
            }
            lines.push(line);
        }
        if !self.missing.is_empty() {
            let first = &self.missing[0];
            lines.push(style.warn(format!(
                "{} not given: pass {first}=<frequency> (e.g. {first}=40MHz) or set it under \
                 {}",
                self.missing.join(", "),
                self.inputs_hint
            )));
        }
        lines.join("\n")
    }

    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::json;
        let clocks: Vec<_> = self
            .clocks
            .iter()
            .map(|clock| {
                let (state, hz, reason) = match &clock.state {
                    State::Hz(hz) => ("on", Some(*hz), None),
                    State::Off(reason) => ("off", None, Some(reason)),
                    State::Unknown(reason) => ("unknown", None, Some(reason)),
                    State::Error(reason) => ("error", None, Some(reason)),
                };
                json!({
                    "name": clock.name,
                    "source": clock.parent,
                    "state": state,
                    "hz": hz,
                    "frequency": hz.map(format_frequency),
                    "reason": reason,
                    "how": clock.how(),
                    "warnings": clock.warnings,
                    "note": clock.note,
                    "group": clock.group,
                })
            })
            .collect();
        let registers: Vec<_> = self
            .registers
            .values
            .iter()
            .map(|(name, address, value)| {
                json!({
                    "name": name,
                    "address": address.to_string(),
                    "value": value.as_ref().ok(),
                    "hex": value.as_ref().ok().map(|value| hex32(*value)),
                    "error": value.as_ref().err(),
                })
            })
            .collect();
        json!({"clocks": clocks, "registers": registers, "missing": self.missing})
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn eval(text: &str, registers: &[(&str, u64)]) -> Result<f64, String> {
        let registers = Registers {
            values: registers
                .iter()
                .map(|(name, value)| {
                    (
                        name.to_string(),
                        TargetAddress::parse("AD:0x0").unwrap(),
                        Ok(*value),
                    )
                })
                .collect(),
        };
        Expr::parse(text)?.eval(&registers)
    }

    #[test]
    fn expressions() {
        assert_eq!(eval("2 + 3 * 4", &[]), Ok(14.0));
        assert_eq!(eval("(2 + 3) * 4", &[]), Ok(20.0));
        assert_eq!(eval("10 ^ 2 / 4", &[]), Ok(25.0));
        assert_eq!(eval("2 ^ 3 ^ 2", &[]), Ok(512.0));
        assert_eq!(eval("0x10 - 1.5", &[]), Ok(14.5));
        let dv = [("DV", 0x5001_2014)];
        assert_eq!(eval("DV[6:0]", &dv), Ok(20.0));
        assert_eq!(eval("DV[14:12] * DV[21:16]", &dv), Ok(2.0));
        assert_eq!(eval("DV[30:27]+1", &dv), Ok(11.0));
        assert_eq!(eval("DV[28]", &dv), Ok(1.0));
        assert_eq!(eval("DV", &dv), Ok(f64::from(0x5001_2014u32)));
        assert_eq!(eval("DV[2] == 1", &dv), Ok(1.0));
        assert_eq!(eval("DV[2] != 1", &dv), Ok(0.0));
        assert_eq!(
            eval("1 / DV[0]", &dv),
            Err("1 / DV[0]: division by zero".into())
        );
    }

    #[test]
    fn expression_errors_name_the_column() {
        let error = |text: &str| Expr::parse(text).unwrap_err();
        assert_eq!(error("2 +"), "expected a number or a register at column 4");
        assert_eq!(
            error("DV[3:7]"),
            "bits must be [high:low] within 31..0 at column 8"
        );
        assert_eq!(
            error("DV[32]"),
            "bits must be [high:low] within 31..0 at column 7"
        );
        assert_eq!(error("(1"), "expected ')' at column 3");
        assert_eq!(error("1 2"), "unexpected text at column 3");
        assert_eq!(error("0xZZ"), "not a number at column 5");
    }

    #[test]
    fn description_errors_name_the_clock() {
        let error = |text: &str| parse(text).unwrap_err();
        assert_eq!(error("chips = []"), "no [[clock]] entries");
        assert_eq!(
            error("[[clock]]\nname = \"A\"\nfrom = \"B\""),
            "clock A: source B is not a clock"
        );
        assert_eq!(
            error("[[clock]]\nname = \"A\"\n[[clock]]\nname = \"A\""),
            "clock A: defined twice"
        );
        assert_eq!(
            error("[[clock]]\nname = \"A\"\n[[clock]]\nname = \"B\"\nfrom = \"A\"\ndiv = \"R[1]\""),
            "clock B: R[1]: R is not in [reg]"
        );
        assert_eq!(
            error("[[clock]]\nname = \"A\"\nselect = \"1\""),
            "clock A: select needs sources = { \"<value>\" = \"<clock>\", ... }"
        );
        assert_eq!(
            error("[[clock]]\nname = \"A\"\ntypo = 1"),
            "clock A: unknown key 'typo'"
        );
        assert_eq!(
            error("[[clock]]\nname = \"A\"\nmul = \"2 *\""),
            "clock A: mul = \"2 *\": expected a number or a register at column 4"
        );
        assert_eq!(
            error("[reg]\nR = \"0x100\"\n[[clock]]\nname = \"A\""),
            "reg.R must be <access class>:<address>, e.g. \"AD:0x40001008\""
        );
        assert_eq!(
            error("[[clock]]\nname = \"A\"\nhz = \"fast\""),
            "clock A: hz: \"fast\" is not a frequency such as \"40MHz\""
        );
    }

    #[test]
    fn frequencies_are_printed_in_their_unit() {
        assert_eq!(format_frequency(400e6), "400 MHz");
        assert_eq!(format_frequency(6_048_000.0), "6.048 MHz");
        assert_eq!(format_frequency(32_768.0), "32.768 kHz");
        assert_eq!(format_frequency(50.0), "50 Hz");
        assert_eq!(format_frequency(1.2e9), "1.2 GHz");
        assert_eq!(format_frequency(40e6 / 3.0), "13.333333 MHz");
    }

    #[test]
    fn the_description_is_chosen_by_chip() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, chips: &str| {
            std::fs::write(
                dir.path().join(name),
                format!("chips = [{chips}]\n[[clock]]\nname = \"X\"\n"),
            )
            .unwrap();
        };
        write("family.toml", "\"MYCHIP*\"");
        write("exact.toml", "\"MYCHIP7\", \"OTHER\"");
        std::fs::write(dir.path().join("broken.toml"), "chips = 1").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not a description").unwrap();
        let (path, _) = find("MYCHIP7", dir.path()).unwrap();
        assert_eq!(path.file_name().unwrap(), "exact.toml");
        let (path, _) = find("mychip5", dir.path()).unwrap();
        assert_eq!(path.file_name().unwrap(), "family.toml");
        let error = find("NOPE", dir.path()).unwrap_err().message;
        assert!(
            error.starts_with("no clock tree description for chip NOPE (not usable: "),
            "{error}"
        );
        assert!(
            error.contains("broken.toml: chips must be an array of strings"),
            "{error}"
        );
        assert!(error.contains("pass --tree <file>"), "{error}");
        write("twin.toml", "\"MYCHIP*\"");
        let error = find("MYCHIP5", dir.path()).unwrap_err().message;
        assert!(
            error.contains("matches several clock tree descriptions equally well"),
            "{error}"
        );
    }
}
