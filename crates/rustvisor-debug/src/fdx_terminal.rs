//! `rvdbg fdx`: talk to an image over its FDX link while the cores run,
//! through any memory access that works then (TRACE32 `AD:`, the OpenOCD
//! MEM-AP).
//!
//! Interactive, each line typed is one packet to the target and each packet
//! from it is printed as one line, until Ctrl-C. With `--send`/`--expect`
//! it sends, waits for the expected packets and exits: what a script or the
//! board runner needs.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use probecore::Memory;
use probecore::fdx::{Error, Packet};
use probecore::style::Style;
use rustvisor_debug::fdx::Link;
use serde_json::json;

use crate::EXIT_FAILED;

/// How often the link is polled.
const POLL: Duration = Duration::from_millis(20);

pub struct Options {
    /// The probe's name in the status lines (`TRACE32`, `StellarLINK`).
    pub probe: &'static str,
    pub symbol: String,
    pub address: u64,
    /// Packets to send, then exit.
    pub send: Vec<String>,
    /// Packets to wait for after sending, in order.
    pub expect: Vec<String>,
    /// For the link to open, for room, and for each expected packet.
    pub timeout: Duration,
    pub json: bool,
}

impl Options {
    fn scripted(&self) -> bool {
        !self.send.is_empty() || !self.expect.is_empty()
    }
}

/// Runs `rvdbg fdx`; Ctrl-C sets `interrupted`.
pub fn run(
    memory: &mut impl Memory,
    options: &Options,
    interrupted: &AtomicBool,
) -> Result<u8, String> {
    if options.scripted() {
        scripted(memory, options, interrupted)
    } else {
        interactive(memory, options, interrupted)
    }
}

fn text(packet: &[u8]) -> String {
    String::from_utf8_lossy(packet).into_owned()
}

/// Waits until the target opens the link; `None` if `deadline` passes or
/// Ctrl-C comes first.
fn open(
    memory: &mut impl Memory,
    address: u64,
    deadline: Option<Instant>,
    interrupted: &AtomicBool,
) -> Result<Option<Link>, String> {
    loop {
        if let Some(link) = Link::locate(memory, address).map_err(|error| error.to_string())? {
            return Ok(Some(link));
        }
        if interrupted.load(Ordering::SeqCst) || deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok(None);
        }
        std::thread::sleep(POLL);
    }
}

fn scripted(
    memory: &mut impl Memory,
    options: &Options,
    interrupted: &AtomicBool,
) -> Result<u8, String> {
    let deadline = || Some(Instant::now() + options.timeout);
    let stopped = || {
        if interrupted.load(Ordering::SeqCst) {
            Err("interrupted".to_string())
        } else {
            Ok(())
        }
    };
    let Some(link) = open(memory, options.address, deadline(), interrupted)? else {
        stopped()?;
        return Err(format!(
            "{} at 0x{:08X} did not open within {:?}; is the image running?",
            options.symbol, options.address, options.timeout
        ));
    };
    for packet in &options.send {
        if packet.is_empty() || packet.len() > link.max_payload() {
            return Err(format!(
                "a packet must have 1 to {} bytes; {packet:?} has {}",
                link.max_payload(),
                packet.len()
            ));
        }
        let until = deadline().expect("deadline");
        while !link
            .send(memory, packet.as_bytes())
            .map_err(|error| error.to_string())?
        {
            stopped()?;
            if Instant::now() >= until {
                return Err(format!(
                    "no room for {packet:?} within {:?}: the target does not read",
                    options.timeout
                ));
            }
            std::thread::sleep(POLL);
        }
    }

    let mut received = Vec::new();
    let mut dropped = 0;
    let mut missing = None;
    let mut stdout = std::io::stdout();
    'expect: for wanted in &options.expect {
        let until = deadline().expect("deadline");
        loop {
            match link.receive(memory).map_err(|error| error.to_string())? {
                Some(Packet::Data(packet)) => {
                    let packet = text(&packet);
                    if !options.json {
                        let _ = writeln!(stdout, "{packet}");
                    }
                    let found = packet == *wanted;
                    received.push(packet);
                    if found {
                        continue 'expect;
                    }
                }
                Some(Packet::Dropped) => dropped += 1,
                None if Instant::now() >= until => {
                    missing = Some(wanted.clone());
                    break 'expect;
                }
                None => {
                    stopped()?;
                    std::thread::sleep(POLL);
                }
            }
        }
    }

    if options.json {
        println!(
            "{}",
            json!({
                "symbol": options.symbol,
                "address": format!("0x{:08X}", options.address),
                "size": link.to_target.size,
                "sent": options.send,
                "received": received,
                "dropped": dropped,
                "expected": options.expect,
                "missing": missing,
                "ok": missing.is_none(),
            })
        );
    }
    let style = Style::stderr();
    if dropped > 0 {
        eprintln!(
            "{} the target dropped packets {dropped} time(s): its ring was full",
            style.warn("[fdx]")
        );
    }
    match missing {
        Some(wanted) => {
            if !options.json {
                eprintln!(
                    "{} no {wanted:?} within {:?}",
                    style.bad("[fdx]"),
                    options.timeout
                );
            }
            Ok(EXIT_FAILED)
        }
        None => Ok(0),
    }
}

fn interactive(
    memory: &mut impl Memory,
    options: &Options,
    interrupted: &AtomicBool,
) -> Result<u8, String> {
    let style = Style::stderr();
    let probe = options.probe;
    eprintln!(
        "{} {} @ {}; one line is one packet; Ctrl-C to stop",
        style.label(format!("{probe} FDX:")),
        style.symbol(&options.symbol),
        style.address(format!("0x{:08X}", options.address))
    );
    let mut stdout = std::io::stdout();
    let mut input_open = true;
    let mut pending: Vec<u8> = Vec::new();
    let mut queue: std::collections::VecDeque<String> = Default::default();
    let mut consecutive_errors = 0u64;
    'link: while !interrupted.load(Ordering::SeqCst) {
        let link = match Link::locate(memory, options.address) {
            Ok(Some(link)) => link,
            Ok(None) => {
                eprintln!(
                    "{} waiting for the target to open the link",
                    style.warn("[fdx]")
                );
                match open(memory, options.address, None, interrupted)? {
                    Some(link) => link,
                    None => break,
                }
            }
            Err(Error::Memory(error)) if error.lost => {
                return Err(format!("FDX: the connection to the probe is gone: {error}"));
            }
            Err(error) => return Err(format!("FDX: {error}")),
        };
        eprintln!(
            "{} open, {} bytes each way, packets up to {} bytes",
            style.label("[fdx]"),
            link.to_target.size,
            link.max_payload()
        );
        while !interrupted.load(Ordering::SeqCst) {
            if input_open {
                match tracebridge::backend::read_stdin_now() {
                    Some(bytes) => pending.extend(bytes),
                    None => input_open = false,
                }
                while let Some(end) = pending.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = pending.drain(..=end).collect();
                    let line = text(&line).trim_end_matches(['\r', '\n']).to_string();
                    if line.is_empty() {
                        continue;
                    }
                    if line.len() > link.max_payload() {
                        eprintln!(
                            "{} not sent: {} bytes, the link takes up to {}",
                            style.bad("[fdx]"),
                            line.len(),
                            link.max_payload()
                        );
                        continue;
                    }
                    queue.push_back(line);
                }
            }
            let step = (|| -> Result<(), Error> {
                while let Some(packet) = link.receive(memory)? {
                    match packet {
                        Packet::Data(packet) => {
                            let _ = writeln!(stdout, "{}", text(&packet));
                            let _ = stdout.flush();
                        }
                        Packet::Dropped => eprintln!(
                            "{} the target dropped packets: its ring was full",
                            style.warn("[fdx]")
                        ),
                    }
                }
                while let Some(packet) = queue.front() {
                    if !link.send(memory, packet.as_bytes())? {
                        break;
                    }
                    queue.pop_front();
                }
                Ok(())
            })();
            match step {
                Ok(()) => {
                    consecutive_errors = 0;
                    std::thread::sleep(POLL);
                }
                Err(Error::Closed) => {
                    eprintln!(
                        "{} the target closed or reopened the link",
                        style.warn("[fdx]")
                    );
                    continue 'link;
                }
                Err(Error::Memory(error)) if error.lost => {
                    return Err(format!("FDX: the connection to the probe is gone: {error}"));
                }
                Err(Error::Memory(error)) => {
                    consecutive_errors += 1;
                    if matches!(consecutive_errors, 5 | 50) || consecutive_errors % 300 == 0 {
                        eprintln!(
                            "{} memory access failed x{consecutive_errors}: {error}",
                            style.warn("[fdx]")
                        );
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(error) => return Err(format!("FDX: {error}")),
            }
        }
    }
    eprintln!("{}", style.label(format!("{probe} FDX stopped")));
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use probecore::fdx::HEADER;

    /// One open link of 64 bytes each way at 0x1000, with a target that
    /// answers every packet with "ok <packet>".
    struct Board {
        bytes: Vec<u8>,
        reads: usize,
    }

    const BASE: u64 = 0x1000;
    const N: usize = 64;

    impl Board {
        fn new() -> Board {
            let mut board = Board {
                bytes: vec![0; 2 * (HEADER + N)],
                reads: 0,
            };
            board.put_word(BASE + (HEADER + N) as u64 + 4, N as u32);
            board.put_word(BASE + 4, N as u32);
            board
        }

        fn put_word(&mut self, address: u64, value: u32) {
            let at = (address - BASE) as usize;
            self.bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }

        fn word(&self, address: u64) -> u32 {
            let at = (address - BASE) as usize;
            u32::from_le_bytes(self.bytes[at..at + 4].try_into().unwrap())
        }

        /// The target's side, the way the agent's ring logic works for a
        /// few short packets: read one from `to_target`, answer on `to_host`.
        fn target_step(&mut self) {
            let to_target = BASE;
            let to_host = BASE + (HEADER + N) as u64;
            let mut index = self.word(to_target + 12) as usize;
            let data = (to_target - BASE) as usize + HEADER;
            let mut mark = self.bytes[data + index];
            if mark == 0xFF {
                index = N - 1;
                mark = self.bytes[data + index];
            }
            if mark == 0 {
                return;
            }
            let len = mark as usize;
            let next = index - len - 1;
            let packet = self.bytes[data + next + 1..data + next + 1 + len].to_vec();
            self.put_word(to_target + 12, next as u32);
            let mut answer = b"ok ".to_vec();
            answer.extend(packet);
            let out = (to_host - BASE) as usize + HEADER;
            let at = self.word(to_host + 12) as usize;
            self.bytes[out + at + 1..out + at + 1 + answer.len()].copy_from_slice(&answer);
            self.bytes[out + at + 1 + answer.len()] = 0;
            self.bytes[out + at] = answer.len() as u8;
            self.put_word(to_host + 12, (at + 1 + answer.len()) as u32);
        }
    }

    impl Memory for Board {
        fn read(&mut self, address: u64, length: usize) -> probecore::Result<Vec<u8>> {
            // The target runs between the debugger's reads.
            self.reads += 1;
            self.target_step();
            let at = (address - BASE) as usize;
            Ok(self.bytes[at..at + length].to_vec())
        }
        fn read_u32s(&mut self, address: u64, count: usize) -> probecore::Result<Vec<u32>> {
            Ok((0..count)
                .map(|i| self.word(address + 4 * i as u64))
                .collect())
        }
        fn write(&mut self, _: u64, _: &[u8]) -> probecore::Result<()> {
            unreachable!()
        }
        fn write_u32(&mut self, address: u64, value: u32) -> probecore::Result<()> {
            self.put_word(address, value);
            Ok(())
        }
    }

    fn options(send: &[&str], expect: &[&str]) -> Options {
        Options {
            probe: "TEST",
            symbol: "RUSTVISOR_FDX".into(),
            address: BASE,
            send: send.iter().map(|s| s.to_string()).collect(),
            expect: expect.iter().map(|s| s.to_string()).collect(),
            timeout: Duration::from_millis(300),
            json: true,
        }
    }

    #[test]
    fn send_then_expect_the_answer() {
        let mut board = Board::new();
        let stop = AtomicBool::new(false);
        let code = run(
            &mut board,
            &options(&["boot single"], &["ok boot single"]),
            &stop,
        );
        assert_eq!(code, Ok(0));
    }

    #[test]
    fn a_missing_answer_exits_3() {
        let mut board = Board::new();
        let stop = AtomicBool::new(false);
        let code = run(&mut board, &options(&["boot"], &["ready"]), &stop);
        assert_eq!(code, Ok(EXIT_FAILED));
    }

    #[test]
    fn a_closed_link_times_out_and_long_packets_are_refused() {
        let mut board = Board::new();
        board.put_word(BASE + 4, 0);
        let stop = AtomicBool::new(false);
        let error = run(&mut board, &options(&["x"], &[]), &stop).unwrap_err();
        assert!(error.contains("did not open"), "{error}");
        let mut board = Board::new();
        let long = "x".repeat(40);
        let error = run(&mut board, &options(&[long.as_str()], &[]), &stop).unwrap_err();
        assert!(error.contains("1 to 29 bytes"), "{error}");
    }
}
