//! The VM state timeline (`rvdbg swtrace --html`): the records of one read
//! as data of the format `rustvisor-timeline/3`, embedded in a page that
//! draws it (`assets/timeline.html`, self-contained, no network).
//!
//! One panel per VM: its core's EL2 channel gives the EL2 blocks (an
//! `EL2_EXIT` with the `EL2_ENTRY` before it, by what the entry was), the
//! time between them is EL1; its guest's channel gives the IPC records,
//! and every `IPC_SEND` with the `IPC_RECV` of the same (direction,
//! sequence) is a message linked across panels. Times are nanoseconds from
//! the first record.
//!
//! The records say neither which VM a core runs nor which core a guest is
//! on: a [`Map`] does (`[swtrace] map` in rvdbg.toml, `--map`). Without
//! it each channel is a panel of its own.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{Value, json};

use crate::swtrace::{self, Found, Record};

/// The data format the page reads.
pub const FORMAT: &str = "rustvisor-timeline/3";

/// The page; its data goes into [`DATA_TAG`].
const TEMPLATE: &str = include_str!("../assets/timeline.html");
const DATA_TAG: &str = r#"<script id="timeline-data" type="application/json">null</script>"#;

/// Which VM (and core) a channel belongs to: `C0C0 → VM1` for the EL2
/// channel `<symbol>_C0C0`, `guest0 → VM1:C0C0` for an image's channel.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Map(BTreeMap<String, String>);

impl Map {
    /// `KEY=VM` or `KEY=VM:CORE` items; later ones replace earlier ones.
    pub fn parse<'a>(items: impl IntoIterator<Item = &'a str>) -> Result<Map, String> {
        let mut map = Map::default();
        for item in items {
            let (key, value) = item
                .split_once('=')
                .filter(|(key, value)| !key.is_empty() && !value.is_empty())
                .ok_or_else(|| format!("{item:?}: expected KEY=VM or KEY=VM:CORE"))?;
            map.0
                .insert(key.trim().to_string(), value.trim().to_string());
        }
        Ok(map)
    }

    /// `self` with `other`'s entries on top.
    pub fn merged(mut self, other: &Map) -> Map {
        self.0
            .extend(other.0.iter().map(|(k, v)| (k.clone(), v.clone())));
        self
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// "C0C0=VM1, guest0=VM1:C0C0".
    pub fn describe(&self) -> String {
        self.0
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// (VM, core, ring name, whether it is a core's EL2 channel) of a
    /// channel. An EL2 channel is `<main>_<core>` with records from EL2.
    fn unit(&self, found: &Found, main: &str) -> (String, String, String, bool) {
        let label = found.label(main);
        let el2 = found.drained.records.iter().any(|r| r.source & 0x80 != 0);
        let suffix = found
            .symbol
            .strip_prefix(main)
            .and_then(|s| s.strip_prefix('_'))
            .filter(|_| el2);
        match suffix {
            Some(core) => {
                let vm = self
                    .0
                    .get(core)
                    .map_or(core, |v| v.split(':').next().unwrap_or(v));
                (
                    vm.to_string(),
                    core.to_string(),
                    format!("EL2 ring {core}"),
                    true,
                )
            }
            None => {
                let (vm, core) = match self.0.get(&label) {
                    Some(value) => match value.split_once(':') {
                        Some((vm, core)) => (vm.to_string(), core.to_string()),
                        None => (value.clone(), "?".to_string()),
                    },
                    None => (label.clone(), "?".to_string()),
                };
                (vm, core, format!("{label} ring"), false)
            }
        }
    }
}

/// The page's texts for one read.
#[derive(Debug, Clone)]
pub struct Texts {
    /// The run's name, e.g. the CSV's stem.
    pub name: String,
    /// Where the records came from, e.g. "board, 7 channels" or a file name.
    pub source: String,
}

/// What an `EL2_EXIT`'s flags say the entry was (debug/swtrace/FORMAT.md).
fn reason(flags: u8) -> String {
    match flags {
        1 => "hvc".into(),
        2 => "IRQ".into(),
        3 => "SGI".into(),
        4 => "notice".into(),
        _ => "?".into(),
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// The data of the page for the channels of one read.
pub fn data(found: &[Found], main: &str, map: &Map, texts: &Texts) -> Result<Value, String> {
    let clocks: BTreeSet<u32> = found
        .iter()
        .filter_map(|f| f.drained.clock())
        .map(|(hz, _)| hz)
        .collect();
    let frequency = match clocks.len() {
        1 => *clocks.first().unwrap(),
        0 => {
            return Err(
                "no START record, so the clock frequency is unknown: START is only in a \
                        channel's first read (draw an earlier read's CSV with --from)"
                    .into(),
            );
        }
        _ => {
            return Err(format!(
                "the channels' clock frequencies differ: {clocks:?} Hz"
            ));
        }
    };
    if frequency == 0 {
        return Err("START gives a clock frequency of 0 Hz".into());
    }

    struct Unit {
        vm: String,
        core: String,
        rings: BTreeSet<String>,
        el2_ring: bool,
        records: Vec<(Record, bool)>,
    }
    let mut units: BTreeMap<String, Unit> = BTreeMap::new();
    let mut all: Vec<u64> = Vec::new();
    for f in found {
        if f.drained.records.is_empty() {
            continue;
        }
        let (vm, core, ring, el2) = map.unit(f, main);
        let unit = units.entry(vm.clone()).or_insert_with(|| Unit {
            vm,
            core: core.clone(),
            rings: BTreeSet::new(),
            el2_ring: false,
            records: Vec::new(),
        });
        if el2 {
            unit.core = core;
            unit.el2_ring = true;
        }
        unit.rings.insert(ring);
        unit.records
            .extend(f.drained.records.iter().map(|&r| (r, el2)));
        all.extend(f.drained.records.iter().map(|r| r.time));
    }
    let (Some(&t0), Some(&t1)) = (all.iter().min(), all.iter().max()) else {
        return Err("the channels hold no records".into());
    };
    let ns = |time: u64| -> u64 {
        (u128::from(time.saturating_sub(t0)) * 1_000_000_000 / u128::from(frequency)) as u64
    };
    let resolution = all.iter().fold(0, |g, &t| gcd(g, ns(t)));

    let mut sends: HashMap<(u8, u32), (String, u64)> = HashMap::new();
    let mut receives: HashMap<(u8, u32), (String, u64)> = HashMap::new();
    let mut out_units = Vec::new();
    for unit in units.values_mut() {
        unit.records.sort_by_key(|(r, _)| r.time);
        // Time counts from the EL2 channel's START when there is one,
        // else from the guest channel's.
        let base: Vec<&Record> = unit
            .records
            .iter()
            .filter(|(_, el2)| *el2 == unit.el2_ring)
            .map(|(r, _)| r)
            .collect();
        let start = base
            .iter()
            .find(|r| r.event == swtrace::START)
            .or(base.first())
            .map_or(t0, |r| r.time);
        let mut segs: Vec<Value> = Vec::new();
        if start > t0 {
            segs.push(json!([0, ns(start), "unknown", ""]));
        }
        let mut cursor = start;
        let mut entry: Option<Record> = None;
        for (r, _) in &unit.records {
            match r.event {
                swtrace::EL2_ENTRY => {
                    if r.time > cursor {
                        segs.push(json!([ns(cursor), ns(r.time), "el1", "app"]));
                    }
                    entry = Some(*r);
                }
                swtrace::EL2_EXIT => {
                    if let Some(e) = entry.take() {
                        let why = reason(r.flags);
                        let mut extra = json!({ "vec": e.flags });
                        if why == "hvc" {
                            extra["detail"] = json!(format!("r0=0x{:08X}", r.value));
                        }
                        segs.push(json!([ns(e.time), ns(r.time), "el2", why, extra]));
                        cursor = r.time;
                    }
                }
                swtrace::IPC_SEND => {
                    sends.insert((r.flags, r.value), (unit.vm.clone(), ns(r.time)));
                }
                swtrace::IPC_RECV => {
                    receives.insert((r.flags, r.value), (unit.vm.clone(), ns(r.time)));
                }
                _ => {}
            }
        }
        if t1 > cursor {
            segs.push(json!([ns(cursor), ns(t1), "el1", "app"]));
        }
        out_units.push(json!({
            "vm": unit.vm,
            "core": unit.core,
            "ring": unit.rings.iter().cloned().collect::<Vec<_>>().join(" + "),
            "segs": segs,
        }));
    }
    let mut msgs: Vec<(u64, u8, u32, String, String, u64)> = sends
        .iter()
        .filter_map(|(key, (from, sent))| {
            let (to, received) = receives.get(key)?;
            Some((*sent, key.0, key.1, from.clone(), to.clone(), *received))
        })
        .collect();
    msgs.sort();
    let unpaired = sends.len() + receives.len() - 2 * msgs.len();

    let mut doc = json!({
        "format": FORMAT,
        "title": "VM State Timeline",
        "lede": "One panel per VM (or core), rows by exception level: EL1 (the guest) on top, \
                 EL2 below; the step line follows the level. IPC records are marked on the EL1 \
                 row and linked across panels.",
        "meta": [
            {"label": "Source", "text": format!("rvdbg swtrace · {}", texts.source)},
            {"label": "Clock", "text": format!("{frequency} Hz")},
        ],
        "footnote": if map.is_empty() {
            "No VM map: each channel is its own panel. [swtrace] map in rvdbg.toml or --map \
             KEY=VM[:CORE] says which VM a core runs and which VM and core an image belongs to."
                .to_string()
        } else {
            format!("VM map: {}. Times are from the first record.", map.describe())
        },
        "runs": [{
            "key": "run",
            "name": texts.name,
            "note": "",
            "source": texts.source,
            "t0": t0,
            "span": ns(t1),
            "units": out_units,
            "msgs": msgs
                .iter()
                .map(|(sent, dir, seq, from, to, received)| json!([dir, seq, from, to, sent, received]))
                .collect::<Vec<_>>(),
            "unpaired": unpaired,
        }],
    });
    if resolution > 0 {
        doc["resolution_ns"] = json!(resolution);
    }
    Ok(doc)
}

/// The page with `doc` embedded.
pub fn page(doc: &Value) -> String {
    let data = doc.to_string().replace("</", "<\\/");
    TEMPLATE.replace(DATA_TAG, &DATA_TAG.replace(">null<", &format!(">{data}<")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swtrace::{Drained, EL2_ENTRY, EL2_EXIT, IPC_RECV, IPC_SEND, START};

    fn record(time: u64, event: u16, flags: u8, value: u32) -> Record {
        Record {
            time,
            event,
            source: 0,
            flags,
            value,
        }
    }

    fn channel(image: &str, symbol: &str, mut records: Vec<Record>) -> Found {
        if symbol != MAIN {
            for r in &mut records {
                r.source = 0x80;
            }
        }
        Found {
            image: image.into(),
            symbol: symbol.into(),
            address: 0,
            drained: Drained {
                open: true,
                records,
                ..Drained::default()
            },
        }
    }

    const MAIN: &str = "RUSTVISOR_TRACE";

    fn texts() -> Texts {
        Texts {
            name: "t".into(),
            source: "test".into(),
        }
    }

    #[test]
    fn the_template_has_the_data_tag() {
        assert!(TEMPLATE.contains(DATA_TAG));
        let page = page(&json!({"a": "</script>"}));
        assert!(page.contains(r#"{"a":"<\/script>"}"#));
    }

    #[test]
    fn a_map_parses_and_later_items_win() {
        let map = Map::parse(["C0C0=VM1", "guest0=VM1:C0C0", "C0C0=VM9"]).unwrap();
        assert_eq!(map.describe(), "C0C0=VM9, guest0=VM1:C0C0");
        assert!(Map::parse(["C0C0"]).is_err());
        assert!(Map::parse(["=VM1"]).is_err());
    }

    /// A VM: its core's EL2 channel and its guest's channel, at 500 MHz
    /// (one count is 2 ns); a second VM receives.
    #[test]
    fn channels_become_panels_blocks_and_messages() {
        let found = vec![
            channel(
                "elf",
                "RUSTVISOR_TRACE_C0C0",
                vec![
                    record(100, START, 0, 500_000_000),
                    record(150, EL2_ENTRY, 0x14, 0),
                    record(175, EL2_EXIT, 1, 7),
                    record(200, EL2_ENTRY, 0x18, 0),
                    record(210, EL2_EXIT, 2, 0),
                ],
            ),
            channel(
                "guest0",
                MAIN,
                vec![
                    record(90, START, 0x80, 500_000_000),
                    record(160, IPC_SEND, 1, 0),
                    record(220, IPC_SEND, 1, 1),
                ],
            ),
            channel(
                "guest1",
                MAIN,
                vec![record(180, IPC_RECV, 1, 0), record(230, IPC_RECV, 2, 0)],
            ),
        ];
        let map = Map::parse(["C0C0=VM1", "guest0=VM1:C0C0", "guest1=VM2:C0C1"]).unwrap();
        let doc = data(&found, MAIN, &map, &texts()).unwrap();
        let run = &doc["runs"][0];
        assert_eq!(doc["format"], FORMAT);
        assert_eq!(run["t0"], 90);
        assert_eq!(run["span"], 280);
        assert_eq!(doc["resolution_ns"], 10);
        let vm1 = &run["units"][0];
        assert_eq!(vm1["vm"], "VM1");
        assert_eq!(vm1["core"], "C0C0");
        assert_eq!(vm1["ring"], "EL2 ring C0C0 + guest0 ring");
        assert_eq!(
            vm1["segs"],
            json!([
                [0, 20, "unknown", ""],
                [20, 120, "el1", "app"],
                [120, 170, "el2", "hvc", {"vec": 0x14, "detail": "r0=0x00000007"}],
                [170, 220, "el1", "app"],
                [220, 240, "el2", "IRQ", {"vec": 0x18}],
                [240, 280, "el1", "app"],
            ])
        );
        let vm2 = &run["units"][1];
        assert_eq!(
            (vm2["vm"].as_str(), vm2["core"].as_str()),
            (Some("VM2"), Some("C0C1"))
        );
        // No START in guest1's channel: its time counts from its first record.
        assert_eq!(
            vm2["segs"],
            json!([[0, 180, "unknown", ""], [180, 280, "el1", "app"]])
        );
        assert_eq!(run["msgs"], json!([[1, 0, "VM1", "VM2", 140, 180]]));
        assert_eq!(run["unpaired"], 2);
    }

    #[test]
    fn without_a_map_each_channel_is_a_panel() {
        let found = vec![
            channel(
                "elf",
                "RUSTVISOR_TRACE_C1C0",
                vec![record(0, START, 0, 1_000_000_000)],
            ),
            channel("guest2", MAIN, vec![record(5, START, 0x80, 1_000_000_000)]),
        ];
        let doc = data(&found, MAIN, &Map::default(), &texts()).unwrap();
        let names: Vec<&str> = doc["runs"][0]["units"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u["vm"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["C1C0", "guest2"]);
    }

    #[test]
    fn the_clock_must_be_known_and_shared() {
        let none = vec![channel("guest0", MAIN, vec![record(1, IPC_SEND, 1, 0)])];
        assert!(
            data(&none, MAIN, &Map::default(), &texts())
                .unwrap_err()
                .starts_with("no START")
        );
        let two = vec![
            channel("guest0", MAIN, vec![record(1, START, 0, 1)]),
            channel("guest1", MAIN, vec![record(1, START, 0, 2)]),
        ];
        assert!(
            data(&two, MAIN, &Map::default(), &texts())
                .unwrap_err()
                .contains("differ")
        );
    }
}
