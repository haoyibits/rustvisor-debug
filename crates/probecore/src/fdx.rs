//! TRACE32 FDX (Fast Data eXchange) channels, BufferE method: the
//! debugger's end, over any [`Memory`] that works while the core runs.
//!
//! A channel is a ring buffer in target RAM, laid out as Lauterbach's
//! target agent defines it (`T32_FDX_BUFFER` in `t32fdx.h`, from TRACE32
//! `demo/arm/fdx`): eight 32-bit header words, then `size` data bytes. Of
//! the header only four words are used here:
//!
//! | Word | Field | Written by |
//! |---|---|---|
//! | 1 | `size`: data bytes, 0 while the channel is closed | target |
//! | 2 | `hostindex`: the debugger's position in the ring | debugger |
//! | 3 | `targetindex`: the target's position in the ring | target |
//! | 4 | `stall`: 1 once a packet has been dropped | target |
//!
//! One side only ever moves its own index, so the ring needs no lock. A
//! packet is a length, the payload, and a 0 byte after it that says
//! "nothing yet" until the next packet's length overwrites it. The length
//! is one byte for 1 to 251 payload bytes, or `0xFE` with two more bytes
//! (little-endian) for up to 65535. `0xFF` means "wrapped, continue at the
//! other end", `0xFD` "a packet was dropped here" (target to debugger).
//!
//! - A channel from the target fills upwards from `targetindex`: length
//!   byte(s), payload.
//! - A channel to the target fills downwards from `hostindex`: the length
//!   byte at the top (`0xFE` with the two length bytes just below it), the
//!   payload below that.
//!
//! The agent has no code for this end. [`receive`] reads a channel the way
//! the agent's DCC path does (`T32_Fdx_SendDCC`), which plays the debugger
//! on the target; [`send`] fills one the way `T32_Fdx_ReceiveDCC` does, with
//! the same room checks. The `fdx-oracle` crate runs both against the
//! agent's C source.
//!
//! Header words are read one 32-bit access each ([`Memory::read_u32s`]), so
//! an index the target is updating is never seen half old and half new.
//! Every write is one aligned word. The target never writes a data byte of
//! a channel to the target, so rewriting whole words of it with the bytes
//! just read cannot undo a target write. The byte that makes a packet
//! visible is in the last data word written; `hostindex` follows it.
//!
//! Ported from rustvisor-sdk's `tests/fdx` (`host.rs`), where the board
//! runner used it first.

use std::fmt;

use crate::target::Memory;

/// Bytes of a channel header: eight 32-bit words.
pub const HEADER: usize = 32;
/// Header word `size`.
const SIZE: usize = 1;
/// Header word `hostindex`.
const HOSTINDEX: usize = 2;
/// Header word `targetindex`.
const TARGETINDEX: usize = 3;

/// The longest payload a one-byte length carries; longer ones use `LONG`.
const SHORT_MAX: usize = 251;
/// Length marker: two length bytes follow (from the target) or precede (to
/// the target).
const LONG: u8 = 0xFE;
/// Length marker: wrapped, continue at the other end of the ring.
const WRAP: u8 = 0xFF;
/// Length marker from the target: a packet was dropped because the ring was
/// full.
const DROPPED: u8 = 0xFD;

/// The longest payload a channel of `n` data bytes always has room for once
/// the other side has read everything: its length bytes, the payload and the
/// 0 after it in half the ring. 13 bytes for `n` = 32, 61 for 128.
pub const fn max_payload(n: usize) -> usize {
    let short = n / 2 - 3;
    if short <= SHORT_MAX { short } else { n / 2 - 5 }
}

/// One open channel: where it is and its data bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Channel {
    pub address: u64,
    pub size: usize,
}

/// One packet from the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    Data(Vec<u8>),
    /// The target dropped at least one packet here: the ring was full.
    Dropped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The channel's size changed: the target closed or reopened it.
    Closed,
    /// Empty, or longer than [`max_payload`].
    Size,
    /// An index or length in the ring is outside it.
    Corrupt,
    Memory(crate::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Closed => f.write_str("the target closed or reopened the FDX channel"),
            Error::Size => f.write_str("FDX packet is empty or too long for the channel"),
            Error::Corrupt => f.write_str("FDX channel header or ring is corrupt"),
            Error::Memory(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for Error {}

impl From<crate::Error> for Error {
    fn from(error: crate::Error) -> Self {
        Error::Memory(error)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// The data bytes of the channel at `address`, `None` while it is closed.
pub fn opened(memory: &mut impl Memory, address: u64) -> Result<Option<usize>> {
    let size = memory.read_u32s(address + 4 * SIZE as u64, 1)?[0] as usize;
    if size == 0 {
        return Ok(None);
    }
    if size % 4 != 0 || !(8..=0xFFFF).contains(&size) {
        return Err(Error::Corrupt);
    }
    Ok(Some(size))
}

/// Marks the channel at `address` closed, so that [`opened`] says `None`
/// until the target opens it again. Use while the target is held in reset:
/// RAM keeps the previous run's open channel.
pub fn close(memory: &mut impl Memory, address: u64) -> Result<()> {
    Ok(memory.write_u32(address + 4 * SIZE as u64, 0)?)
}

/// One channel as read: its header words and data bytes.
struct Image {
    address: u64,
    header: Vec<u32>,
    data: Vec<u8>,
}

impl Image {
    fn read(memory: &mut impl Memory, channel: &Channel) -> Result<Self> {
        if channel.address % 4 != 0 {
            return Err(Error::Corrupt);
        }
        let header = memory.read_u32s(channel.address, HEADER / 4)?;
        if header[SIZE] as usize != channel.size {
            return Err(Error::Closed);
        }
        let data = memory.read(channel.address + HEADER as u64, channel.size)?;
        if data.len() != channel.size {
            return Err(Error::Memory(crate::Error::new(format!(
                "read {} of {} FDX data bytes",
                data.len(),
                channel.size
            ))));
        }
        Ok(Self {
            address: channel.address,
            header,
            data,
        })
    }

    /// Writes `hostindex`.
    fn release(&self, memory: &mut impl Memory, index: usize) -> Result<()> {
        Ok(memory.write_u32(self.address + 4 * HOSTINDEX as u64, index as u32)?)
    }
}

/// Sends one packet into the channel to the target. `Ok(false)` while the
/// ring has no room for it; nothing is written then.
pub fn send(memory: &mut impl Memory, channel: &Channel, payload: &[u8]) -> Result<bool> {
    let size = channel.size;
    let len = payload.len();
    let head = if len > SHORT_MAX { 3 } else { 1 };
    if len == 0 || len > max_payload(size) {
        return Err(Error::Size);
    }
    let image = Image::read(memory, channel)?;
    let mut host = image.header[HOSTINDEX] as usize;
    let target = image.header[TARGETINDEX] as usize;
    if host >= size || target >= size {
        return Err(Error::Corrupt);
    }
    // Wrap when the packet and the 0 below it do not fit below `host`,
    // unless the target is still reading the top of the ring.
    let mut wrapped = None;
    if host < head + len + 1 {
        if target < host || target >= size - 1 {
            return Ok(false);
        }
        wrapped = Some(host);
        host = size - 1;
    }
    // Below the target's mark only once wrapped: keep above it, one spare.
    let next = host - head - len;
    if target < host && next - 1 <= target {
        return Ok(false);
    }
    let mut data = image.data.clone();
    data[next] = 0;
    data[next + 1..next + 1 + len].copy_from_slice(payload);
    let mark = if head == 3 {
        data[host - 2] = len as u8;
        data[host - 1] = (len >> 8) as u8;
        LONG
    } else {
        len as u8
    };
    // The byte that makes the packet visible: its own mark, or once wrapped
    // the wrap mark, which leads the target to it.
    let (visible, value) = match wrapped {
        Some(old) => {
            data[host] = mark;
            (old, WRAP)
        }
        None => (host, mark),
    };
    let last = visible / 4;
    for word in (0..size / 4).filter(|&w| w != last) {
        let span = 4 * word..4 * word + 4;
        if data[span.clone()] != image.data[span.clone()] {
            write_data(memory, channel.address, word, &data[span])?;
        }
    }
    data[visible] = value;
    write_data(memory, channel.address, last, &data[4 * last..4 * last + 4])?;
    image.release(memory, next)?;
    Ok(true)
}

fn write_data(memory: &mut impl Memory, channel: u64, word: usize, bytes: &[u8]) -> Result<()> {
    Ok(memory.write_u32(
        channel + (HEADER + 4 * word) as u64,
        u32::from_le_bytes(bytes.try_into().expect("4 bytes")),
    )?)
}

/// Takes the next packet from the channel from the target, if one is there.
pub fn receive(memory: &mut impl Memory, channel: &Channel) -> Result<Option<Packet>> {
    let size = channel.size;
    let image = Image::read(memory, channel)?;
    let data = &image.data;
    let mut host = image.header[HOSTINDEX] as usize;
    if host >= size {
        return Err(Error::Corrupt);
    }
    let mut mark = data[host];
    if mark == WRAP {
        host = 0;
        mark = data[0];
        if mark == 0 {
            // The target wraps before it writes the packet at 0.
            image.release(memory, 0)?;
            return Ok(None);
        }
    }
    if mark == 0 {
        return Ok(None);
    }
    let (len, head) = match mark {
        LONG if host + 2 < size => (data[host + 1] as usize | (data[host + 2] as usize) << 8, 3),
        LONG => return Err(Error::Corrupt),
        DROPPED => (0, 1),
        short => (short as usize, 1),
    };
    let end = host + head + len;
    if end >= size {
        return Err(Error::Corrupt);
    }
    let packet = if mark == DROPPED {
        Packet::Dropped
    } else {
        Packet::Data(data[host + head..end].to_vec())
    };
    image.release(memory, end)?;
    Ok(Some(packet))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Target RAM from address 0, with every access logged.
    struct Ram {
        bytes: Vec<u8>,
        log: Vec<String>,
    }

    impl Ram {
        fn channel(size: usize) -> (Ram, Channel) {
            let mut ram = Ram {
                bytes: vec![0; HEADER + size],
                log: Vec::new(),
            };
            ram.word(SIZE, size as u32);
            (ram, Channel { address: 0, size })
        }

        fn word(&mut self, index: usize, value: u32) {
            self.bytes[4 * index..4 * index + 4].copy_from_slice(&value.to_le_bytes());
        }

        fn header(&self, index: usize) -> u32 {
            u32::from_le_bytes(self.bytes[4 * index..4 * index + 4].try_into().unwrap())
        }

        fn data(&mut self) -> &mut [u8] {
            &mut self.bytes[HEADER..]
        }
    }

    impl Memory for Ram {
        fn read(&mut self, address: u64, length: usize) -> crate::Result<Vec<u8>> {
            let at = address as usize;
            Ok(self.bytes[at..at + length].to_vec())
        }
        fn read_u32s(&mut self, address: u64, count: usize) -> crate::Result<Vec<u32>> {
            self.log.push(format!("read_u32s {address:#x} {count}"));
            Ok((0..count)
                .map(|i| self.header(address as usize / 4 + i))
                .collect())
        }
        fn write(&mut self, _: u64, _: &[u8]) -> crate::Result<()> {
            panic!("FDX writes only whole words");
        }
        fn write_u32(&mut self, address: u64, value: u32) -> crate::Result<()> {
            assert_eq!(address % 4, 0, "unaligned word write");
            self.log.push(format!("write_u32 {address:#x}"));
            self.word(address as usize / 4, value);
            Ok(())
        }
    }

    #[test]
    fn a_closed_channel_is_none_and_a_bad_size_is_corrupt() {
        let (mut ram, _) = Ram::channel(32);
        assert_eq!(opened(&mut ram, 0), Ok(Some(32)));
        close(&mut ram, 0).unwrap();
        assert_eq!(opened(&mut ram, 0), Ok(None));
        ram.word(SIZE, 30);
        assert_eq!(opened(&mut ram, 0), Err(Error::Corrupt));
        assert_eq!(ram.log[0], "read_u32s 0x4 1");
    }

    #[test]
    fn short_and_long_packets_from_the_target() {
        let (mut ram, channel) = Ram::channel(600);
        let data = ram.data();
        data[0] = 2;
        data[1..3].copy_from_slice(b"ok");
        data[3] = LONG;
        data[4] = 0x2C;
        data[5] = 0x01;
        for (i, byte) in data[6..6 + 300].iter_mut().enumerate() {
            *byte = i as u8;
        }
        assert_eq!(
            receive(&mut ram, &channel),
            Ok(Some(Packet::Data(b"ok".to_vec())))
        );
        assert_eq!(ram.header(HOSTINDEX), 3);
        let Ok(Some(Packet::Data(long))) = receive(&mut ram, &channel) else {
            panic!("no long packet");
        };
        assert_eq!(long.len(), 300);
        assert_eq!(receive(&mut ram, &channel), Ok(None));
    }

    #[test]
    fn a_wrap_mark_continues_at_the_start() {
        let (mut ram, channel) = Ram::channel(32);
        ram.word(HOSTINDEX, 20);
        ram.data()[20] = WRAP;
        assert_eq!(receive(&mut ram, &channel), Ok(None));
        assert_eq!(ram.header(HOSTINDEX), 0);
        ram.data()[0] = 1;
        ram.data()[1] = b'x';
        assert_eq!(
            receive(&mut ram, &channel),
            Ok(Some(Packet::Data(b"x".to_vec())))
        );
    }

    #[test]
    fn a_sent_packet_hangs_below_the_host_index() {
        let (mut ram, channel) = Ram::channel(32);
        ram.word(HOSTINDEX, 31);
        ram.word(TARGETINDEX, 31);
        assert_eq!(send(&mut ram, &channel, b"hi"), Ok(true));
        // Mark at 31, payload at 29..31, the 0 below it at 28.
        assert_eq!(&ram.data()[28..32], &[0, b'h', b'i', 2]);
        assert_eq!(ram.header(HOSTINDEX), 28);
        // The word with the mark is written last, then hostindex.
        let writes: Vec<_> = ram
            .log
            .iter()
            .filter(|line| line.starts_with("write"))
            .cloned()
            .collect();
        assert_eq!(writes, ["write_u32 0x3c", "write_u32 0x8"]);
    }

    #[test]
    fn no_room_writes_nothing_and_bad_sizes_are_refused() {
        let (mut ram, channel) = Ram::channel(32);
        assert_eq!(send(&mut ram, &channel, b""), Err(Error::Size));
        assert_eq!(
            send(&mut ram, &channel, &[0; 14]),
            Err(Error::Size),
            "max_payload(32) is 13"
        );
        // The host is at the bottom, the target still reads the top.
        ram.word(HOSTINDEX, 2);
        ram.word(TARGETINDEX, 0);
        ram.log.clear();
        assert_eq!(send(&mut ram, &channel, b"abc"), Ok(false));
        assert!(ram.log.iter().all(|line| !line.starts_with("write")));
    }

    #[test]
    fn a_reopened_channel_is_closed_and_bad_indices_are_corrupt() {
        let (mut ram, channel) = Ram::channel(32);
        ram.word(SIZE, 64);
        assert_eq!(receive(&mut ram, &channel), Err(Error::Closed));
        ram.word(SIZE, 32);
        ram.word(HOSTINDEX, 32);
        assert_eq!(receive(&mut ram, &channel), Err(Error::Corrupt));
        ram.word(HOSTINDEX, 0);
        ram.data()[0] = 40;
        assert_eq!(receive(&mut ram, &channel), Err(Error::Corrupt));
    }

    #[test]
    fn max_payload_fits_half_the_ring() {
        assert_eq!(max_payload(32), 13);
        assert_eq!(max_payload(128), 61);
        assert_eq!(max_payload(0x1000), 0x800 - 5);
    }
}
