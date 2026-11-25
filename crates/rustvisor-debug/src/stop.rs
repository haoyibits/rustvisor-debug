//! The hypervisor's stop records: why a core of a rustvisor image stopped,
//! read from RAM while the other cores run.
//!
//! An EL2 image has `RUSTVISOR_STOP_RECORDS`, one 0x40-byte `StopSlot` per
//! core slot, each starting with a version-1 `StopRecord` of 12 words
//! (rustvisor-sdk `el2/hal/src/diagnostic.rs`, slots in the BSP's
//! `record.rs`, `docs/17-debug-abi.md`). The decoding follows the sdk's
//! offline decoder `tools/t32/offline/stop_decode.py`; kinds, codes and
//! detail words that it names are named here the same way.

use std::path::Path;

use probecore::Memory;
use serde_json::{Value, json};

/// The symbol of the stop slots in an EL2 image.
pub const SYMBOL: &str = "RUSTVISOR_STOP_RECORDS";
/// Bytes of one slot.
pub const SLOT: u64 = 0x40;
/// Words of one record.
pub const WORDS: usize = 12;
/// A complete version-1 record ("ST", version 1).
pub const MAGIC: u32 = 0x5354_0001;
/// A record being written.
pub const WRITING: u32 = 0x5354_FFFF;

/// Where the slots are: their address and how many.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slots {
    pub address: u64,
    pub count: usize,
}

/// The slots of the image at `path`, `None` if it has no stop records.
pub fn slots(path: &Path) -> Result<Option<Slots>, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let symbol = probecore::elf::symbols(&bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .into_iter()
        .find(|entry| entry.name == SYMBOL);
    Ok(symbol.map(|entry| Slots {
        address: entry.value,
        count: usize::try_from(entry.size / SLOT).unwrap_or(0).max(1),
    }))
}

/// One slot as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    /// No stop since the image's startup initialised it.
    Empty,
    /// A core started writing and did not finish.
    Incomplete,
    /// Neither 0 nor a known marker: not a record (or not initialised yet).
    Invalid(u32),
    Complete(Stop),
}

/// A complete record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stop {
    /// `C<cluster>C<core>`.
    pub core: String,
    pub kind: u32,
    pub code: u32,
    pub detail: [u32; 8],
}

/// Reads every slot.
pub fn read(memory: &mut impl Memory, slots: Slots) -> probecore::Result<Vec<Record>> {
    (0..slots.count)
        .map(|n| {
            let words = memory.read_u32s(slots.address + SLOT * n as u64, WORDS)?;
            Ok(decode(&words))
        })
        .collect()
}

/// The record in 12 words.
pub fn decode(words: &[u32]) -> Record {
    let [magic, kind, core, code, ..] = *words else {
        return Record::Invalid(0);
    };
    if words.len() < WORDS {
        return Record::Invalid(0);
    }
    match magic {
        0 => Record::Empty,
        WRITING => Record::Incomplete,
        MAGIC => {
            let mut detail = [0; 8];
            for (slot, word) in detail.iter_mut().zip(words.iter().skip(4)) {
                *slot = *word;
            }
            Record::Complete(Stop {
                core: core_name(core),
                kind,
                code,
                detail,
            })
        }
        other => Record::Invalid(other),
    }
}

/// A `core_key` word: cluster in bits 15:8, core in 7:0.
pub fn core_name(key: u32) -> String {
    format!("C{}C{}", key >> 8, key & 0xFF)
}

/// `StopKind` (hal `diagnostic.rs`); 7 is retired (`LegacyBoot`).
pub fn kind_name(kind: u32) -> Option<&'static str> {
    Some(match kind {
        1 => "Boot",
        2 => "Fault",
        3 => "UnownedIrq",
        4 => "InjectFailed",
        5 => "Panic",
        6 => "Unbound",
        7 => "LegacyBoot",
        8 => "PeerStop",
        9 => "SgiRefused",
        10 => "SgiUndelivered",
        _ => return None,
    })
}

/// `BootCode` (runtime `diagnostic.rs`).
fn boot_name(code: u32) -> Option<&'static str> {
    Some(match code {
        1 => "NotPrimary",
        2 => "Plan",
        3 => "Init",
        4 => "Route",
        5 => "Prepare",
        6 => "Start",
        7 => "Secondary",
        8 => "NotBound",
        9 => "Local",
        10 => "Aborted",
        11 => "Devices",
        12 => "SharedRegions",
        _ => return None,
    })
}

fn decision(word: u32) -> &'static str {
    match word {
        1 => "Halt",
        2 => "StopGuest",
        _ => "unknown",
    }
}

fn hex(word: u32) -> String {
    format!("0x{word:08X}")
}

impl Stop {
    /// The kind's name, `unknown-<n>` for one this rvdbg does not know.
    pub fn kind_name(&self) -> String {
        kind_name(self.kind).map_or_else(|| format!("unknown-{}", self.kind), str::to_string)
    }

    /// The program counter of a `Fault`, to name with the images' symbols.
    pub fn pc(&self) -> Option<u32> {
        (self.kind == 2).then_some(self.detail[1])
    }

    /// The fields `stop_decode.py` gives for this kind, as JSON.
    pub fn fields(&self) -> Value {
        let d = &self.detail;
        let mut out = json!({
            "core": self.core,
            "kind": self.kind_name(),
            "code": self.code,
            "detail": d.iter().map(|w| hex(*w)).collect::<Vec<_>>(),
        });
        let extra = match self.kind {
            1 => {
                let failure = boot_name(self.code)
                    .map_or_else(|| format!("unknown-{}", self.code), str::to_string);
                let mut boot = json!({"boot_failure": failure});
                match failure.as_str() {
                    "Init" | "Route" | "Local" | "Devices" | "SharedRegions" => {
                        boot["stage"] = json!(d[0]);
                        boot["failure_code"] = json!(d[1]);
                    }
                    "Prepare" => boot["image_error"] = json!(d[0]),
                    "Start" => {
                        boot["target"] = json!(core_name(d[0]));
                        boot["stage"] = json!(d[1]);
                        boot["failure_code"] = json!(d[2]);
                    }
                    "NotPrimary" | "NotBound" | "Secondary" => {
                        boot["target"] = json!(core_name(d[0]))
                    }
                    "Plan" => boot["plan_error"] = json!(d[0]),
                    _ => {}
                }
                boot
            }
            2 => json!({
                "source": match d[0] { 1 => "Hypervisor", 2 => "Guest", _ => "unknown" },
                "fault": match self.code {
                    1 => "Read", 2 => "Write", 3 => "Execute", 4 => "Trapped", 5 => "Other",
                    _ => "unknown",
                },
                "pc": hex(d[1]),
                "address": (d[3] != 0).then(|| hex(d[2])),
                "syndrome": hex(d[4]),
                "decision": decision(d[5]),
                "vm": d[6],
            }),
            3 | 4 => {
                let mut irq = json!({"irq": self.code, "decision": decision(d[0]), "vm": d[1]});
                if self.kind == 4 {
                    irq["failure"] = json!(if d[2] == 1 {
                        "NoListRegister"
                    } else {
                        "unknown"
                    });
                }
                irq
            }
            5 => json!({"line": d[0], "column": d[1], "file_hash": hex(d[2])}),
            8 => json!({"decision": decision(d[0]), "vm": d[1]}),
            9 => json!({
                "refusal": match self.code {
                    1 => "ReservedSgi", 2 => "NoTarget", 3 => "OutsideVm", 4 => "NotOwned",
                    _ => "unknown",
                },
                "decision": decision(d[0]),
                "vm": d[1],
                "sgi": d[2],
            }),
            10 => json!({
                "sgi": self.code,
                "decision": decision(d[0]),
                "vm": d[1],
                "target": core_name(d[2]),
            }),
            7 => json!({"stage": self.code >> 16, "failure_code": self.code & 0xFFFF}),
            _ => json!({}),
        };
        if let (Value::Object(out), Value::Object(extra)) = (&mut out, extra) {
            out.extend(extra);
        }
        out
    }

    /// One line: `Fault: Guest Write, pc 0x28010020, syndrome 0x92000251,
    /// StopGuest, VM 1`. `symbol` names an address (`_start`).
    pub fn summary(&self, symbol: &dyn Fn(u32) -> Option<String>) -> String {
        let fields = self.fields();
        let text = |key: &str| fields[key].as_str().unwrap_or("").to_string();
        let named = |address: u32| match symbol(address) {
            Some(name) => format!("{} ({name})", hex(address)),
            None => hex(address),
        };
        let d = &self.detail;
        let what = match self.kind {
            1 => {
                let mut line = format!("boot failure {}", text("boot_failure"));
                for key in ["target", "plan_error"] {
                    if let Some(value) = fields[key].as_str() {
                        line.push_str(&format!(", {key} {value}"));
                    }
                }
                for key in ["stage", "failure_code", "image_error"] {
                    if let Some(value) = fields[key].as_u64() {
                        line.push_str(&format!(", {key} {value}"));
                    }
                }
                line
            }
            2 => {
                let mut line = format!(
                    "{} {} fault, pc {}",
                    text("source"),
                    text("fault"),
                    named(d[1])
                );
                if d[3] != 0 {
                    line.push_str(&format!(", address {}", hex(d[2])));
                }
                line.push_str(&format!(
                    ", syndrome {}, {}, VM {}",
                    hex(d[4]),
                    text("decision"),
                    d[6]
                ));
                line
            }
            3 | 4 => format!(
                "IRQ {}{}, {}, VM {}",
                self.code,
                if self.kind == 4 {
                    " not injected"
                } else {
                    " has no owner"
                },
                text("decision"),
                d[1]
            ),
            5 => format!(
                "panic at line {} column {} (file hash {})",
                d[0],
                d[1],
                hex(d[2])
            ),
            8 => format!("another core of VM {} stopped, {}", d[1], text("decision")),
            9 => format!(
                "SGI {} refused ({}), {}, VM {}",
                d[2],
                text("refusal"),
                text("decision"),
                d[1]
            ),
            10 => format!(
                "SGI {} not delivered to {}, {}, VM {}",
                self.code,
                text("target"),
                text("decision"),
                d[1]
            ),
            _ => format!("code {}, detail {:?}", self.code, fields["detail"]),
        };
        format!("{}: {what}", self.kind_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete(words: [u32; 12]) -> Stop {
        match decode(&words) {
            Record::Complete(stop) => stop,
            other => panic!("{other:?}"),
        }
    }

    /// The payloads `stop_decode.py --check` decodes, with its answers.
    #[test]
    fn records_decode_as_the_sdk_decoder_does() {
        assert_eq!(decode(&[0; 12]), Record::Empty);
        let mut writing = [0; 12];
        writing[0] = WRITING;
        assert_eq!(decode(&writing), Record::Incomplete);
        assert_eq!(decode(&[7; 12]), Record::Invalid(7));

        let boot = complete([MAGIC, 1, 0, 6, 0x201, 9, 8, 0, 0, 0, 0, 0]);
        let fields = boot.fields();
        assert_eq!(fields["boot_failure"], "Start");
        assert_eq!(fields["target"], "C2C1");
        assert_eq!(fields["failure_code"], 8);

        let fault = complete([MAGIC, 2, 0, 1, 2, 0x28010020, 0, 1, 0x9000000C, 2, 3, 0]);
        let fields = fault.fields();
        assert_eq!(fields["address"], "0x00000000");
        assert_eq!(fields["vm"], 3);
        assert_eq!(fields["decision"], "StopGuest");
        assert_eq!(fields["source"], "Guest");
        assert_eq!(fault.pc(), Some(0x28010020));
        let name = |address: u32| (address == 0x28010020).then(|| "_start".to_string());
        assert_eq!(
            fault.summary(&name),
            "Fault: Guest Read fault, pc 0x28010020 (_start), address 0x00000000, \
             syndrome 0x9000000C, StopGuest, VM 3"
        );

        let inject = complete([MAGIC, 4, 0, 27, 1, 0, 1, 0, 0, 0, 0, 0]);
        assert_eq!(inject.fields()["failure"], "NoListRegister");
        assert_eq!(inject.fields()["decision"], "Halt");

        let peer = complete([MAGIC, 8, 0, 0, 2, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            peer.summary(&|_| None),
            "PeerStop: another core of VM 1 stopped, StopGuest"
        );

        let refused = complete([MAGIC, 9, 0, 3, 2, 2, 3, 1, 0x00010001, 1, 0x100, 1]);
        assert_eq!(refused.fields()["refusal"], "OutsideVm");
        assert_eq!(refused.fields()["sgi"], 3);

        let undelivered = complete([MAGIC, 10, 0, 3, 1, 0, 0x100, 1, 0, 0, 0, 0]);
        assert_eq!(undelivered.fields()["target"], "C1C0");

        let legacy = complete([MAGIC, 7, 0, 0x000D0001, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(legacy.fields()["stage"], 13);
        assert_eq!(legacy.fields()["failure_code"], 1);
        assert_eq!(
            complete([MAGIC, 99, 0x201, 0, 0, 0, 0, 0, 0, 0, 0, 0]).kind_name(),
            "unknown-99"
        );
        assert_eq!(
            complete([MAGIC, 99, 0x201, 0, 0, 0, 0, 0, 0, 0, 0, 0]).core,
            "C2C1"
        );
    }

    #[test]
    fn a_short_record_is_invalid() {
        assert_eq!(decode(&[MAGIC]), Record::Invalid(0));
    }
}
