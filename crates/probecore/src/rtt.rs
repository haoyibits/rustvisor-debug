//! SEGGER RTT channel 0 over target memory: the control block, the ring
//! buffer descriptors and the two-copy reads and writes around the wrap.
//! Only the 32-bit control block layout is supported. The probe access is the
//! backend's [`Memory`] (TRACE32 uses run-time access, `E:`).

use crate::target::{Error, Memory, Result};

pub const ID_STRING: &[u8] = b"SEGGER RTT";

// 32-bit SEGGER RTT control-block offsets.
pub const UP_DESCRIPTOR: u64 = 0x18;
/// UP_DESCRIPTOR + one 24-byte up descriptor.
pub const DOWN_DESCRIPTOR: u64 = 0x30;
pub const DESC_BUFFER: u64 = 0x04;
pub const DESC_SIZE: u64 = 0x08;
pub const DESC_WR_OFF: u64 = 0x0C;
pub const DESC_RD_OFF: u64 = 0x10;

/// One ring buffer descriptor: `pBuffer`, `SizeOfBuffer`, `WrOff`, `RdOff`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Descriptor {
    buffer: u32,
    size: u32,
    write_offset: u32,
    read_offset: u32,
}

impl Descriptor {
    fn read(memory: &mut impl Memory, address: u64) -> Result<Descriptor> {
        let raw = memory.read(address, 16)?;
        if raw.len() < 16 {
            return Err(Error::new("short RTT descriptor read"));
        }
        let word = |i: usize| u32::from_le_bytes(raw[i * 4..i * 4 + 4].try_into().unwrap());
        Ok(Descriptor {
            buffer: word(0),
            size: word(1),
            write_offset: word(2),
            read_offset: word(3),
        })
    }

    fn is_valid(&self) -> bool {
        self.buffer != 0
            && self.size >= 2
            && self.write_offset < self.size
            && self.read_offset < self.size
    }
}

/// Bidirectional RTT channel 0 (`RttChannel`).
#[derive(Debug)]
pub struct RttChannel {
    pub control_block: u64,
    pub initialized: bool,
}

impl RttChannel {
    pub fn new(control_block: u64) -> Self {
        RttChannel {
            control_block,
            initialized: false,
        }
    }

    /// The address of a control block field.
    pub fn cb(&self, offset: u64) -> u64 {
        self.control_block + offset
    }

    /// Check the "SEGGER RTT" identifier.
    pub fn refresh_state(&mut self, memory: &mut impl Memory) -> Result<bool> {
        let identifier = memory.read(self.cb(0), 16)?;
        self.initialized = identifier.starts_with(ID_STRING);
        Ok(self.initialized)
    }

    /// Drain the target-to-host bytes that are currently available.
    pub fn read_up(&mut self, memory: &mut impl Memory) -> Result<Vec<u8>> {
        if !self.initialized && !self.refresh_state(memory)? {
            return Ok(Vec::new());
        }
        let descriptor = Descriptor::read(memory, self.cb(UP_DESCRIPTOR + DESC_BUFFER))?;
        if !descriptor.is_valid() {
            self.initialized = false;
            return Ok(Vec::new());
        }
        let Descriptor {
            buffer,
            size,
            write_offset,
            mut read_offset,
        } = descriptor;
        if write_offset == read_offset {
            return Ok(Vec::new());
        }
        let mut data = Vec::new();
        if write_offset < read_offset {
            data.extend(memory.read(
                u64::from(buffer) + u64::from(read_offset),
                (size - read_offset) as usize,
            )?);
            read_offset = 0;
        }
        if write_offset > read_offset {
            data.extend(memory.read(
                u64::from(buffer) + u64::from(read_offset),
                (write_offset - read_offset) as usize,
            )?);
        }
        // The host is the sole writer of the up-channel read offset.
        memory.write_u32(self.cb(UP_DESCRIPTOR + DESC_RD_OFF), write_offset)?;
        Ok(data)
    }

    /// Write as much of `data` as fits into host-to-target channel 0; returns
    /// the number of bytes written.
    pub fn write_down(&mut self, memory: &mut impl Memory, data: &[u8]) -> Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        if !self.initialized && !self.refresh_state(memory)? {
            return Ok(0);
        }
        let descriptor = Descriptor::read(memory, self.cb(DOWN_DESCRIPTOR + DESC_BUFFER))?;
        if !descriptor.is_valid() {
            self.initialized = false;
            return Ok(0);
        }
        let Descriptor {
            buffer,
            size,
            write_offset,
            read_offset,
        } = descriptor;
        let (size, write_offset, read_offset) =
            (size as usize, write_offset as usize, read_offset as usize);
        let free = (read_offset + size - write_offset - 1) % size;
        let count = data.len().min(free);
        if count == 0 {
            return Ok(0);
        }
        let first = count.min(size - write_offset);
        memory.write(u64::from(buffer) + write_offset as u64, &data[..first])?;
        if count > first {
            memory.write(u64::from(buffer), &data[first..count])?;
        }
        // Publish WrOff last so the target never sees incomplete input.
        memory.write_u32(
            self.cb(DOWN_DESCRIPTOR + DESC_WR_OFF),
            ((write_offset + count) % size) as u32,
        )?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const CB: u64 = 0x2000_0000;
    const UP_BUFFER: u32 = 0x2000_1000;
    const DOWN_BUFFER: u32 = 0x2000_2000;

    /// Sparse little-endian memory that logs every access.
    #[derive(Default)]
    struct FakeMemory {
        bytes: HashMap<u64, u8>,
        log: Vec<String>,
        fail: bool,
    }

    impl FakeMemory {
        fn put(&mut self, address: u64, data: &[u8]) {
            for (i, byte) in data.iter().enumerate() {
                self.bytes.insert(address + i as u64, *byte);
            }
        }

        fn get(&self, address: u64, length: usize) -> Vec<u8> {
            (0..length)
                .map(|i| *self.bytes.get(&(address + i as u64)).unwrap_or(&0))
                .collect()
        }

        fn word(&self, address: u64) -> u32 {
            u32::from_le_bytes(self.get(address, 4).try_into().unwrap())
        }

        fn descriptor(&mut self, at: u64, buffer: u32, size: u32, write: u32, read: u32) {
            for (i, value) in [buffer, size, write, read].into_iter().enumerate() {
                self.put(at + DESC_BUFFER + 4 * i as u64, &value.to_le_bytes());
            }
        }

        /// An initialized control block with a 16-byte up and an 8-byte down buffer.
        fn initialized() -> FakeMemory {
            let mut memory = FakeMemory::default();
            memory.put(CB, b"SEGGER RTT\0\0\0\0\0\0");
            memory.descriptor(CB + UP_DESCRIPTOR, UP_BUFFER, 16, 0, 0);
            memory.descriptor(CB + DOWN_DESCRIPTOR, DOWN_BUFFER, 8, 0, 0);
            memory
        }
    }

    impl Memory for FakeMemory {
        fn read(&mut self, address: u64, length: usize) -> Result<Vec<u8>> {
            if self.fail {
                return Err(Error::new("timed out"));
            }
            self.log.push(format!("read {address:#x} {length}"));
            Ok(self.get(address, length))
        }
        fn read_u32s(&mut self, address: u64, count: usize) -> Result<Vec<u32>> {
            let bytes = self.read(address, 4 * count)?;
            Ok(bytes
                .chunks_exact(4)
                .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
                .collect())
        }
        fn write(&mut self, address: u64, data: &[u8]) -> Result<()> {
            self.log.push(format!("write {address:#x} {data:?}"));
            self.put(address, data);
            Ok(())
        }
        fn write_u32(&mut self, address: u64, value: u32) -> Result<()> {
            self.log.push(format!("write_u32 {address:#x} {value}"));
            self.put(address, &value.to_le_bytes());
            Ok(())
        }
    }

    const UP_RD: u64 = CB + UP_DESCRIPTOR + DESC_RD_OFF;
    const UP_WR: u64 = CB + UP_DESCRIPTOR + DESC_WR_OFF;
    const DOWN_WR: u64 = CB + DOWN_DESCRIPTOR + DESC_WR_OFF;
    const DOWN_RD: u64 = CB + DOWN_DESCRIPTOR + DESC_RD_OFF;

    #[test]
    fn offsets_match_the_32_bit_layout() {
        assert_eq!(DOWN_DESCRIPTOR, UP_DESCRIPTOR + 24);
        assert_eq!(DESC_SIZE, DESC_BUFFER + 4);
    }

    #[test]
    fn uninitialized_control_block_waits() {
        let mut memory = FakeMemory::default();
        let mut channel = RttChannel::new(CB);
        assert!(channel.read_up(&mut memory).unwrap().is_empty());
        assert!(!channel.initialized);
        assert_eq!(channel.write_down(&mut memory, b"x").unwrap(), 0);
        assert!(memory.log.iter().all(|entry| entry.starts_with("read")));
        // Initialization is picked up on the next poll.
        memory = FakeMemory::initialized();
        memory.put(u64::from(UP_BUFFER), b"hi");
        memory.put(UP_WR, &2u32.to_le_bytes());
        assert_eq!(channel.read_up(&mut memory).unwrap(), b"hi");
        assert!(channel.initialized);
    }

    #[test]
    fn empty_up_buffer_writes_nothing() {
        let mut memory = FakeMemory::initialized();
        let mut channel = RttChannel::new(CB);
        assert!(channel.read_up(&mut memory).unwrap().is_empty());
        assert!(!memory.log.iter().any(|entry| entry.starts_with("write")));
    }

    #[test]
    fn contiguous_up_read_advances_rd_off() {
        let mut memory = FakeMemory::initialized();
        memory.put(u64::from(UP_BUFFER) + 3, b"hello");
        memory.put(UP_RD, &3u32.to_le_bytes());
        memory.put(UP_WR, &8u32.to_le_bytes());
        let mut channel = RttChannel::new(CB);
        assert_eq!(channel.read_up(&mut memory).unwrap(), b"hello");
        assert_eq!(memory.word(UP_RD), 8);
    }

    #[test]
    fn wrapped_up_read_uses_two_copies() {
        let mut memory = FakeMemory::initialized();
        memory.put(u64::from(UP_BUFFER) + 13, b"abc");
        memory.put(u64::from(UP_BUFFER), b"de");
        memory.put(UP_RD, &13u32.to_le_bytes());
        memory.put(UP_WR, &2u32.to_le_bytes());
        let mut channel = RttChannel::new(CB);
        assert_eq!(channel.read_up(&mut memory).unwrap(), b"abcde");
        assert_eq!(memory.word(UP_RD), 2);
        let reads: Vec<&String> = memory
            .log
            .iter()
            .filter(|entry| {
                entry.starts_with(&format!("read {UP_BUFFER:#x}"))
                    || entry.starts_with(&format!("read {:#x}", u64::from(UP_BUFFER) + 13))
            })
            .collect();
        assert_eq!(
            reads,
            [
                &format!("read {:#x} 3", u64::from(UP_BUFFER) + 13),
                &format!("read {UP_BUFFER:#x} 2")
            ]
        );
    }

    #[test]
    fn wrapped_up_read_ending_at_zero_needs_one_copy() {
        let mut memory = FakeMemory::initialized();
        memory.put(u64::from(UP_BUFFER) + 14, b"yz");
        memory.put(UP_RD, &14u32.to_le_bytes());
        memory.put(UP_WR, &0u32.to_le_bytes());
        let mut channel = RttChannel::new(CB);
        assert_eq!(channel.read_up(&mut memory).unwrap(), b"yz");
        assert_eq!(memory.word(UP_RD), 0);
    }

    #[test]
    fn invalid_descriptor_resets_initialization() {
        for (buffer, size, write, read) in [
            (0, 16, 0, 1),
            (UP_BUFFER, 1, 0, 0),
            (UP_BUFFER, 16, 16, 0),
            (UP_BUFFER, 16, 0, 16),
        ] {
            let mut memory = FakeMemory::initialized();
            memory.descriptor(CB + UP_DESCRIPTOR, buffer, size, write, read);
            let mut channel = RttChannel::new(CB);
            assert!(channel.read_up(&mut memory).unwrap().is_empty());
            assert!(!channel.initialized);
        }
    }

    #[test]
    fn down_write_publishes_wr_off_last() {
        let mut memory = FakeMemory::initialized();
        let mut channel = RttChannel::new(CB);
        assert_eq!(channel.write_down(&mut memory, b"ls\n").unwrap(), 3);
        assert_eq!(memory.get(u64::from(DOWN_BUFFER), 3), b"ls\n");
        assert_eq!(memory.word(DOWN_WR), 3);
        assert!(
            memory
                .log
                .last()
                .unwrap()
                .starts_with(&format!("write_u32 {DOWN_WR:#x} 3"))
        );
    }

    #[test]
    fn wrapped_down_write_uses_two_copies() {
        let mut memory = FakeMemory::initialized();
        memory.put(DOWN_WR, &6u32.to_le_bytes());
        memory.put(DOWN_RD, &5u32.to_le_bytes());
        let mut channel = RttChannel::new(CB);
        // size 8, WrOff 6, RdOff 5: 6 bytes free.
        assert_eq!(channel.write_down(&mut memory, b"abcdefgh").unwrap(), 6);
        assert_eq!(memory.get(u64::from(DOWN_BUFFER) + 6, 2), b"ab");
        assert_eq!(memory.get(u64::from(DOWN_BUFFER), 4), b"cdef");
        assert_eq!(memory.word(DOWN_WR), 4);
        let writes: Vec<&String> = memory
            .log
            .iter()
            .filter(|e| e.starts_with("write"))
            .collect();
        assert_eq!(writes.len(), 3);
        assert!(writes[2].starts_with("write_u32"));
    }

    #[test]
    fn full_down_buffer_accepts_nothing() {
        let mut memory = FakeMemory::initialized();
        memory.put(DOWN_WR, &4u32.to_le_bytes());
        memory.put(DOWN_RD, &5u32.to_le_bytes());
        let mut channel = RttChannel::new(CB);
        assert_eq!(channel.write_down(&mut memory, b"x").unwrap(), 0);
        assert!(!memory.log.iter().any(|entry| entry.starts_with("write")));
    }

    #[test]
    fn empty_input_touches_no_memory() {
        let mut memory = FakeMemory::initialized();
        let mut channel = RttChannel::new(CB);
        assert_eq!(channel.write_down(&mut memory, b"").unwrap(), 0);
        assert!(memory.log.is_empty());
    }

    #[test]
    fn memory_errors_propagate() {
        let mut memory = FakeMemory::initialized();
        memory.fail = true;
        let mut channel = RttChannel::new(CB);
        assert!(channel.read_up(&mut memory).is_err());
    }
}
