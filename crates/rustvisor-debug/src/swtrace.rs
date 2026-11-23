//! Software trace of rustvisor images (`rvdbg swtrace`): the records that
//! rustvisor-sdk's `rustvisor-trace` writes, one FDX channel per writer
//! (the symbol `RUSTVISOR_TRACE` by default, a `t32_fdx::Outbox`), read
//! through the debugger, merged by time, and guest-ipc sends paired with
//! their receives.
//!
//! The record format is the sdk's `trace/FORMAT.md`, version 1: 16 bytes,
//! little-endian, `time: u64, event: u16, source: u8, flags: u8, value:
//! u32`. This module decodes it without linking the sdk (rvdbg links no
//! crate of it); the tests pin the byte layout.
//!
//! Reading consumes: the host index moves past what was read, as with
//! `rvdbg fdx`, so a second read returns only what came since.

use std::collections::{BTreeMap, HashMap};

use probecore::Memory;
use probecore::fdx::{self, Channel, Packet};

/// The channel's symbol in rustvisor images.
pub const SYMBOL: &str = "RUSTVISOR_TRACE";

/// Bytes of one record.
pub const RECORD_BYTES: usize = 16;

pub const START: u16 = 0x0001;
pub const DROPPED: u16 = 0x0002;
pub const IPC_SEND: u16 = 0x0101;
pub const IPC_RECV: u16 = 0x0102;
pub const IPC_FULL: u16 = 0x0103;
/// EL2 entered from the Guest; `flags` the vector.
pub const EL2_ENTRY: u16 = 0x1001;
/// EL2 returns to the Guest; `flags` what the entry was ([`el2_kind`]).
pub const EL2_EXIT: u16 = 0x1002;
/// `START`'s flags bit: the times are the virtual count.
pub const START_VIRTUAL: u8 = 0x80;

/// One record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    pub time: u64,
    pub event: u16,
    pub source: u8,
    pub flags: u8,
    pub value: u32,
}

impl Record {
    pub fn decode(bytes: &[u8; RECORD_BYTES]) -> Record {
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        Record {
            time: u64::from(word(0)) | u64::from(word(4)) << 32,
            event: u16::from_le_bytes([bytes[8], bytes[9]]),
            source: bytes[10],
            flags: bytes[11],
            value: word(12),
        }
    }

    /// The event's name in FORMAT.md, or its number.
    pub fn event_name(&self) -> String {
        match self.event {
            START => "START".into(),
            DROPPED => "DROPPED".into(),
            IPC_SEND => "IPC_SEND".into(),
            IPC_RECV => "IPC_RECV".into(),
            IPC_FULL => "IPC_FULL".into(),
            EL2_ENTRY => "EL2_ENTRY".into(),
            EL2_EXIT => "EL2_EXIT".into(),
            other => format!("0x{other:04X}"),
        }
    }

    /// "el2.<core>" or "vcpu<n>".
    pub fn source_name(&self) -> String {
        if self.source & 0x80 != 0 {
            format!("el2.{}", self.source & 0x7F)
        } else {
            format!("vcpu{}", self.source & 0x7F)
        }
    }
}

/// What one image's channel held.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Drained {
    /// Whether the channel was open: a traced run opened it.
    pub open: bool,
    pub records: Vec<Record>,
    /// FDX drop markers seen: at least one record lost at each.
    pub drop_markers: usize,
    /// Packets that were not 16 bytes (not records of this format).
    pub foreign: usize,
}

impl Drained {
    /// The frequency and whether the times are virtual, from `START`.
    pub fn clock(&self) -> Option<(u32, bool)> {
        self.records
            .iter()
            .find(|r| r.event == START)
            .map(|r| (r.value, r.flags & START_VIRTUAL != 0))
    }

    /// Records the writer counted as lost (its `DROPPED` records).
    pub fn reported_drops(&self) -> u64 {
        self.records
            .iter()
            .filter(|r| r.event == DROPPED)
            .map(|r| u64::from(r.value))
            .sum()
    }
}

/// The trace channels in the ELF at `path`: `symbol` itself and every
/// `symbol_<suffix>` (one per core or vCPU), with their addresses, by name.
pub fn channels(path: &std::path::Path, symbol: &str) -> Result<Vec<(String, u64)>, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let prefix = format!("{symbol}_");
    let mut found: Vec<(String, u64)> = probecore::elf::symbols(&bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .into_iter()
        .filter(|entry| entry.name == symbol || entry.name.starts_with(&prefix))
        .map(|entry| (entry.name, entry.value))
        .collect();
    found.sort();
    found.dedup();
    Ok(found)
}

/// Reads every packet now in the channel at `address`; a closed channel
/// (size 0) gives `open: false` and nothing else.
pub fn read(memory: &mut impl Memory, address: u64) -> fdx::Result<Drained> {
    let Some(size) = fdx::opened(memory, address)? else {
        return Ok(Drained::default());
    };
    let channel = Channel { address, size };
    let mut out = Drained {
        open: true,
        ..Drained::default()
    };
    for packet in fdx::drain(memory, &channel)? {
        match packet {
            Packet::Data(bytes) => match <[u8; RECORD_BYTES]>::try_from(bytes.as_slice()) {
                Ok(bytes) => out.records.push(Record::decode(&bytes)),
                Err(_) => out.foreign += 1,
            },
            Packet::Dropped => out.drop_markers += 1,
        }
    }
    Ok(out)
}

/// The name of an `EL2_EXIT`'s `flags`: what the entry was.
pub fn el2_kind(flags: u8) -> String {
    match flags {
        1 => "hvc".into(),
        2 => "irq".into(),
        3 => "sgi".into(),
        4 => "notice".into(),
        other => format!("kind{other}"),
    }
}

/// Time in EL2 per entry, in counts, by what the entry was: each
/// `EL2_EXIT` minus the `EL2_ENTRY` just before it in the same channel.
pub fn dwell(records: &[Record]) -> BTreeMap<u8, Vec<u64>> {
    let mut out: BTreeMap<u8, Vec<u64>> = BTreeMap::new();
    let mut entered = None;
    for record in records {
        match record.event {
            EL2_ENTRY => entered = Some(record.time),
            EL2_EXIT => {
                if let Some(time) = entered.take()
                    && let Some(dwell) = record.time.checked_sub(time)
                {
                    out.entry(record.flags).or_default().push(dwell);
                }
            }
            _ => {}
        }
    }
    out
}

/// Latencies of one direction, in counts of the writers' clock.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Direction {
    /// Receive time minus send time, one per paired message, in sequence
    /// order.
    pub latencies: Vec<u64>,
    /// Sends with no receive, and receives with no send.
    pub unpaired_sends: usize,
    pub unpaired_receives: usize,
    /// Receives earlier than their send: clocks not on one axis.
    pub negative: usize,
}

/// Pairs each `IPC_SEND` with the `IPC_RECV` of the same (direction,
/// sequence) in another image, by direction (the records' `flags`).
/// `records` is every image's records with the image's index.
pub fn pair(records: &[(usize, Record)]) -> BTreeMap<u8, Direction> {
    let mut sends: HashMap<(u8, u32), (usize, u64)> = HashMap::new();
    let mut receives: HashMap<(u8, u32), (usize, u64)> = HashMap::new();
    for &(image, record) in records {
        let key = (record.flags, record.value);
        match record.event {
            IPC_SEND => {
                sends.insert(key, (image, record.time));
            }
            IPC_RECV => {
                receives.insert(key, (image, record.time));
            }
            _ => {}
        }
    }
    let mut out: BTreeMap<u8, Direction> = BTreeMap::new();
    let mut keys: Vec<_> = sends.keys().chain(receives.keys()).copied().collect();
    keys.sort_unstable();
    keys.dedup();
    for key in keys {
        let direction = out.entry(key.0).or_default();
        match (sends.get(&key), receives.get(&key)) {
            (Some(&(from, sent)), Some(&(to, received))) if from != to => {
                match received.checked_sub(sent) {
                    Some(latency) => direction.latencies.push(latency),
                    None => direction.negative += 1,
                }
            }
            (Some(_), _) => direction.unpaired_sends += 1,
            (None, Some(_)) => direction.unpaired_receives += 1,
            (None, None) => {}
        }
    }
    out
}

/// Summary of latencies in nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub min: f64,
    pub median: f64,
    pub p90: f64,
    pub p99: f64,
    pub max: f64,
    pub mean: f64,
}

/// `latencies` (counts) at `frequency` Hz, summarised; `None` if empty.
pub fn summarise(latencies: &[u64], frequency: u32) -> Option<Summary> {
    if latencies.is_empty() || frequency == 0 {
        return None;
    }
    let ns = 1e9 / f64::from(frequency);
    let mut sorted = latencies.to_vec();
    sorted.sort_unstable();
    // Nearest rank: the smallest value with at least p of the samples at
    // or below it.
    let rank = |p: f64| {
        let index = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len()) - 1;
        sorted[index] as f64 * ns
    };
    Some(Summary {
        count: sorted.len(),
        min: sorted[0] as f64 * ns,
        median: rank(0.5),
        p90: rank(0.9),
        p99: rank(0.99),
        max: sorted[sorted.len() - 1] as f64 * ns,
        mean: sorted.iter().map(|&v| v as f64).sum::<f64>() / sorted.len() as f64 * ns,
    })
}

/// One CSV line per record, all images merged by time; `names[i]` is image
/// i's label.
pub fn csv(records: &[(usize, Record)], names: &[String]) -> String {
    let mut sorted = records.to_vec();
    sorted.sort_by_key(|&(image, record)| (record.time, image));
    let mut out = String::from("time,image,source,event,flags,value\n");
    for (image, record) in sorted {
        out.push_str(&format!(
            "{},{},{},{},0x{:02X},{}\n",
            record.time,
            names[image],
            record.source_name(),
            record.event_name(),
            record.flags,
            record.value
        ));
    }
    out
}

/// One channel as `rvdbg swtrace` found it.
#[derive(Debug, Clone)]
pub struct Found {
    /// "elf", "guest<n>": the image.
    pub image: String,
    pub symbol: String,
    pub address: u64,
    pub drained: Drained,
}

impl Found {
    /// "guest1" for the image's main channel, "elf:RUSTVISOR_TRACE_C2" for
    /// another.
    pub fn label(&self, main: &str) -> String {
        if self.symbol == main {
            self.image.clone()
        } else {
            format!("{}:{}", self.image, self.symbol)
        }
    }
}

/// The report: text lines and the JSON result.
pub fn report(found: &[Found], main: &str) -> (String, serde_json::Value) {
    use serde_json::json;
    let names: Vec<String> = found.iter().map(|f| f.label(main)).collect();
    let mut lines = Vec::new();
    let mut channels = Vec::new();
    let mut clocks: Vec<(u32, bool)> = Vec::new();
    for (f, name) in found.iter().zip(&names) {
        let d = &f.drained;
        let clock = d.clock();
        if let Some(clock) = clock {
            clocks.push(clock);
        }
        let mut events: BTreeMap<String, usize> = BTreeMap::new();
        for record in &d.records {
            *events.entry(record.event_name()).or_default() += 1;
        }
        lines.push(if d.open {
            format!(
                "{name:<12} 0x{:08X}  {} records, {} lost (DROPPED), {} drop marker(s){}{}",
                f.address,
                d.records.len(),
                d.reported_drops(),
                d.drop_markers,
                match clock {
                    Some((hz, virt)) => format!(
                        ", clock {hz} Hz {}",
                        if virt { "virtual" } else { "physical" }
                    ),
                    None => ", no START".into(),
                },
                if d.foreign > 0 {
                    format!(", {} packet(s) not 16 bytes", d.foreign)
                } else {
                    String::new()
                }
            )
        } else {
            format!("{name:<12} 0x{:08X}  closed (not traced)", f.address)
        });
        let mut el2 = Vec::new();
        for (kind, dwells) in dwell(&d.records) {
            let summary = clock.and_then(|(hz, _)| summarise(&dwells, hz));
            if let Some(s) = summary {
                lines.push(format!(
                    "{:<12} EL2 {:<6} {:>6} entries, ns min {:.0} median {:.0} p99 {:.0} max {:.0} mean {:.0}",
                    "",
                    el2_kind(kind),
                    s.count,
                    s.min,
                    s.median,
                    s.p99,
                    s.max,
                    s.mean
                ));
            }
            el2.push(json!({
                "kind": el2_kind(kind),
                "entries": dwells.len(),
                "dwell_ns": summary.map(|s| json!({
                    "min": s.min, "median": s.median, "p90": s.p90, "p99": s.p99,
                    "max": s.max, "mean": s.mean,
                })),
            }));
        }
        channels.push(json!({
            "image": f.image,
            "symbol": f.symbol,
            "address": format!("0x{:08X}", f.address),
            "open": d.open,
            "records": d.records.len(),
            "events": events,
            "reported_drops": d.reported_drops(),
            "drop_markers": d.drop_markers,
            "foreign_packets": d.foreign,
            "frequency": clock.map(|c| c.0),
            "virtual": clock.map(|c| c.1),
            "el2": el2,
        }));
    }
    let records: Vec<(usize, Record)> = found
        .iter()
        .enumerate()
        .flat_map(|(i, f)| f.drained.records.iter().map(move |&r| (i, r)))
        .collect();
    let frequency = clocks.first().map(|c| c.0);
    let mixed = clocks.iter().any(|&c| Some(c.0) != frequency);
    let mut directions = Vec::new();
    let paired = pair(&records);
    if !paired.is_empty() {
        if mixed {
            lines.push("the channels' clock frequencies differ: no latencies".into());
        } else {
            lines.push(format!(
                "{:<9} {:>6} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9}  unpaired send/recv",
                "direction", "count", "min ns", "median", "p90", "p99", "max", "mean"
            ));
        }
    }
    for (direction, d) in &paired {
        let summary = if mixed {
            None
        } else {
            frequency.and_then(|hz| summarise(&d.latencies, hz))
        };
        if !mixed {
            lines.push(match summary {
                Some(s) => format!(
                    "0x{direction:02X}      {:>6} {:>9.0} {:>9.0} {:>9.0} {:>9.0} {:>9.0} {:>9.0}  {}/{}{}",
                    s.count,
                    s.min,
                    s.median,
                    s.p90,
                    s.p99,
                    s.max,
                    s.mean,
                    d.unpaired_sends,
                    d.unpaired_receives,
                    if d.negative > 0 {
                        format!(", {} received before sent", d.negative)
                    } else {
                        String::new()
                    }
                ),
                None => format!(
                    "0x{direction:02X}           0 paired  {}/{}",
                    d.unpaired_sends, d.unpaired_receives
                ),
            });
        }
        directions.push(json!({
            "direction": format!("0x{direction:02X}"),
            "paired": d.latencies.len(),
            "unpaired_sends": d.unpaired_sends,
            "unpaired_receives": d.unpaired_receives,
            "negative": d.negative,
            "latency_ns": summary.map(|s| json!({
                "min": s.min, "median": s.median, "p90": s.p90, "p99": s.p99,
                "max": s.max, "mean": s.mean,
            })),
        }));
    }
    let json = json!({
        "symbol": main,
        "frequency": frequency,
        "channels": channels,
        "directions": directions,
    });
    (lines.join("\n"), json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(time: u64, event: u16, flags: u8, value: u32) -> Record {
        Record {
            time,
            event,
            source: 0,
            flags,
            value,
        }
    }

    /// The bytes of FORMAT.md's layout, as rustvisor-trace's test writes
    /// them.
    #[test]
    fn decodes_the_format_byte_for_byte() {
        let bytes = [
            1, 2, 3, 4, 5, 6, 7, 8, 9, 0x0A, 0x83, 0x0C, 0x0D, 0x0E, 0x0F, 0x10,
        ];
        let record = Record::decode(&bytes);
        assert_eq!(record.time, 0x0807_0605_0403_0201);
        assert_eq!(record.event, 0x0A09);
        assert_eq!(record.source_name(), "el2.3");
        assert_eq!(record.flags, 0x0C);
        assert_eq!(record.value, 0x100F_0E0D);
    }

    #[test]
    fn sends_pair_with_receives_of_the_other_image_by_direction_and_sequence() {
        let records = vec![
            (0, record(1000, IPC_SEND, 0x01, 0)),
            (1, record(1400, IPC_RECV, 0x01, 0)),
            (1, record(1600, IPC_SEND, 0x10, 0)),
            (0, record(2000, IPC_RECV, 0x10, 0)),
            (0, record(3000, IPC_SEND, 0x01, 1)),
            (1, record(3200, IPC_RECV, 0x01, 1)),
            // Sent, never received.
            (0, record(5000, IPC_SEND, 0x01, 2)),
            // Received with no send in the trace (the send was dropped).
            (0, record(5200, IPC_RECV, 0x10, 7)),
        ];
        let directions = pair(&records);
        assert_eq!(directions[&0x01].latencies, [400, 200]);
        assert_eq!(directions[&0x01].unpaired_sends, 1);
        assert_eq!(directions[&0x10].latencies, [400]);
        assert_eq!(directions[&0x10].unpaired_receives, 1);
    }

    #[test]
    fn a_send_and_receive_in_one_image_do_not_pair() {
        let records = vec![
            (0, record(1, IPC_SEND, 1, 0)),
            (0, record(2, IPC_RECV, 1, 0)),
        ];
        assert_eq!(pair(&records)[&1].unpaired_sends, 1);
    }

    #[test]
    fn el2_dwell_pairs_each_exit_with_the_entry_before_it() {
        let records = [
            record(100, START, 1, 1_000_000_000),
            record(1000, EL2_ENTRY, 0x18, 0),
            record(1400, EL2_EXIT, 2, 0),
            // An exit without its entry (the entry was dropped).
            record(1500, EL2_EXIT, 2, 0),
            record(2000, EL2_ENTRY, 0x14, 0),
            record(2600, EL2_EXIT, 1, 5),
        ];
        let dwells = dwell(&records);
        assert_eq!(dwells[&2], [400]);
        assert_eq!(dwells[&1], [600]);
        assert_eq!(el2_kind(1), "hvc");
    }

    #[test]
    fn summaries_are_in_nanoseconds() {
        // 1 GHz: counts are nanoseconds.
        let latencies: Vec<u64> = (1..=100).map(|k| k * 200).collect();
        let summary = summarise(&latencies, 1_000_000_000).unwrap();
        assert_eq!(summary.count, 100);
        assert_eq!(summary.min, 200.0);
        assert_eq!(summary.median, 10_000.0);
        assert_eq!(summary.p90, 18_000.0);
        assert_eq!(summary.p99, 19_800.0);
        assert_eq!(summary.max, 20_000.0);
        assert_eq!(summary.mean, 10_100.0);
        assert_eq!(summarise(&[], 1), None);
    }

    #[test]
    fn csv_merges_images_by_time() {
        let records = vec![
            (1, record(20, IPC_RECV, 1, 0)),
            (0, record(10, IPC_SEND, 1, 0)),
        ];
        let text = csv(&records, &["guest0".into(), "guest1".into()]);
        assert_eq!(
            text,
            "time,image,source,event,flags,value\n\
             10,guest0,vcpu0,IPC_SEND,0x01,0\n\
             20,guest1,vcpu0,IPC_RECV,0x01,0\n"
        );
    }

    /// RAM from 0x100 holding one channel that a target opened and filled.
    struct Ram(Vec<u8>);

    impl Memory for Ram {
        fn read(&mut self, address: u64, length: usize) -> probecore::Result<Vec<u8>> {
            let at = (address - 0x100) as usize;
            Ok(self.0[at..at + length].to_vec())
        }
        fn read_u32s(&mut self, address: u64, count: usize) -> probecore::Result<Vec<u32>> {
            let bytes = self.read(address, 4 * count)?;
            Ok(bytes
                .chunks_exact(4)
                .map(|w| u32::from_le_bytes(w.try_into().unwrap()))
                .collect())
        }
        fn write(&mut self, _: u64, _: &[u8]) -> probecore::Result<()> {
            unreachable!()
        }
        fn write_u32(&mut self, address: u64, value: u32) -> probecore::Result<()> {
            let at = (address - 0x100) as usize;
            self.0[at..at + 4].copy_from_slice(&value.to_le_bytes());
            Ok(())
        }
    }

    #[test]
    fn a_closed_channel_reads_as_not_open() {
        let mut ram = Ram(vec![0; 32 + 64]);
        assert_eq!(read(&mut ram, 0x100).unwrap(), Drained::default());
    }

    #[test]
    fn reads_records_and_drop_markers() {
        // A channel of 64 bytes as the target leaves it: two 16-byte
        // packets (length byte 16, payload, then the 0 that ends the ring's
        // filled part), a drop marker, targetindex after it.
        let mut ram = Ram(vec![0; 32 + 64]);
        let header = [0u32, 64, 0, 35, 1, 0, 0, 1];
        for (i, word) in header.iter().enumerate() {
            ram.write_u32(0x100 + 4 * i as u64, *word).unwrap();
        }
        let first = [1u8, 0, 0, 0, 0, 0, 0, 0, 1, 1, 0, 1, 7, 0, 0, 0];
        let second = [2u8, 0, 0, 0, 0, 0, 0, 0, 2, 1, 0, 1, 7, 0, 0, 0];
        ram.0[32] = 16;
        ram.0[33..49].copy_from_slice(&first);
        ram.0[49] = 16;
        ram.0[50..66].copy_from_slice(&second);
        ram.0[66] = 0xFD;
        ram.0[67] = 0;
        let channel = read(&mut ram, 0x100).unwrap();
        assert!(channel.open);
        assert_eq!(channel.records.len(), 2);
        assert_eq!(channel.records[0].event, IPC_SEND);
        assert_eq!(channel.records[1].event, IPC_RECV);
        assert_eq!(channel.drop_markers, 1);
    }
}
