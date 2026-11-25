//! The FDX link of a rustvisor image: one static, `RUSTVISOR_FDX` by
//! default, that holds two FDX channels back to back, `to_target` first and
//! `to_host` right after it (`HEADER + N` bytes in), each with `N` data
//! bytes. This is the layout of rustvisor-sdk's `t32_fdx::Link<N>`; each
//! channel is Lauterbach's (see [`probecore::fdx`]).
//!
//! The target opens `to_host` first and `to_target` last, so once
//! `to_target`'s size is set both channels are ready.

use std::path::Path;

use probecore::Memory;
use probecore::fdx::{self, Channel, HEADER, Packet, Result};

/// The link's symbol in rustvisor's images.
pub const SYMBOL: &str = "RUSTVISOR_FDX";

/// An open link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link {
    pub to_target: Channel,
    pub to_host: Channel,
}

impl Link {
    /// The link at `address` once the target has opened it.
    pub fn locate(memory: &mut impl Memory, address: u64) -> Result<Option<Link>> {
        let Some(size) = fdx::opened(memory, address)? else {
            return Ok(None);
        };
        Ok(Some(Link {
            to_target: Channel { address, size },
            to_host: Channel {
                address: address + (HEADER + size) as u64,
                size,
            },
        }))
    }

    /// Marks the link at `address` closed, so that [`Link::locate`] waits
    /// until the target opens it again. Use while the target is held in
    /// reset: RAM keeps the previous run's open link.
    pub fn close(memory: &mut impl Memory, address: u64) -> Result<()> {
        fdx::close(memory, address)
    }

    /// The longest packet either way.
    pub fn max_payload(&self) -> usize {
        fdx::max_payload(self.to_target.size)
    }

    /// Sends one packet; `Ok(false)` while the target has not read enough.
    pub fn send(&self, memory: &mut impl Memory, payload: &[u8]) -> Result<bool> {
        fdx::send(memory, &self.to_target, payload)
    }

    /// The next packet from the target, if one is there.
    pub fn receive(&self, memory: &mut impl Memory) -> Result<Option<Packet>> {
        fdx::receive(memory, &self.to_host)
    }

    /// Whether a packet sent to the target is still unread.
    pub fn unread(&self, memory: &mut impl Memory) -> Result<bool> {
        fdx::unread(memory, &self.to_target)
    }
}

/// The name of an image's link: `el2` for the ELF, `guest<n>` for the n-th
/// extra image.
pub fn image_name(extra: Option<usize>) -> String {
    match extra {
        None => "el2".to_string(),
        Some(n) => format!("guest{n}"),
    }
}

/// The image a name stands for: `el2` (the ELF, `None`), `guest<n>` or `<n>`
/// (the n-th extra image); `extra` is how many extra images there are.
pub fn parse_image(name: &str, extra: usize) -> std::result::Result<Option<usize>, String> {
    if name == "el2" {
        return Ok(None);
    }
    let number = name.strip_prefix("guest").unwrap_or(name);
    match number.parse::<usize>() {
        Ok(n) if n < extra => Ok(Some(n)),
        Ok(n) => Err(format!(
            "{name}: there is no extra image {n}; project.extra_elf has {extra} image(s)"
        )),
        Err(_) => Err(format!(
            "{name}: an image is el2 (the ELF), guest<n> or <n> (the n-th extra image)"
        )),
    }
}

/// The address of `symbol` in the symbol table of the ELF at `path`.
pub fn symbol_address(path: &Path, symbol: &str) -> std::result::Result<u64, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    probecore::elf::symbols(&bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .into_iter()
        .find(|entry| entry.name == symbol)
        .map(|entry| entry.value)
        .ok_or_else(|| format!("{symbol} is not in the symbol table of {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RAM from address 0x100 holding one link of 32 bytes each way.
    struct Ram(Vec<u8>);

    const BASE: u64 = 0x100;

    impl Memory for Ram {
        fn read(&mut self, address: u64, length: usize) -> probecore::Result<Vec<u8>> {
            let at = (address - BASE) as usize;
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
            let at = (address - BASE) as usize;
            self.0[at..at + 4].copy_from_slice(&value.to_le_bytes());
            Ok(())
        }
    }

    #[test]
    fn the_second_channel_follows_the_first() {
        let mut ram = Ram(vec![0; 2 * (HEADER + 32)]);
        assert_eq!(Link::locate(&mut ram, BASE), Ok(None));
        // Opened as the target does it: to_host, then to_target.
        ram.write_u32(BASE + HEADER as u64 + 32 + 4, 32).unwrap();
        ram.write_u32(BASE + 4, 32).unwrap();
        let link = Link::locate(&mut ram, BASE).unwrap().unwrap();
        assert_eq!(
            link.to_target,
            Channel {
                address: BASE,
                size: 32
            }
        );
        assert_eq!(link.to_host.address, BASE + 64);
        assert_eq!(link.max_payload(), 13);
        assert_eq!(link.receive(&mut ram), Ok(None));
        assert_eq!(link.send(&mut ram, b"plan single"), Ok(true));
        Link::close(&mut ram, BASE).unwrap();
        assert_eq!(Link::locate(&mut ram, BASE), Ok(None));
    }

    #[test]
    fn images_are_named_el2_and_guest_n() {
        assert_eq!(image_name(None), "el2");
        assert_eq!(image_name(Some(3)), "guest3");
        assert_eq!(parse_image("el2", 5), Ok(None));
        assert_eq!(parse_image("guest4", 5), Ok(Some(4)));
        assert_eq!(parse_image("2", 5), Ok(Some(2)));
        assert!(
            parse_image("guest5", 5)
                .unwrap_err()
                .contains("has 5 image(s)")
        );
        assert!(parse_image("g1", 5).is_err());
    }
}
