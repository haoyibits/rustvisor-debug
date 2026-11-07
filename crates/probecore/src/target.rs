//! What the analysis code needs from a debug probe: typed reads, the run
//! state and symbol addresses. Each backend implements [`Target`] (and
//! [`Memory`] for RTT) on top of its own protocol.

use std::fmt;

/// A failed target operation. `lost` means the connection to the probe is
/// gone: it ends a whole check run, while any other error fails only the one
/// check that hit it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub message: String,
    pub lost: bool,
}

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Error {
            message: message.into(),
            lost: false,
        }
    }

    /// Put `context: ` in front of the message.
    pub fn context(mut self, context: impl fmt::Display) -> Self {
        self.message = format!("{context}: {}", self.message);
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// A classified address as check files and clock descriptions write it (the
/// TRACE32 notation): `C15:0x4025`, `AD:0x70F40000`. A backend rejects access
/// classes it cannot read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetAddress {
    /// Access class; empty when there is none.
    pub class: String,
    pub value: u64,
}

impl TargetAddress {
    /// `<class>:<number>` with a `0x` hexadecimal or decimal number (TRACE32
    /// also marks decimals with a trailing dot). The class is letters and
    /// digits only, so register names and PER paths never parse.
    pub fn parse(text: &str) -> Option<TargetAddress> {
        let text = text.trim();
        let (class, number) = text.rsplit_once(':')?;
        if !class.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        let value = if let Some(hex) = number
            .strip_prefix("0x")
            .or_else(|| number.strip_prefix("0X"))
        {
            u64::from_str_radix(hex, 16).ok()?
        } else {
            let digits = number.strip_suffix('.').unwrap_or(number);
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            digits.parse().ok()?
        };
        Some(TargetAddress {
            class: class.to_string(),
            value,
        })
    }

    /// Coprocessor registers (`C15:`, `C14:` and their variants) are core
    /// registers: they can only be read while the core is halted.
    pub fn is_coprocessor(&self) -> bool {
        let class = self.class.to_ascii_uppercase();
        class.ends_with("C15") || class.ends_with("C14")
    }

    /// A Hyp-mode CP15 register: class `C15` and op1 = 4 (bits 12-14 of the
    /// command-line address), e.g. HCR `C15:0x4011`, HSR `C15:0x4025`.
    pub fn is_hyp_cp15(&self) -> bool {
        self.class.eq_ignore_ascii_case("C15") && (self.value >> 12) & 0x7 == 4
    }

    /// The same class at `value + offset`.
    pub fn offset(&self, offset: u64) -> TargetAddress {
        TargetAddress {
            class: self.class.clone(),
            value: self.value.wrapping_add(offset),
        }
    }
}

impl fmt::Display for TargetAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.class.is_empty() {
            write!(f, "0x{:X}", self.value)
        } else if self.is_coprocessor() {
            // As PER files write them: c15:0x000E.
            write!(f, "{}:0x{:04X}", self.class, self.value)
        } else {
            write!(f, "{}:0x{:X}", self.class, self.value)
        }
    }
}

/// `0x` and eight digits up to 32 bits, all digits above.
pub fn hex32(value: u64) -> String {
    if value <= 0xFFFF_FFFF {
        format!("0x{value:08X}")
    } else {
        format!("0x{value:X}")
    }
}

/// A backend-specific read of a check file, resolved before anything is read
/// (TRACE32: `read.reg` through the PER file, `read.expr` in PRACTICE).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendRead {
    /// What the `read` column shows, e.g. `reg HSR (C15:0x4025)`.
    pub label: String,
    /// The value is a core or coprocessor register: the core must be halted.
    pub needs_halt: bool,
    /// The backend's own handle for [`Target::read_prepared`].
    pub id: usize,
}

/// Typed access to a target through a debug probe.
pub trait Target {
    /// One 32-bit read at a classified address.
    fn read_u32(&mut self, address: &TargetAddress) -> Result<u32>;
    /// A core register by its architecture name (`PC`, `CPSR`, `R0` ...).
    fn read_core_register(&mut self, name: &str) -> Result<u64>;
    /// Plain physical memory, `length` bytes from `address`.
    fn read_memory(&mut self, address: u64, length: usize) -> Result<Vec<u8>>;
    /// Whether the core runs. Backends report "not running" when the state
    /// cannot be read but the connection is fine.
    fn is_running(&mut self) -> Result<bool>;
    /// Stop the core.
    fn halt(&mut self) -> Result<()>;
    /// First and last address of a symbol.
    fn symbol_range(&mut self, name: &str) -> Result<(u64, u64)>;
    /// First address of a symbol.
    fn symbol_address(&mut self, name: &str) -> Result<u64> {
        Ok(self.symbol_range(name)?.0)
    }
    /// How the user stops the core with this tool, for messages
    /// (`tracebridge debug break`).
    fn break_command(&self) -> &str;

    /// Called once before the checks are resolved, with the backend-specific
    /// read kinds they use (`reg`, `expr`). An error ends the run.
    fn begin_reads(&mut self, kinds: &[&str]) -> Result<()> {
        let _ = kinds;
        Ok(())
    }
    /// Resolve a backend-specific read of a check file.
    fn prepare_read(&mut self, kind: &str, argument: &str) -> Result<BackendRead> {
        let _ = argument;
        Err(Error::new(format!(
            "read.{kind} is not supported by this backend"
        )))
    }
    /// Read what [`Target::prepare_read`] resolved.
    fn read_prepared(&mut self, read: &BackendRead) -> Result<u64> {
        let _ = read;
        Err(Error::new("no backend-specific reads are supported"))
    }
}

/// Memory access while the core runs, with writes: what the RAM channels
/// between the debugger and the target (RTT, FDX) need.
pub trait Memory {
    fn read(&mut self, address: u64, length: usize) -> Result<Vec<u8>>;
    /// `count` little-endian words from a word-aligned `address`, each read
    /// in one 32-bit access, so that a word the target updates is never seen
    /// half old and half new.
    fn read_u32s(&mut self, address: u64, count: usize) -> Result<Vec<u32>>;
    fn write(&mut self, address: u64, data: &[u8]) -> Result<()>;
    /// One aligned 32-bit write in a single access.
    fn write_u32(&mut self, address: u64, value: u32) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_parse_and_print_in_the_data_file_notation() {
        let address = TargetAddress::parse("C15:0x4025").unwrap();
        assert!(address.is_coprocessor() && address.is_hyp_cp15());
        assert_eq!(address.to_string(), "C15:0x4025");
        assert_eq!(
            TargetAddress::parse("c14:0x7").unwrap().to_string(),
            "c14:0x0007"
        );
        let address = TargetAddress::parse("AD:4096.").unwrap();
        assert_eq!(address.to_string(), "AD:0x1000");
        assert_eq!(address.offset(4).value, 0x1004);
        assert!(TargetAddress::parse("HSCTLR.M").is_none());
        assert!(TargetAddress::parse(".a.b:0x1").is_none());
    }

    #[test]
    fn errors_carry_context_and_the_lost_flag() {
        let error = Error::new("bus error").context("AD:0x0");
        assert_eq!(error.to_string(), "AD:0x0: bus error");
        assert!(!error.lost);
        assert_eq!(hex32(0x10), "0x00000010");
        assert_eq!(hex32(0x1_0000_0000), "0x100000000");
    }
}
