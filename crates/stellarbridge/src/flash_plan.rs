//! Sector-aware write planning. Geometry comes from OpenOCD, never board constants.
use super::{Error, Result};
use probecore::elf::Segment;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub(crate) struct Bank {
    pub number: usize,
    pub base: u64,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct Sector {
    pub bank: usize,
    pub number: usize,
    pub address: u64,
    pub size: usize,
}

pub(crate) struct Write {
    pub bank: usize,
    pub sector: usize,
    pub address: u64,
    pub data: Vec<u8>,
}

fn number(text: &str) -> Result<u64> {
    let text = text.trim_end_matches([',', ':']);
    let parsed = if let Some(hex) = text.strip_prefix("0x") {
        u64::from_str_radix(hex, 16)
    } else {
        text.parse()
    };
    parsed.map_err(|_| Error(format!("invalid OpenOCD flash geometry: {text:?}")))
}

// OpenOCD src/flash/nor/tcl.c: handle_flash_banks_command / handle_flash_info_command.
pub(crate) fn banks(text: &str) -> Result<Vec<Bank>> {
    let mut banks = Vec::new();
    for line in text.lines().filter(|line| line.starts_with('#')) {
        let words: Vec<_> = line.split_whitespace().collect();
        if words.len() < 9 || words[4] != "at" || words[6] != "size" {
            return Err(Error(format!("invalid OpenOCD flash bank: {line}")));
        }
        let bank = Bank {
            number: number(&words[0][1..])? as usize,
            base: number(words[5])?,
            size: number(words[7])?,
        };
        bank.base
            .checked_add(bank.size)
            .ok_or_else(|| Error("flash bank address overflow".into()))?;
        banks.push(bank);
    }
    if banks.is_empty() {
        return Err(Error("OpenOCD reported no flash banks".into()));
    }
    Ok(banks)
}

pub(crate) fn sectors(bank: &Bank, text: &str) -> Result<Vec<Sector>> {
    let mut sectors = Vec::new();
    let mut offset = 0;
    for line in text
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("# "))
    {
        // '#  0: 0x00000000 (0x4000 16kB) protected'
        let words: Vec<_> = line.split_whitespace().collect();
        if words.len() < 4 {
            return Err(Error(format!("invalid OpenOCD flash sector: {line}")));
        }
        let index = number(words[1])? as usize;
        let at = number(words[2])?;
        let size = number(words[3].trim_start_matches('('))?;
        let end = at
            .checked_add(size)
            .ok_or_else(|| Error("flash sector address overflow".into()))?;
        if index != sectors.len() || at != offset || size == 0 || end > bank.size {
            return Err(Error(format!("inconsistent OpenOCD flash sector: {line}")));
        }
        sectors.push(Sector {
            bank: bank.number,
            number: index,
            address: bank.base + at,
            size: usize::try_from(size).map_err(|_| Error("flash sector too large".into()))?,
        });
        offset = end;
    }
    if offset != bank.size {
        return Err(Error(format!(
            "incomplete sector geometry for flash bank {}",
            bank.number
        )));
    }
    Ok(sectors)
}

pub(crate) fn plan(segments: &[Segment], sectors: &[Sector]) -> Result<Vec<Write>> {
    let mut sorted: Vec<_> = segments.iter().filter(|s| !s.data.is_empty()).collect();
    sorted.sort_by_key(|s| s.paddr);
    let mut previous_end = 0;
    let mut writes: BTreeMap<u64, Write> = BTreeMap::new();
    for segment in sorted {
        let end = segment
            .paddr
            .checked_add(segment.data.len() as u64)
            .ok_or_else(|| Error("ELF segment address overflow".into()))?;
        if segment.paddr < previous_end {
            return Err(Error("overlapping ELF loadable segments".into()));
        }
        previous_end = end;
        let mut address = segment.paddr;
        while address < end {
            let sector = sectors
                .iter()
                .find(|s| s.address <= address && address < s.address + s.size as u64)
                .ok_or_else(|| {
                    Error(format!("no flash sector at ELF load address 0x{address:X}"))
                })?;
            let sector_end = sector.address + sector.size as u64;
            let stop = end.min(sector_end);
            // Preserve write_image erase's padding to the end of the last sector.
            // Merge all segments sharing a sector before any write to that sector.
            let write = writes.entry(sector.address).or_insert_with(|| Write {
                bank: sector.bank,
                sector: sector.number,
                address,
                data: vec![0xFF; (sector_end - address) as usize],
            });
            let source = (address - segment.paddr) as usize;
            let dest = (address - write.address) as usize;
            let count = (stop - address) as usize;
            write.data[dest..dest + count].copy_from_slice(&segment.data[source..source + count]);
            address = stop;
        }
    }
    Ok(writes.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn segment(at: u64, data: &[u8]) -> Segment {
        Segment {
            paddr: at,
            vaddr: at,
            data: data.to_vec(),
        }
    }
    #[test]
    fn geometry_and_shared_sectors_preserve_data_and_padding() {
        let banks =
            banks("#0 : chip.flash (driver) at 0x1000, size 0x00000020, buswidth 0, chipwidth 0\n")
                .unwrap();
        let sectors = sectors(&banks[0], "#0 : driver at 0x1000\n\t# 0: 0x00000000 (0x10 0kB) protected\n\t# 1: 0x00000010 (0x10 0kB) protected\n").unwrap();
        let writes = plan(
            &[
                segment(0x1002, b"AB"),
                segment(0x1008, b"CD"),
                segment(0x100F, b"EF"),
            ],
            &sectors,
        )
        .unwrap();
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0].address, 0x1002);
        assert_eq!(
            writes[0].data,
            [
                b'A', b'B', 255, 255, 255, 255, b'C', b'D', 255, 255, 255, 255, 255, b'E'
            ]
        );
        assert_eq!(writes[1].data[0], b'F');
        assert_eq!(writes[1].data[1..], [255; 15]);
        assert!(plan(&[segment(0x1020, b"bad")], &sectors).is_err());
        assert!(plan(&[segment(0x1000, b"AB"), segment(0x1001, b"CD")], &sectors).is_err());
        assert!(super::sectors(&banks[0], "# 0: 0x0 (0x10 0kB) protected").is_err());
    }
}
