//! `probecore::Target` on top of PowerView: every read is evaluated on the
//! PowerView side, as everywhere else in `debug` (see `probe`).

use probecore::target::{self, BackendRead, Target, TargetAddress};
use t32rcl::Address;

use super::probe::{self, DebugError, PerRegister, PerSnapshot, Probe};

/// A read of a check file that only TRACE32 has.
enum Prepared {
    /// `read.reg`: a register or field of the PER file.
    Register(PerRegister),
    /// `read.expr`: a PRACTICE expression.
    Expression(String),
}

pub struct T32Target<'a> {
    probe: &'a mut dyn Probe,
    /// For `read.reg`; reads without PER names need none.
    per: Option<&'a mut PerSnapshot>,
    prepared: Vec<Prepared>,
}

impl<'a> T32Target<'a> {
    pub fn new(probe: &'a mut dyn Probe) -> Self {
        T32Target {
            probe,
            per: None,
            prepared: Vec::new(),
        }
    }
}

fn no_per() -> target::Error {
    target::Error::new("read.reg needs the PER file")
}

fn lift<T>(result: probe::DResult<T>) -> target::Result<T> {
    result.map_err(target::Error::from)
}

impl Target for T32Target<'_> {
    /// `Data.Long(<address>)`, with the HC15 fallback of `probe::read_long`.
    fn read_u32(&mut self, address: &TargetAddress) -> target::Result<u32> {
        let value = lift(probe::read_long(self.probe, address))?;
        u32::try_from(value).map_err(|_| {
            target::Error::new(format!(
                "Data.Long({address}) returned 0x{value:X}, not a 32-bit value"
            ))
        })
    }

    fn read_core_register(&mut self, name: &str) -> target::Result<u64> {
        lift(probe::eval_u64(self.probe, &format!("Register({name})")))
    }

    /// The RCL memory API at `AD:` (physical, bypassing the caches).
    fn read_memory(&mut self, address: u64, length: usize) -> target::Result<Vec<u8>> {
        self.probe
            .read_memory(&Address::new(Some("AD"), address), length)
            .map_err(|error| DebugError::from(error).into())
    }

    fn is_running(&mut self) -> target::Result<bool> {
        lift(probe::core_running(self.probe))
    }

    /// `Break`; the PER snapshot is stale afterwards and taken again when
    /// PER registers were resolved.
    fn halt(&mut self) -> target::Result<()> {
        self.probe
            .cmd("Break")
            .map_err(|error| target::Error::from(DebugError::from(error)))?;
        let registers = self
            .prepared
            .iter()
            .any(|prepared| matches!(prepared, Prepared::Register(_)));
        if let Some(per) = self.per.as_deref_mut() {
            per.invalidate();
            if registers {
                lift(per.ensure(self.probe))?;
            }
        }
        Ok(())
    }

    fn symbol_range(&mut self, name: &str) -> target::Result<(u64, u64)> {
        let (begin, end) = lift(probe::symbol_range(self.probe, name))?;
        Ok((begin.value, end))
    }

    fn symbol_address(&mut self, name: &str) -> target::Result<u64> {
        Ok(lift(probe::symbol_address(self.probe, name))?.value)
    }

    fn break_command(&self) -> &str {
        "tracebridge debug break"
    }

    fn begin_reads(&mut self, kinds: &[&str]) -> target::Result<()> {
        if kinds.contains(&"reg") {
            let per = self.per.as_deref_mut().ok_or_else(no_per)?;
            lift(per.ensure(self.probe))?;
        }
        Ok(())
    }

    fn prepare_read(&mut self, kind: &str, argument: &str) -> target::Result<BackendRead> {
        let (label, needs_halt, prepared) = match kind {
            "reg" => {
                let per = self.per.as_deref_mut().ok_or_else(no_per)?;
                let register = lift(probe::resolve_register(self.probe, per, argument))?;
                (
                    format!("reg {} ({})", register.path, register.address),
                    register.address.is_coprocessor(),
                    Prepared::Register(register),
                )
            }
            "expr" => (
                format!("expr {argument}"),
                false,
                Prepared::Expression(argument.to_string()),
            ),
            _ => {
                return Err(target::Error::new(format!(
                    "read.{kind} is not supported by TRACE32"
                )));
            }
        };
        self.prepared.push(prepared);
        Ok(BackendRead {
            label,
            needs_halt,
            id: self.prepared.len() - 1,
        })
    }

    fn read_prepared(&mut self, read: &BackendRead) -> target::Result<u64> {
        match self.prepared.get(read.id) {
            Some(Prepared::Register(register)) => lift(probe::read_register(self.probe, register)),
            Some(Prepared::Expression(expression)) => lift(probe::eval_u64(self.probe, expression)),
            None => Err(target::Error::new("unknown prepared read")),
        }
    }
}
