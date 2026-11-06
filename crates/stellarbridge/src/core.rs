//! A core through OpenOCD as a [`probecore::Target`]: physical memory
//! through a MEM-AP target (readable while the core runs), core registers
//! with `get_reg`, coprocessor registers with `aarch64 mrc` (AArch32 cores
//! of the aarch64/armv8r target types), the run state, halting, and the
//! symbols of the ELF files, since OpenOCD has no symbol table.

use probecore::elf::SymbolTable;
use probecore::fault::{self, SymbolRef};
use probecore::target::{self, Memory, Target, TargetAddress};

use crate::Server;

/// What the user runs to stop the core, for messages.
pub const HALT_COMMAND: &str = "rvdbg halt";

fn lift(error: oocd::Error) -> target::Error {
    target::Error {
        lost: matches!(error, oocd::Error::Connect(_) | oocd::Error::Timeout),
        message: error.to_string(),
    }
}

/// The run state as `<target> curstate` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Running,
    Halted,
    /// `reset`, `unknown`, `debug-running` and anything else.
    Other,
}

/// How [`CoreTarget::run_until`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunUntil {
    /// Where the core stopped; `None`: it still ran at the timeout or when
    /// `cancel` said to stop waiting.
    pub pc: Option<u64>,
    /// Waiting ended because `cancel` returned true.
    pub cancelled: bool,
    pub waited: std::time::Duration,
    pub breakpoint_removed: bool,
}

/// One core of the target, by its OpenOCD target name (`chip.k0c0.core`).
pub struct CoreTarget<'a> {
    pub server: &'a mut Server,
    pub core: String,
    /// The MEM-AP target for physical memory; without it, memory goes
    /// through the core, which may need it halted.
    pub memory_ap: Option<String>,
    pub symbols: SymbolTable,
}

/// The OpenOCD name of a core register written the TRACE32 way (`PC`,
/// `R13`, `ELR_HYP`) for an AArch32 core of the aarch64/armv8r types.
pub fn register_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "r13" => "sp".into(),
        "r14" => "lr".into(),
        "r15" | "pp" => "pc".into(),
        // OpenOCD's AArch64 names; on an AArch32 core they read ELR_hyp
        // and SPSR_hyp.
        "elr_hyp" | "elr_el2" => "ELR_EL2".into(),
        "spsr_hyp" | "spsr_el2" => "SPSR_EL2".into(),
        "sp" | "lr" | "pc" | "cpsr" => lower,
        _ if lower.len() <= 3
            && lower.starts_with('r')
            && lower[1..].parse::<u8>().is_ok_and(|n| n <= 12) =>
        {
            lower
        }
        _ => name.to_string(),
    }
}

/// `aarch64 mrc` arguments of a coprocessor address in the TRACE32 notation
/// (`C15:0x4025` is op1 4, CRn 5, CRm 2, op2 0: HSR): coprocessor, op1,
/// CRn, CRm, op2.
pub fn mrc_arguments(address: &TargetAddress) -> Option<[u64; 5]> {
    let class = address.class.to_ascii_uppercase();
    let coprocessor = match class.as_str() {
        "C15" => 15,
        "C14" => 14,
        _ => return None,
    };
    let value = address.value;
    if value > 0x7FFF {
        return None;
    }
    let crn = value & 0xF;
    let crm = (value >> 4) & 0xF;
    let op2 = (value >> 8) & 0x7;
    let op1 = (value >> 12) & 0x7;
    Some([coprocessor, op1, crn, crm, op2])
}

/// The value of `name` in a `get_reg` answer: a Tcl dict `name 0x... ...`.
fn dict_value(answer: &str, name: &str) -> Option<u64> {
    let words: Vec<&str> = answer.split_whitespace().collect();
    words.chunks(2).find_map(|pair| match pair {
        [key, value] if key.eq_ignore_ascii_case(name) => {
            let hex = value.strip_prefix("0x").unwrap_or(value);
            u64::from_str_radix(hex, 16).ok()
        }
        _ => None,
    })
}

impl CoreTarget<'_> {
    fn eval(&mut self, command: &str) -> target::Result<String> {
        self.server.client.eval(command).map_err(lift)
    }

    pub fn run_state(&mut self) -> target::Result<RunState> {
        let answer = self.eval(&format!("{} curstate", oocd::quote(&self.core)))?;
        Ok(match answer.trim() {
            "running" | "debug-running" => RunState::Running,
            "halted" => RunState::Halted,
            _ => RunState::Other,
        })
    }

    /// Let the core run from where it stopped.
    pub fn resume(&mut self) -> target::Result<()> {
        self.eval(&format!("{} resume", oocd::quote(&self.core)))
            .map(|_| ())
    }

    /// Reset through the target configuration's reset events and stop at
    /// the reset vector (`reset halt`).
    pub fn reset_halt(&mut self) -> target::Result<()> {
        self.server
            .client
            .set_timeout(std::time::Duration::from_secs(60))
            .map_err(lift)?;
        let result = self.eval("reset halt").map(|_| ());
        self.server
            .client
            .set_timeout(std::time::Duration::from_secs(5))
            .map_err(lift)?;
        result
    }

    /// Run a halted core until it stops, with a hardware breakpoint at
    /// `address` (the code runs from flash, where a software breakpoint
    /// cannot be written). The breakpoint is removed when the core stops;
    /// on a timeout the core keeps running and the breakpoint is removed if
    /// OpenOCD allows it while the core runs.
    pub fn run_until(
        &mut self,
        address: u64,
        timeout: std::time::Duration,
        cancel: &dyn Fn() -> bool,
    ) -> target::Result<RunUntil> {
        if self.run_state()? != RunState::Halted {
            return Err(target::Error::new(format!(
                "the core is not halted; run-until starts a halted core (stop it with \
                 '{HALT_COMMAND}')"
            )));
        }
        let core = oocd::quote(&self.core);
        // bp and rbp act on the current target.
        self.eval(&format!("targets {core}"))?;
        self.eval(&format!("bp 0x{address:X} 4 hw"))
            .map_err(|error| error.context(format!("cannot set a breakpoint at 0x{address:X}")))?;
        if let Err(error) = self.resume() {
            let _ = self.eval(&format!("rbp 0x{address:X}"));
            return Err(error);
        }
        let start = std::time::Instant::now();
        loop {
            match self.run_state() {
                Ok(RunState::Halted) => break,
                Ok(_) => {}
                Err(error) => return Err(error),
            }
            let cancelled = cancel();
            if cancelled || start.elapsed() > timeout {
                let removed = self.eval(&format!("rbp 0x{address:X}")).is_ok();
                return Ok(RunUntil {
                    pc: None,
                    waited: start.elapsed(),
                    breakpoint_removed: removed,
                    cancelled,
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let waited = start.elapsed();
        let removed = self.eval(&format!("rbp 0x{address:X}")).is_ok();
        let pc = self.read_core_register("PC")?;
        Ok(RunUntil {
            pc: Some(pc),
            waited,
            breakpoint_removed: removed,
            cancelled: false,
        })
    }

    fn require_halted(&mut self, what: &str) -> target::Result<()> {
        match self.run_state()? {
            RunState::Halted => Ok(()),
            _ => Err(target::Error::new(format!(
                "{what} can only be read while the core is halted, and it is not; \
                 stop it with '{HALT_COMMAND}' (or a breakpoint), then try again"
            ))),
        }
    }

    /// The symbol an address belongs to, from the ELF files.
    pub fn symbolize(&self, address: u64) -> Option<SymbolRef> {
        self.symbols
            .lookup(address)
            .map(|(symbol, offset)| SymbolRef {
                path: symbol.name.clone(),
                offset: Some(offset),
            })
    }
}

impl Target for CoreTarget<'_> {
    fn read_u32(&mut self, address: &TargetAddress) -> target::Result<u32> {
        if let Some([coprocessor, op1, crn, crm, op2]) = mrc_arguments(address) {
            self.require_halted(&format!("{address} (a coprocessor register)"))?;
            let answer = self
                .eval(&format!(
                    "{} aarch64 mrc {coprocessor} {op1} {crn} {crm} {op2}",
                    oocd::quote(&self.core)
                ))
                .map_err(|error| error.context(address))?;
            let value = oocd::parse_numbers(&answer)
                .ok()
                .and_then(|values| values.first().copied())
                .ok_or_else(|| {
                    target::Error::new(format!("{address}: unexpected answer {answer:?}"))
                })?;
            return Ok(value as u32);
        }
        if !matches!(
            address.class.to_ascii_uppercase().as_str(),
            "" | "A" | "AD" | "D"
        ) {
            return Err(target::Error::new(format!(
                "access class {} is not supported by OpenOCD (physical memory AD:, \
                 coprocessors C15: and C14:)",
                address.class
            )));
        }
        let bytes = self.read_memory(address.value, 4)?;
        Ok(u32::from_le_bytes(bytes.try_into().expect("4 bytes")))
    }

    fn read_core_register(&mut self, name: &str) -> target::Result<u64> {
        self.require_halted(&format!("core register {name}"))?;
        let register = register_name(name);
        let answer = self
            .eval(&format!(
                "{} get_reg -force {}",
                oocd::quote(&self.core),
                oocd::quote(&register)
            ))
            .map_err(|error| error.context(format!("core register {name}")))?;
        dict_value(&answer, &register).ok_or_else(|| {
            target::Error::new(format!(
                "core register {name}: unexpected answer {answer:?}"
            ))
        })
    }

    fn read_memory(&mut self, address: u64, length: usize) -> target::Result<Vec<u8>> {
        let via = self.memory_ap.clone().unwrap_or_else(|| self.core.clone());
        self.server
            .client
            .read_memory(&via, address, length)
            .map_err(lift)
    }

    fn is_running(&mut self) -> target::Result<bool> {
        Ok(self.run_state()? == RunState::Running)
    }

    fn halt(&mut self) -> target::Result<()> {
        let core = oocd::quote(&self.core);
        self.eval(&format!("{core} arp_halt"))?;
        self.eval(&format!("{core} arp_waitstate halted 1000"))
            .map(|_| ())
            .map_err(|error| error.context("the core did not halt"))
    }

    fn symbol_range(&mut self, name: &str) -> target::Result<(u64, u64)> {
        self.symbols.range(name).ok_or_else(|| {
            target::Error::new(format!(
                "{name} is not in the symbol table of the ELF files"
            ))
        })
    }

    fn break_command(&self) -> &str {
        HALT_COMMAND
    }
}

/// Physical memory through a MEM-AP while the core runs, for RTT.
pub struct ApMemory<'a> {
    pub server: &'a mut Server,
    pub memory_ap: String,
}

impl Memory for ApMemory<'_> {
    fn read(&mut self, address: u64, length: usize) -> target::Result<Vec<u8>> {
        self.server
            .client
            .read_memory(&self.memory_ap, address, length)
            .map_err(lift)
    }

    fn write(&mut self, address: u64, data: &[u8]) -> target::Result<()> {
        self.server
            .client
            .write_memory(&self.memory_ap, address, data)
            .map_err(lift)
    }

    fn write_u32(&mut self, address: u64, value: u32) -> target::Result<()> {
        self.server
            .client
            .write_u32(&self.memory_ap, address, value)
            .map_err(lift)
    }
}

/// Read the registers of the Hyp fault report (the core must be halted).
pub fn fault_facts(target: &mut CoreTarget) -> target::Result<fault::Facts> {
    if target.run_state()? != RunState::Halted {
        return Err(target::Error::new(format!(
            "The fault registers (CP15, ELR_hyp, SPSR_hyp) can only be read while the core \
             is halted, and it is not; stop it with '{HALT_COMMAND}' (or a breakpoint), then \
             try again"
        )));
    }
    // The core's mode, for the error when the Hyp registers cannot be read.
    let mode = target
        .read_core_register("CPSR")
        .ok()
        .map(|cpsr| probecore::decode::decode_psr(cpsr as u32));
    let not_hyp = mode
        .as_ref()
        .filter(|psr| psr.mode != 0x1A)
        .map(|psr| psr.mode_name.unwrap_or("an unknown").to_string());
    let no_hyp = |what: &str, error: target::Error| -> target::Error {
        if error.lost {
            return error;
        }
        match &not_hyp {
            Some(mode) => error.context(format!(
                "cannot read {what}: the core is halted in {mode} mode, not in Hyp mode, and \
                 OpenOCD reads the Hyp registers by executing instructions in the current \
                 mode; stop the core in EL2, e.g. at a breakpoint in the hypervisor"
            )),
            None => error.context(format!(
                "cannot read {what}; fault needs an AArch32 core with Hyp mode"
            )),
        }
    };
    let cp15 = |target: &mut CoreTarget, text: &str, what: &str| {
        let address = TargetAddress::parse(text).expect("constant address");
        target
            .read_u32(&address)
            .map(u64::from)
            .map_err(|error| no_hyp(what, error))
    };
    let hsr = cp15(target, fault::HSR, "HSR (C15:0x4025)")? as u32;
    let hdfar = cp15(target, fault::HDFAR, "HDFAR (C15:0x4006)")?;
    let hifar = cp15(target, fault::HIFAR, "HIFAR (C15:0x4206)")?;
    let hvbar = cp15(target, fault::HVBAR, "HVBAR (C15:0x400C)")?;
    let elr = target
        .read_core_register("ELR_hyp")
        .map_err(|error| no_hyp("ELR_hyp", error))?;
    let spsr = target
        .read_core_register("SPSR_hyp")
        .map_err(|error| no_hyp("SPSR_hyp", error))?;
    let pc = target.read_core_register("PC")?;
    Ok(fault::Facts {
        pc,
        pc_symbol: target.symbolize(pc),
        hvbar,
        hvbar_symbol: target.symbolize(hvbar),
        hsr,
        hdfar,
        hifar,
        elr,
        elr_symbol: target.symbolize(elr),
        spsr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coprocessor_addresses_become_mrc_arguments() {
        let parse = |text| mrc_arguments(&TargetAddress::parse(text).unwrap());
        // HSR c5,c2,0 op1 4; HIFAR c6,c0,2; HVBAR c12,c0,0; SCTLR c1,c0,0.
        assert_eq!(parse("C15:0x4025"), Some([15, 4, 5, 2, 0]));
        assert_eq!(parse("C15:0x4206"), Some([15, 4, 6, 0, 2]));
        assert_eq!(parse("C15:0x400C"), Some([15, 4, 12, 0, 0]));
        assert_eq!(parse("c15:0x0001"), Some([15, 0, 1, 0, 0]));
        assert_eq!(parse("C14:0x0070"), Some([14, 0, 0, 7, 0]));
        assert_eq!(parse("AD:0x4025"), None);
        assert_eq!(parse("C15:0x10030"), None);
    }

    #[test]
    fn register_names_follow_openocd() {
        assert_eq!(register_name("PC"), "pc");
        assert_eq!(register_name("PP"), "pc");
        assert_eq!(register_name("R13"), "sp");
        assert_eq!(register_name("R7"), "r7");
        assert_eq!(register_name("CPSR"), "cpsr");
        assert_eq!(register_name("ELR_HYP"), "ELR_EL2");
        assert_eq!(register_name("SPSR_hyp"), "SPSR_EL2");
        assert_eq!(register_name("R99"), "R99");
        assert_eq!(register_name("fpscr"), "fpscr");
    }

    /// A Tcl server that answers each command with the answer of the first
    /// pattern it contains, and records the commands.
    fn tcl_server(
        answers: Vec<(&'static str, &'static str)>,
    ) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            let mut buffer = Vec::new();
            let mut byte = [0u8; 1];
            while let Ok(1) = stream.read(&mut byte) {
                if byte[0] != oocd::TERMINATOR {
                    buffer.push(byte[0]);
                    continue;
                }
                let command = String::from_utf8(std::mem::take(&mut buffer)).unwrap();
                let answer = answers
                    .iter()
                    .find(|(pattern, _)| command.contains(pattern))
                    .map(|(_, answer)| *answer)
                    .unwrap_or("1 unexpected command");
                received.push(command);
                stream.write_all(answer.as_bytes()).unwrap();
                stream.write_all(&[oocd::TERMINATOR]).unwrap();
            }
            received
        });
        (port, handle)
    }

    #[test]
    fn fault_reads_cp15_with_mrc_and_the_hyp_registers_with_get_reg() {
        let (port, handle) = tcl_server(vec![
            ("curstate", "0 halted"),
            ("get_reg -force cpsr", "0 cpsr 0x0000001a"),
            ("aarch64 mrc 15 4 5 2 0", "0 0x92000010"),
            ("aarch64 mrc 15 4 6 0 0", "0 0x40000000"),
            ("aarch64 mrc 15 4 6 0 2", "0 0x0"),
            ("aarch64 mrc 15 4 12 0 0", "0 0x28001000"),
            ("get_reg -force ELR_EL2", "0 ELR_EL2 0x0000000028002004"),
            ("get_reg -force SPSR_EL2", "0 SPSR_EL2 0x00000000600001d3"),
            ("get_reg -force pc", "0 pc 0x28001010"),
        ]);
        let mut server = Server::attach(port).unwrap();
        assert!(!server.owned());
        let mut symbols = SymbolTable::default();
        symbols.symbols.push(probecore::elf::Symbol {
            name: "__hyp_vectors".into(),
            value: 0x2800_1000,
            size: 0x40,
        });
        let mut target = CoreTarget {
            server: &mut server,
            core: "chip.core".into(),
            memory_ap: Some("chip.axi".into()),
            symbols,
        };
        let facts = fault_facts(&mut target).unwrap();
        assert_eq!(facts.hsr, 0x9200_0010);
        assert_eq!(facts.hvbar, 0x2800_1000);
        assert_eq!(facts.hdfar, 0x4000_0000);
        assert_eq!(
            (facts.elr, facts.spsr, facts.pc),
            (0x2800_2004, 0x6000_01D3, 0x2800_1010)
        );
        assert_eq!(facts.pc_symbol.unwrap().describe(), "__hyp_vectors+0x10");
        drop(target);
        drop(server);
        let received = handle.join().unwrap();
        assert!(
            received
                .iter()
                .any(|c| c.contains("{chip.core aarch64 mrc 15 4 5 2 0}"))
        );
    }

    #[test]
    fn run_until_sets_a_hardware_breakpoint_and_removes_it() {
        let (port, handle) = tcl_server(vec![
            ("curstate", "0 halted"),
            ("targets", "0 "),
            ("bp 0x28000100 4 hw", "0 "),
            ("resume", "0 "),
            ("rbp 0x28000100", "0 "),
            ("get_reg -force pc", "0 pc 0x28000100"),
        ]);
        let mut server = Server::attach(port).unwrap();
        let mut target = CoreTarget {
            server: &mut server,
            core: "chip.core".into(),
            memory_ap: None,
            symbols: SymbolTable::default(),
        };
        let result = target
            .run_until(0x2800_0100, std::time::Duration::from_secs(1), &|| false)
            .unwrap();
        assert_eq!(result.pc, Some(0x2800_0100));
        assert!(result.breakpoint_removed);
        drop(target);
        drop(server);
        let received = handle.join().unwrap();
        let order: Vec<usize> = [
            "{targets chip.core}",
            "{bp 0x28000100 4 hw}",
            "{chip.core resume}",
            "{rbp 0x28000100}",
        ]
        .iter()
        .map(|needle| received.iter().position(|c| c.contains(needle)).unwrap())
        .collect();
        assert!(
            order.windows(2).all(|pair| pair[0] < pair[1]),
            "{received:?}"
        );
    }

    #[test]
    fn a_running_core_is_not_read() {
        let (port, _handle) = tcl_server(vec![("curstate", "0 running")]);
        let mut server = Server::attach(port).unwrap();
        let mut target = CoreTarget {
            server: &mut server,
            core: "chip.core".into(),
            memory_ap: None,
            symbols: SymbolTable::default(),
        };
        assert!(target.is_running().unwrap());
        let error = fault_facts(&mut target).unwrap_err();
        assert!(error.message.contains("rvdbg halt"), "{error}");
        let address = TargetAddress::parse("C15:0x4025").unwrap();
        let error = target.read_u32(&address).unwrap_err();
        assert!(error.message.contains("halted"), "{error}");
        let error = target
            .read_u32(&TargetAddress::parse("E:0x0").unwrap())
            .unwrap_err();
        assert!(error.message.contains("access class E"), "{error}");
    }

    #[test]
    fn get_reg_answers_are_dicts() {
        assert_eq!(
            dict_value("pc 0x28000100 cpsr 0x000001da", "cpsr"),
            Some(0x1DA)
        );
        assert_eq!(
            dict_value("ELR_EL2 0x0000000028002004", "ELR_EL2"),
            Some(0x2800_2004)
        );
        assert_eq!(dict_value("pc 0x1", "sp"), None);
    }
}
