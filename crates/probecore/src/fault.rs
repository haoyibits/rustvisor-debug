//! The AArch32 Hyp fault report: which vector the core stopped at, the
//! decoded HSR, the fault address and the return state. The backends read
//! the registers and look up the symbols; the report is the same text and
//! JSON for each of them.

use serde_json::{Value, json};

use crate::decode::{self, FaultAddress};
use crate::style::Style;
use crate::target::hex32;

/// The Hyp registers of the report as check files write them.
pub const HSR: &str = "C15:0x4025";
pub const HDFAR: &str = "C15:0x4006";
pub const HIFAR: &str = "C15:0x4206";
pub const HVBAR: &str = "C15:0x400C";

/// A symbol an address belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolRef {
    /// The backend's symbol path, e.g. TRACE32's `\\app\Global\main`, or the
    /// plain name from an ELF symbol table.
    pub path: String,
    pub offset: Option<u64>,
}

impl SymbolRef {
    /// The last element of the symbol path.
    pub fn short_name(&self) -> &str {
        self.path
            .rsplit('\\')
            .find(|part| !part.is_empty())
            .unwrap_or(&self.path)
    }

    pub fn describe(&self) -> String {
        match self.offset {
            Some(0) | None => self.short_name().to_string(),
            Some(offset) => format!("{}+0x{offset:X}", self.short_name()),
        }
    }
}

pub fn symbol_json(symbol: &Option<SymbolRef>) -> Value {
    match symbol {
        Some(symbol) => json!({
            "path": symbol.path,
            "name": symbol.short_name(),
            "offset": symbol.offset,
            "text": symbol.describe(),
        }),
        None => Value::Null,
    }
}

/// A value with its symbol, or `(no symbol)`.
pub fn with_symbol(style: Style, value: u64, symbol: &Option<SymbolRef>) -> String {
    let value = style.value(hex32(value));
    match symbol {
        Some(symbol) => format!("{value}  {}", style.symbol(symbol.describe())),
        None => format!("{value}  {}", style.dim("(no symbol)")),
    }
}

/// What the backend read, with the symbols of the code addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub pc: u64,
    pub pc_symbol: Option<SymbolRef>,
    pub hvbar: u64,
    pub hvbar_symbol: Option<SymbolRef>,
    pub hsr: u32,
    pub hdfar: u64,
    pub hifar: u64,
    pub elr: u64,
    pub elr_symbol: Option<SymbolRef>,
    pub spsr: u64,
}

/// A vector table other than the active one that PC lies in.
struct OtherTable {
    base: u64,
    slot: &'static str,
    symbol: String,
}

/// The report as lines of text and as JSON.
pub fn render(facts: &Facts, style: Style) -> (Vec<String>, Value) {
    let Facts {
        pc,
        hvbar,
        hdfar,
        hifar,
        elr,
        spsr,
        ..
    } = *facts;
    let hsr_raw = facts.hsr;
    let hsr = decode::decode_hsr(hsr_raw);
    let spsr_decoded = decode::decode_psr(spsr as u32);
    let slot = decode::vector_slot(hvbar, pc);
    // PC in another table: a fault taken before the final table is installed
    // stops in the first one. Vector tables are 32-byte aligned.
    let other_table = match (&slot, &facts.pc_symbol) {
        (None, Some(symbol)) => symbol.offset.and_then(|offset| {
            let base = pc - offset;
            let (_, slot) = decode::vector_slot(base, pc)?;
            (base != hvbar && base % 32 == 0).then(|| OtherTable {
                base,
                slot,
                symbol: symbol.short_name().to_string(),
            })
        }),
        _ => None,
    };

    let mut lines = Vec::new();
    let table_name = facts
        .hvbar_symbol
        .as_ref()
        .map(|s| format!(" ({})", style.symbol(s.describe())))
        .unwrap_or_default();
    lines.push(format!(
        "{}        {}",
        style.label("PC"),
        with_symbol(style, pc, &facts.pc_symbol)
    ));
    match (&slot, &other_table) {
        (Some((offset, name)), _) => lines.push(format!(
            "          hyp vector {}: HVBAR {}{table_name} + 0x{offset:02X}",
            style.bad(format!("\"{name}\"")),
            hex32(hvbar)
        )),
        (None, Some(other)) => lines.push(format!(
            "          {} entry of {} at {}, which is {} \
             (HVBAR {}{table_name})",
            style.bad(format!("\"{}\"", other.slot)),
            style.symbol(&other.symbol),
            hex32(other.base),
            style.warn("NOT the active table"),
            hex32(hvbar)
        )),
        (None, None) => lines.push(format!(
            "          not at a hyp vector (HVBAR {}{table_name})",
            hex32(hvbar)
        )),
    }
    // HSR = 0 would decode as "EC 0x00 unknown reason"; it means that no
    // exception has been taken to Hyp mode.
    let recorded = hsr_raw != 0;
    if recorded {
        lines.push(format!(
            "{}       {}",
            style.label("HSR"),
            style.value(hex32(hsr_raw.into()))
        ));
        lines.extend(
            hsr.describe(style)
                .into_iter()
                .map(|line| format!("  {line}")),
        );
    } else {
        lines.push(format!(
            "{}       {}  {}",
            style.label("HSR"),
            style.value("0x00000000"),
            style.good("HSR = 0: no exception recorded")
        ));
    }
    let fault_address = if recorded {
        hsr.fault_address()
    } else {
        FaultAddress::None
    };
    match fault_address {
        FaultAddress::Hdfar => lines.push(format!(
            "{}     {}  {}",
            style.label("HDFAR"),
            style.value(hex32(hdfar)),
            style.dim("(faulting data address)")
        )),
        FaultAddress::Hifar => lines.push(format!(
            "{}     {}  {}",
            style.label("HIFAR"),
            style.value(hex32(hifar)),
            style.dim("(faulting instruction address)")
        )),
        FaultAddress::None => {}
    }
    lines.push(format!(
        "{}   {}",
        style.label("ELR_hyp"),
        with_symbol(style, elr, &facts.elr_symbol)
    ));
    lines.push(format!(
        "{}  {}  {}",
        style.label("SPSR_hyp"),
        style.value(hex32(spsr)),
        spsr_decoded.describe()
    ));

    let report = json!({
            "pc": {"value": pc, "hex": hex32(pc), "symbol": symbol_json(&facts.pc_symbol)},
            "hvbar": {"value": hvbar, "hex": hex32(hvbar), "symbol": symbol_json(&facts.hvbar_symbol)},
            "vector": slot.map(|(offset, name)| json!({"offset": offset, "name": name})),
            "other_table": other_table.as_ref().map(|other| json!({
                "base": other.base,
                "hex": hex32(other.base),
                "symbol": other.symbol,
                "slot": other.slot,
            })),
            "exception_recorded": recorded,
            "hsr": if recorded { hsr.to_json() } else { json!({"raw": 0, "hex": "0x00000000"}) },
            "fault_address_register": match fault_address {
                FaultAddress::Hdfar => Some("HDFAR"),
                FaultAddress::Hifar => Some("HIFAR"),
                FaultAddress::None => None,
            },
            "hdfar": {"value": hdfar, "hex": hex32(hdfar)},
            "hifar": {"value": hifar, "hex": hex32(hifar)},
            "elr_hyp": {"value": elr, "hex": hex32(elr), "symbol": symbol_json(&facts.elr_symbol)},
            "spsr_hyp": spsr_decoded.to_json(),
    });
    (lines, report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbol(name: &str, offset: u64) -> Option<SymbolRef> {
        Some(SymbolRef {
            path: name.into(),
            offset: Some(offset),
        })
    }

    #[test]
    fn a_data_abort_at_the_active_table() {
        let facts = Facts {
            pc: 0x2800_1010,
            pc_symbol: symbol("__hyp_vectors", 0x10),
            hvbar: 0x2800_1000,
            hvbar_symbol: symbol("__hyp_vectors", 0),
            // EC 0x24 data abort from a lower EL, DFSC 0x10.
            hsr: 0x9200_0010,
            hdfar: 0x4000_0000,
            hifar: 0,
            elr: 0x2800_2004,
            elr_symbol: symbol("guest_main", 4),
            spsr: 0x6000_01D3,
        };
        let (lines, json) = render(&facts, Style::PLAIN);
        let text = lines.join("\n");
        assert!(
            text.starts_with("PC        0x28001010  __hyp_vectors+0x10\n"),
            "{text}"
        );
        assert!(text.contains("hyp vector \""), "{text}");
        assert!(
            text.contains("HDFAR     0x40000000  (faulting data address)"),
            "{text}"
        );
        assert!(
            text.contains("ELR_hyp   0x28002004  guest_main+0x4"),
            "{text}"
        );
        assert_eq!(json["fault_address_register"], "HDFAR");
        assert_eq!(json["exception_recorded"], true);
        assert_eq!(json["pc"]["symbol"]["name"], "__hyp_vectors");
    }

    #[test]
    fn hsr_zero_means_no_exception() {
        let facts = Facts {
            pc: 0x2800_0000,
            pc_symbol: None,
            hvbar: 0x2800_1000,
            hvbar_symbol: None,
            hsr: 0,
            hdfar: 0,
            hifar: 0,
            elr: 0,
            elr_symbol: None,
            spsr: 0x1DA,
        };
        let (lines, json) = render(&facts, Style::PLAIN);
        let text = lines.join("\n");
        assert!(
            text.contains("HSR       0x00000000  HSR = 0: no exception recorded"),
            "{text}"
        );
        assert!(text.contains("0x28000000  (no symbol)"), "{text}");
        assert!(text.contains("not at a hyp vector"), "{text}");
        assert_eq!(json["exception_recorded"], false);
        assert!(json["fault_address_register"].is_null());
    }
}
