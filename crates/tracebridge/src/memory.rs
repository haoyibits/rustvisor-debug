//! Target RAM through PowerView while the core runs: the memory access of
//! the RAM channels between the debugger and the target (RTT, FDX).

use probecore::Memory;
use t32rcl::{Address, Debugger};

/// RAM through one access class of a PowerView connection.
pub struct RamMemory<'a> {
    debugger: &'a mut Debugger,
    class: &'static str,
}

impl<'a> RamMemory<'a> {
    /// `AD:`: physical addresses through the debug port, as the board
    /// runner's FDX exchanges use them.
    pub(crate) fn absolute(debugger: &'a mut Debugger) -> Self {
        RamMemory {
            debugger,
            class: "AD",
        }
    }

    /// `E:`: TRACE32's run-time access class (the RTT terminal).
    pub(crate) fn run_time(debugger: &'a mut Debugger) -> Self {
        RamMemory {
            debugger,
            class: "E",
        }
    }

    fn address(&self, value: u64) -> Address {
        Address::new(Some(self.class), value)
    }
}

fn lift<T>(result: t32rcl::Result<T>) -> probecore::Result<T> {
    result.map_err(|error| probecore::Error::new(error.to_string()))
}

impl Memory for RamMemory<'_> {
    fn read(&mut self, address: u64, length: usize) -> probecore::Result<Vec<u8>> {
        let at = self.address(address);
        lift(self.debugger.memory_read(&at, length))
    }

    fn read_u32s(&mut self, address: u64, count: usize) -> probecore::Result<Vec<u32>> {
        if address % 4 != 0 {
            return Err(probecore::Error::new(format!(
                "word read at unaligned address {address:#x}"
            )));
        }
        let at = self.address(address);
        let bytes = lift(self.debugger.memory_read_width(&at, 4 * count, 4))?;
        Ok(bytes
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().expect("4 bytes")))
            .collect())
    }

    fn write(&mut self, address: u64, data: &[u8]) -> probecore::Result<()> {
        let at = self.address(address);
        lift(self.debugger.memory_write(&at, data))
    }

    fn write_u32(&mut self, address: u64, value: u32) -> probecore::Result<()> {
        let at = self.address(address);
        lift(self.debugger.memory_write_u32(&at, value))
    }
}
