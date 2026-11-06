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
