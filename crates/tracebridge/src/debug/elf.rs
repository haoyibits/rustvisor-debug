//! `verify`: the ELF reading and the comparison live in `probecore::elf`;
//! here they run against PowerView.

pub use probecore::elf::*;

use super::probe::{DResult, Probe};
use super::t32target::T32Target;

/// [`probecore::elf::compare`] through the RCL memory API at `AD:`.
pub fn compare(probe: &mut dyn Probe, segments: &[Segment]) -> DResult<Vec<SegmentResult>> {
    Ok(probecore::elf::compare(
        &mut T32Target::new(probe),
        segments,
    )?)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::debug::probe::fake::FakeProbe;

    /// A little-endian ELF32 with no program headers and these sections
    /// (after the null section and `.shstrtab`).
    pub fn elf32_sections(names: &[&str]) -> Vec<u8> {
        let mut strings = b"\0.shstrtab\0".to_vec();
        let mut name_offsets = Vec::new();
        for name in names {
            name_offsets.push(strings.len() as u32);
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
        }
        let strings_offset = 52u32;
        let shoff = strings_offset + strings.len() as u32;
        let count = names.len() as u16 + 2;
        let mut bytes = vec![0u8; 52];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 1;
        bytes[5] = 1;
        bytes[6] = 1;
        bytes[0x20..0x24].copy_from_slice(&shoff.to_le_bytes());
        bytes[0x2E..0x30].copy_from_slice(&40u16.to_le_bytes());
        bytes[0x30..0x32].copy_from_slice(&count.to_le_bytes());
        bytes[0x32..0x34].copy_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&strings);
        let header = |name: u32, offset: u32, size: u32| {
            let mut entry = vec![0u8; 40];
            entry[..4].copy_from_slice(&name.to_le_bytes());
            entry[0x10..0x14].copy_from_slice(&offset.to_le_bytes());
            entry[0x14..0x18].copy_from_slice(&size.to_le_bytes());
            entry
        };
        bytes.extend(header(0, 0, 0));
        bytes.extend(header(1, strings_offset, strings.len() as u32));
        for offset in name_offsets {
            bytes.extend(header(offset, 0, 0));
        }
        bytes
    }

    #[test]
    fn section_names_come_from_shstrtab() {
        let bytes = elf32_sections(&[".text", ".debug_line"]);
        assert_eq!(
            section_names(&bytes).unwrap(),
            ["", ".shstrtab", ".text", ".debug_line"]
        );
        let stripped = elf32(&[(0x100, 0x100, "CODE", 4)], None);
        assert!(section_names(&stripped).unwrap().is_empty());
        assert!(section_names(b"not an elf").is_err());
    }

    /// A little-endian ELF32 with the given (paddr, vaddr, filesz, memsz, data)
    /// program headers; the data follows the headers.
    pub fn elf32(segments: &[(u32, u32, &str, u32)], extra_type: Option<u32>) -> Vec<u8> {
        let count = segments.len() + usize::from(extra_type.is_some());
        let phoff = 52u32;
        let mut data_offset = phoff + 32 * count as u32;
        let mut header = vec![0u8; 52];
        header[..4].copy_from_slice(b"\x7fELF");
        header[4] = 1; // ELFCLASS32
        header[5] = 1; // ELFDATA2LSB
        header[6] = 1;
        header[0x1C..0x20].copy_from_slice(&phoff.to_le_bytes());
        header[0x2A..0x2C].copy_from_slice(&32u16.to_le_bytes());
        header[0x2C..0x2E].copy_from_slice(&(count as u16).to_le_bytes());
        let mut table = Vec::new();
        let mut payload = Vec::new();
        for (paddr, vaddr, data, memsz) in segments {
            for word in [
                PT_LOAD,
                data_offset,
                *vaddr,
                *paddr,
                data.len() as u32,
                *memsz,
                5,
                4,
            ] {
                table.extend_from_slice(&word.to_le_bytes());
            }
            payload.extend_from_slice(data.as_bytes());
            data_offset += data.len() as u32;
        }
        if let Some(kind) = extra_type {
            for word in [kind, 0, 0, 0, 16, 16, 4, 4] {
                table.extend_from_slice(&word.to_le_bytes());
            }
        }
        [header, table, payload].concat()
    }

    #[test]
    fn extracts_pt_load_at_the_physical_address() {
        let elf = elf32(
            &[
                (0x0800_0000, 0x0800_0000, "CODE", 4),
                (0x0800_0400, 0x2000_0000, "DATA", 4),
                // .bss: nothing in the file.
                (0x2000_0004, 0x2000_0004, "", 0x100),
            ],
            Some(4), // PT_NOTE
        );
        let segments = load_segments(&elf).unwrap();
        assert_eq!(
            segments,
            [
                Segment {
                    paddr: 0x0800_0000,
                    vaddr: 0x0800_0000,
                    data: b"CODE".to_vec()
                },
                Segment {
                    paddr: 0x0800_0400,
                    vaddr: 0x2000_0000,
                    data: b"DATA".to_vec()
                },
            ]
        );
    }

    #[test]
    fn rejects_non_elf_and_truncated_files() {
        assert_eq!(load_segments(b"hello").unwrap_err(), "not an ELF file");
        let mut elf = elf32(&[(0, 0, "CODE", 4)], None);
        elf.truncate(60);
        assert_eq!(load_segments(&elf).unwrap_err(), "truncated ELF file");
    }

    #[test]
    fn segment_with_different_vma_is_compared_at_the_lma() {
        let elf = elf32(
            &[(0x100, 0x100, "CODE", 4), (0x488, 0x2000_0000, "DATA", 4)],
            None,
        );
        let segments = load_segments(&elf).unwrap();
        let mut probe = FakeProbe::default();
        probe.write(0x100, b"CODE");
        probe.write(0x488, b"DATA");
        // Live RAM at the VMA has been changed by the program.
        probe.write(0x2000_0000, b"XXXX");
        let results = compare(&mut probe, &segments).unwrap();
        assert!(results.iter().all(|r| r.differing == 0), "{results:?}");
        assert_eq!(probe.log, ["read AD:0x100 4", "read AD:0x488 4"]);
    }

    #[test]
    fn differences_are_counted() {
        let segments = vec![Segment {
            paddr: 0x200,
            vaddr: 0x200,
            data: b"ABCDEFGH".to_vec(),
        }];
        let mut probe = FakeProbe::default();
        probe.write(0x200, b"ABxDEyGz");
        let results = compare(&mut probe, &segments).unwrap();
        assert_eq!(results[0].differing, 3);
        assert_eq!(results[0].first_difference, Some(0x202));
    }
}
