//! `verify`: compare the ELF's loadable content with target memory.
//!
//! Only the program headers matter: every `PT_LOAD` segment with file
//! content is compared at its load address (LMA, `p_paddr`), not at its run
//! address (VMA, `p_vaddr`). A segment that is copied to RAM at startup
//! (`.data`) lives in NVM at its LMA; comparing at the VMA would compare
//! against live RAM that the program has changed.

use crate::target::{Result as TargetResult, Target};

/// `p_type` of a loadable segment.
pub const PT_LOAD: u32 = 1;

/// One loadable segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// Load address (`p_paddr`).
    pub paddr: u64,
    /// Run address (`p_vaddr`), for display only.
    pub vaddr: u64,
    /// The `p_filesz` bytes from the file.
    pub data: Vec<u8>,
}

struct Reader<'a> {
    bytes: &'a [u8],
    little: bool,
}

impl Reader<'_> {
    fn get(&self, offset: usize, size: usize) -> Result<&[u8], String> {
        offset
            .checked_add(size)
            .and_then(|end| self.bytes.get(offset..end))
            .ok_or_else(|| "truncated ELF file".to_string())
    }

    fn u16(&self, offset: usize) -> Result<u64, String> {
        let b: [u8; 2] = self.get(offset, 2)?.try_into().unwrap();
        Ok(if self.little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        } as u64)
    }

    fn u32(&self, offset: usize) -> Result<u64, String> {
        let b: [u8; 4] = self.get(offset, 4)?.try_into().unwrap();
        Ok(if self.little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        } as u64)
    }

    fn u64(&self, offset: usize) -> Result<u64, String> {
        let b: [u8; 8] = self.get(offset, 8)?.try_into().unwrap();
        Ok(if self.little {
            u64::from_le_bytes(b)
        } else {
            u64::from_be_bytes(b)
        })
    }
}

fn index(value: u64) -> Result<usize, String> {
    usize::try_from(value).map_err(|_| "ELF offset out of range".to_string())
}

/// The reader for an ELF file and whether it is ELF64.
fn header(bytes: &[u8]) -> Result<(Reader<'_>, bool), String> {
    if bytes.len() < 16 || &bytes[..4] != b"\x7fELF" {
        return Err("not an ELF file".into());
    }
    let wide = match bytes[4] {
        1 => false,
        2 => true,
        other => return Err(format!("unknown ELF class {other}")),
    };
    let little = match bytes[5] {
        1 => true,
        2 => false,
        other => return Err(format!("unknown ELF data encoding {other}")),
    };
    Ok((Reader { bytes, little }, wide))
}

/// The names of the sections, e.g. `.text`, `.debug_line`. `config` uses it
/// to tell an ELF without a line table, which TRACE32 can show only as
/// assembly.
pub fn section_names(bytes: &[u8]) -> Result<Vec<String>, String> {
    let (r, wide) = header(bytes)?;
    let (shoff, shentsize, shnum, shstrndx) = if wide {
        (r.u64(0x28)?, r.u16(0x3A)?, r.u16(0x3C)?, r.u16(0x3E)?)
    } else {
        (r.u32(0x20)?, r.u16(0x2E)?, r.u16(0x30)?, r.u16(0x32)?)
    };
    if shoff == 0 || shnum == 0 {
        return Ok(Vec::new());
    }
    // (sh_name, sh_offset, sh_size) of one section header.
    let section = |number: u64| -> Result<(u64, u64, u64), String> {
        let base = index(shoff + number * shentsize)?;
        Ok(if wide {
            (r.u32(base)?, r.u64(base + 0x18)?, r.u64(base + 0x20)?)
        } else {
            (r.u32(base)?, r.u32(base + 0x10)?, r.u32(base + 0x14)?)
        })
    };
    let (_, strings_offset, strings_size) = section(shstrndx)?;
    let strings = r.get(index(strings_offset)?, index(strings_size)?)?;
    (0..shnum)
        .map(|number| {
            let (name, _, _) = section(number)?;
            let tail = strings
                .get(index(name)?..)
                .ok_or_else(|| "section name out of range".to_string())?;
            let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
            Ok(String::from_utf8_lossy(&tail[..end]).into_owned())
        })
        .collect()
}

/// One symbol of the ELF symbol table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    /// The address; bit 0 of an Arm Thumb function is cleared.
    pub value: u64,
    /// 0 when the size is unknown.
    pub size: u64,
    /// An Arm function in Thumb code (bit 0 of its ELF value was set).
    pub thumb: bool,
    /// STT_FUNC.
    pub is_function: bool,
}

/// How far past a symbol without a size an address may lie and still be
/// shown as `symbol+offset` (the same rule as tracebridge's symbol lookup).
pub const SYMBOL_SLACK: u64 = 0x100;

/// The functions and objects of one or more ELF files, for backends that
/// have no symbol table of their own (OpenOCD).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SymbolTable {
    pub symbols: Vec<Symbol>,
}

impl SymbolTable {
    /// Read the symbols of `bytes` and add them.
    pub fn add_elf(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.symbols.extend(symbols(bytes)?);
        Ok(())
    }

    /// First and last address of the symbol `name` (the last is the first
    /// when the size is unknown).
    pub fn range(&self, name: &str) -> Option<(u64, u64)> {
        let symbol = self.symbols.iter().find(|symbol| symbol.name == name)?;
        Some((symbol.value, symbol.value + symbol.size.saturating_sub(1)))
    }

    /// Whether the code at `address` is Thumb: from the function symbol
    /// that contains it; `None` when no function with a size does.
    pub fn thumb_at(&self, address: u64) -> Option<bool> {
        self.symbols
            .iter()
            .filter(|s| s.size > 0 && s.value <= address && address - s.value < s.size)
            .filter(|s| s.is_function)
            .min_by_key(|s| s.size)
            .map(|s| s.thumb)
    }

    /// The symbol an address belongs to and the offset into it: the
    /// smallest symbol that contains it, else the nearest symbol below
    /// without a size, within [`SYMBOL_SLACK`] bytes.
    pub fn lookup(&self, address: u64) -> Option<(&Symbol, u64)> {
        let containing = self
            .symbols
            .iter()
            .filter(|s| s.size > 0 && s.value <= address && address - s.value < s.size)
            .min_by_key(|s| s.size);
        if let Some(symbol) = containing {
            return Some((symbol, address - symbol.value));
        }
        self.symbols
            .iter()
            .filter(|s| s.size == 0 && s.value <= address && address - s.value < SYMBOL_SLACK)
            .max_by_key(|s| s.value)
            .map(|symbol| (symbol, address - symbol.value))
    }
}

/// `e_machine` of 32-bit Arm, whose Thumb function addresses have bit 0 set.
const EM_ARM: u64 = 40;

/// The named functions, objects and untyped symbols that are defined in a
/// section (`.symtab`). Arm mapping symbols (`$a`, `$t`, `$d`) are skipped.
pub fn symbols(bytes: &[u8]) -> Result<Vec<Symbol>, String> {
    const SHT_SYMTAB: u64 = 2;
    let (r, wide) = header(bytes)?;
    let machine = r.u16(0x12)?;
    let (shoff, shentsize, shnum) = if wide {
        (r.u64(0x28)?, r.u16(0x3A)?, r.u16(0x3C)?)
    } else {
        (r.u32(0x20)?, r.u16(0x2E)?, r.u16(0x30)?)
    };
    // (sh_type, sh_link, sh_offset, sh_size, sh_entsize)
    let section = |number: u64| -> Result<(u64, u64, u64, u64, u64), String> {
        let base = index(shoff + number * shentsize)?;
        Ok(if wide {
            (
                r.u32(base + 4)?,
                r.u32(base + 0x28)?,
                r.u64(base + 0x18)?,
                r.u64(base + 0x20)?,
                r.u64(base + 0x38)?,
            )
        } else {
            (
                r.u32(base + 4)?,
                r.u32(base + 0x18)?,
                r.u32(base + 0x10)?,
                r.u32(base + 0x14)?,
                r.u32(base + 0x24)?,
            )
        })
    };
    let mut found = Vec::new();
    if shoff == 0 {
        return Ok(found);
    }
    for number in 0..shnum {
        let (kind, link, offset, size, entsize) = section(number)?;
        if kind != SHT_SYMTAB || entsize == 0 {
            continue;
        }
        let (_, _, strings_offset, strings_size, _) = section(link)?;
        let strings = r.get(index(strings_offset)?, index(strings_size)?)?;
        for entry in 0..size / entsize {
            let base = index(offset + entry * entsize)?;
            let (name, info, shndx, value, symbol_size) = if wide {
                (
                    r.u32(base)?,
                    r.get(base + 4, 1)?[0],
                    r.u16(base + 6)?,
                    r.u64(base + 8)?,
                    r.u64(base + 16)?,
                )
            } else {
                (
                    r.u32(base)?,
                    r.get(base + 12, 1)?[0],
                    r.u16(base + 14)?,
                    r.u32(base + 4)?,
                    r.u32(base + 8)?,
                )
            };
            // NOTYPE, OBJECT, FUNC; defined (not SHN_UNDEF, not SHN_ABS).
            let kind = info & 0xF;
            if kind > 2 || shndx == 0 || shndx == 0xFFF1 {
                continue;
            }
            let Some(tail) = strings.get(index(name)?..) else {
                continue;
            };
            let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
            let name = String::from_utf8_lossy(&tail[..end]).into_owned();
            if name.is_empty() || name.starts_with('$') {
                continue;
            }
            let function = kind == 2;
            let thumb = machine == EM_ARM && function && value & 1 == 1;
            let value = if machine == EM_ARM && function {
                value & !1
            } else {
                value
            };
            found.push(Symbol {
                name,
                value,
                size: symbol_size,
                thumb,
                is_function: function,
            });
        }
    }
    Ok(found)
}

/// The `PT_LOAD` segments with `p_filesz > 0` (NOLOAD and `.bss`-only
/// segments have nothing to compare).
pub fn load_segments(bytes: &[u8]) -> Result<Vec<Segment>, String> {
    let (r, wide) = header(bytes)?;
    let (phoff, phentsize, phnum) = if wide {
        (r.u64(0x20)?, r.u16(0x36)?, r.u16(0x38)?)
    } else {
        (r.u32(0x1C)?, r.u16(0x2A)?, r.u16(0x2C)?)
    };
    let mut segments = Vec::new();
    for number in 0..phnum {
        let base = index(phoff + number * phentsize)?;
        let (p_type, offset, vaddr, paddr, filesz) = if wide {
            (
                r.u32(base)?,
                r.u64(base + 8)?,
                r.u64(base + 16)?,
                r.u64(base + 24)?,
                r.u64(base + 32)?,
            )
        } else {
            (
                r.u32(base)?,
                r.u32(base + 4)?,
                r.u32(base + 8)?,
                r.u32(base + 12)?,
                r.u32(base + 16)?,
            )
        };
        if p_type as u32 != PT_LOAD || filesz == 0 {
            continue;
        }
        let data = r.get(index(offset)?, index(filesz)?)?.to_vec();
        segments.push(Segment { paddr, vaddr, data });
    }
    Ok(segments)
}

/// The comparison of one segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentResult {
    pub paddr: u64,
    pub vaddr: u64,
    pub size: usize,
    pub differing: usize,
    pub first_difference: Option<u64>,
}

/// Read every segment at its load address (physical memory, `AD:<p_paddr>`
/// in the data-file notation) and compare it byte by byte.
pub fn compare(target: &mut dyn Target, segments: &[Segment]) -> TargetResult<Vec<SegmentResult>> {
    let mut results = Vec::new();
    for segment in segments {
        let memory = target
            .read_memory(segment.paddr, segment.data.len())
            .map_err(|error| {
                error.context(format!(
                    "cannot read AD:0x{:X} ({} bytes)",
                    segment.paddr,
                    segment.data.len()
                ))
            })?;
        let mut differing = 0;
        let mut first_difference = None;
        for (offset, (expected, actual)) in segment.data.iter().zip(&memory).enumerate() {
            if expected != actual {
                differing += 1;
                first_difference.get_or_insert(segment.paddr + offset as u64);
            }
        }
        results.push(SegmentResult {
            paddr: segment.paddr,
            vaddr: segment.vaddr,
            size: segment.data.len(),
            differing,
            first_difference,
        });
    }
    Ok(results)
}

/// The text and JSON of one image's comparison, as `verify` shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifyReport {
    /// The file name line, one line per segment, then the verdict.
    pub lines: Vec<String>,
    pub matched: bool,
    pub differing: usize,
    pub first_difference: Option<u64>,
    pub segments: serde_json::Value,
}

pub fn verify_report(
    path: &std::path::Path,
    results: &[SegmentResult],
    style: crate::style::Style,
) -> VerifyReport {
    let mut lines = vec![path.display().to_string()];
    for result in results {
        let runs = if result.vaddr != result.paddr {
            style.dim(format!("  (runs at 0x{:08X})", result.vaddr))
        } else {
            String::new()
        };
        let verdict = match result.first_difference {
            None => style.good("match"),
            Some(first) => style.bad(format!(
                "{} bytes differ, first at AD:0x{first:08X}",
                result.differing
            )),
        };
        lines.push(format!(
            "  {}  {:>8} bytes{runs}  {verdict}",
            style.address(format!("AD:0x{:08X}", result.paddr)),
            result.size
        ));
    }
    let differing: usize = results.iter().map(|r| r.differing).sum();
    let first = results.iter().find_map(|r| r.first_difference);
    lines.push(match first {
        None => style.good("match"),
        Some(first) => format!(
            "{} {differing} bytes differ, first at AD:0x{first:08X}",
            style.bad("MISMATCH:")
        ),
    });
    let segments = results
        .iter()
        .map(|r| {
            serde_json::json!({
                "paddr": r.paddr,
                "vaddr": r.vaddr,
                "size": r.size,
                "differing": r.differing,
                "first_difference": r.first_difference,
            })
        })
        .collect();
    VerifyReport {
        lines,
        matched: differing == 0,
        differing,
        first_difference: first,
        segments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::{Error, TargetAddress};

    /// Physical memory only; everything else is unreachable here.
    struct Ram(Vec<(u64, Vec<u8>)>);

    impl Target for Ram {
        fn read_u32(&mut self, _: &TargetAddress) -> TargetResult<u32> {
            unreachable!()
        }
        fn read_core_register(&mut self, _: &str) -> TargetResult<u64> {
            unreachable!()
        }
        fn read_memory(&mut self, address: u64, length: usize) -> TargetResult<Vec<u8>> {
            self.0
                .iter()
                .find(|(base, _)| *base == address)
                .map(|(_, bytes)| bytes[..length].to_vec())
                .ok_or_else(|| Error::new("bus error"))
        }
        fn is_running(&mut self) -> TargetResult<bool> {
            unreachable!()
        }
        fn halt(&mut self) -> TargetResult<()> {
            unreachable!()
        }
        fn symbol_range(&mut self, _: &str) -> TargetResult<(u64, u64)> {
            unreachable!()
        }
        fn break_command(&self) -> &str {
            "test break"
        }
    }

    /// A minimal ELF32 Arm file with a `.symtab` (the symbols given) and
    /// its string table.
    fn elf32_with_symbols(entries: &[(&str, u32, u32, u8, u16)]) -> Vec<u8> {
        let mut strtab = vec![0u8];
        let mut symtab = vec![0u8; 16]; // the null symbol
        for (name, value, size, info, shndx) in entries {
            let name_offset = strtab.len() as u32;
            strtab.extend_from_slice(name.as_bytes());
            strtab.push(0);
            symtab.extend_from_slice(&name_offset.to_le_bytes());
            symtab.extend_from_slice(&value.to_le_bytes());
            symtab.extend_from_slice(&size.to_le_bytes());
            symtab.push(*info);
            symtab.push(0);
            symtab.extend_from_slice(&shndx.to_le_bytes());
        }
        let mut file = vec![0u8; 52];
        file[..4].copy_from_slice(b"\x7fELF");
        file[4] = 1; // ELF32
        file[5] = 1; // little endian
        file[0x12..0x14].copy_from_slice(&40u16.to_le_bytes()); // EM_ARM
        let symtab_offset = file.len() as u32;
        file.extend_from_slice(&symtab);
        let strtab_offset = file.len() as u32;
        file.extend_from_slice(&strtab);
        let shoff = file.len() as u32;
        file[0x20..0x24].copy_from_slice(&shoff.to_le_bytes());
        file[0x2E..0x30].copy_from_slice(&40u16.to_le_bytes());
        file[0x30..0x32].copy_from_slice(&3u16.to_le_bytes());
        // Section headers: null, .symtab (link 2), .strtab.
        let header = |kind: u32, link: u32, offset: u32, size: u32, entsize: u32| {
            let mut h = vec![0u8; 40];
            h[4..8].copy_from_slice(&kind.to_le_bytes());
            h[0x10..0x14].copy_from_slice(&offset.to_le_bytes());
            h[0x14..0x18].copy_from_slice(&size.to_le_bytes());
            h[0x18..0x1C].copy_from_slice(&link.to_le_bytes());
            h[0x24..0x28].copy_from_slice(&entsize.to_le_bytes());
            h
        };
        file.extend(header(0, 0, 0, 0, 0));
        file.extend(header(2, 2, symtab_offset, symtab.len() as u32, 16));
        file.extend(header(3, 0, strtab_offset, strtab.len() as u32, 0));
        file
    }

    #[test]
    fn symbols_come_from_the_symbol_table() {
        const FUNC: u8 = 0x12; // GLOBAL FUNC
        const OBJECT: u8 = 0x11;
        const NOTYPE: u8 = 0x10;
        let bytes = elf32_with_symbols(&[
            ("main", 0x2800_0101, 0x40, FUNC, 1),
            ("arm_code", 0x2800_0200, 0x10, FUNC, 1),
            ("_SEGGER_RTT", 0x6000_0000, 0xA8, OBJECT, 2),
            ("__hyp_vectors", 0x2800_1000, 0, NOTYPE, 1),
            ("$t", 0x2800_0100, 0, 0x00, 1),
            ("undefined", 0, 0, FUNC, 0),
            ("absolute", 0x10, 0, NOTYPE, 0xFFF1),
        ]);
        let mut table = SymbolTable::default();
        table.add_elf(&bytes).unwrap();
        let names: Vec<&str> = table.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["main", "arm_code", "_SEGGER_RTT", "__hyp_vectors"]);
        // The instruction set of the function that contains an address.
        assert_eq!(table.thumb_at(0x2800_0102), Some(true));
        assert_eq!(table.thumb_at(0x2800_0204), Some(false));
        assert_eq!(table.thumb_at(0x2800_1004), None); // not a function
        assert_eq!(table.thumb_at(0x6000_0000), None); // an object
        // Thumb bit cleared.
        assert_eq!(table.range("main"), Some((0x2800_0100, 0x2800_013F)));
        assert_eq!(
            table.range("__hyp_vectors"),
            Some((0x2800_1000, 0x2800_1000))
        );
        assert_eq!(table.range("missing"), None);
        let (symbol, offset) = table.lookup(0x2800_0110).unwrap();
        assert_eq!((symbol.name.as_str(), offset), ("main", 0x10));
        let (symbol, offset) = table.lookup(0x2800_1018).unwrap();
        assert_eq!((symbol.name.as_str(), offset), ("__hyp_vectors", 0x18));
        // Past the slack of a symbol without size, and past main's end.
        assert!(table.lookup(0x2800_1100).is_none());
        assert!(table.lookup(0x2800_0140).is_none());
    }

    #[test]
    fn compare_reads_each_segment_at_its_load_address() {
        let segment = |paddr: u64, data: &[u8]| Segment {
            paddr,
            vaddr: paddr + 0x1000,
            data: data.to_vec(),
        };
        let mut ram = Ram(vec![(0x100, b"CODE".to_vec()), (0x200, b"DxTA".to_vec())]);
        let results = compare(
            &mut ram,
            &[segment(0x100, b"CODE"), segment(0x200, b"DATA")],
        )
        .unwrap();
        assert_eq!(results[0].differing, 0);
        assert_eq!(
            (results[1].differing, results[1].first_difference),
            (1, Some(0x201))
        );
        let error = compare(&mut ram, &[segment(0x300, b"X")]).unwrap_err();
        assert_eq!(error.message, "cannot read AD:0x300 (1 bytes): bus error");
    }
}
