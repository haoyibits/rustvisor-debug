//! Client for the OpenOCD Tcl RPC server (`tcl_port`).
//!
//! The protocol is plain text over TCP: a command, then the byte `0x1a`; the
//! answer is the command's Tcl result, then `0x1a`. OpenOCD's log does not go
//! through this connection. A failing command still answers with its error
//! message as the result, so [`Client::eval`] wraps every command in `catch`
//! and reports the return code.
//!
//! OpenOCD runs as a separate process; this crate only talks to it over TCP
//! and does not link or embed any OpenOCD code.
#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// Ends every command and every answer.
pub const TERMINATOR: u8 = 0x1a;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The connection could not be made, broke or was closed.
    Connect(String),
    /// No complete answer within the read timeout; the connection is out of
    /// step afterwards and is closed.
    Timeout,
    /// The command failed in OpenOCD; the Tcl error message.
    Command(String),
    /// An answer this client cannot use.
    Protocol(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Connect(message) | Error::Command(message) | Error::Protocol(message) => {
                f.write_str(message)
            }
            Error::Timeout => f.write_str("OpenOCD did not answer in time"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// A Tcl word that stands for `text` literally: every character that Tcl
/// would interpret is escaped with a backslash, so file names with spaces,
/// brackets or braces stay one word.
pub fn quote(text: &str) -> String {
    if text.is_empty() {
        return "{}".to_string();
    }
    let mut quoted = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' | '.' | '/' | ':' | ',' | '+' | '=' => {
                quoted.push(c)
            }
            '\n' => quoted.push_str("\\n"),
            '\t' => quoted.push_str("\\t"),
            '\r' => quoted.push_str("\\r"),
            _ => {
                quoted.push('\\');
                quoted.push(c);
            }
        }
    }
    quoted
}

/// One connection to OpenOCD's Tcl server.
pub struct Client {
    stream: Option<TcpStream>,
    pending: Vec<u8>,
}

impl Client {
    pub fn connect(address: SocketAddr, timeout: Duration) -> Result<Client> {
        let stream = TcpStream::connect_timeout(&address, timeout)
            .map_err(|error| Error::Connect(format!("cannot connect to {address}: {error}")))?;
        stream
            .set_read_timeout(Some(timeout))
            .and_then(|()| stream.set_nodelay(true))
            .map_err(|error| Error::Connect(error.to_string()))?;
        Ok(Client {
            stream: Some(stream),
            pending: Vec::new(),
        })
    }

    /// How long a command may take, e.g. a whole flash write.
    pub fn set_timeout(&mut self, timeout: Duration) -> Result<()> {
        self.stream()?
            .set_read_timeout(Some(timeout))
            .map_err(|error| Error::Connect(error.to_string()))
    }

    fn stream(&mut self) -> Result<&mut TcpStream> {
        self.stream
            .as_mut()
            .ok_or_else(|| Error::Connect("the connection to OpenOCD is closed".into()))
    }

    /// Send a command and return OpenOCD's answer as it is. A failing
    /// command answers with its error message, indistinguishable from a
    /// result; use [`Client::eval`].
    pub fn raw(&mut self, command: &str) -> Result<String> {
        if command.as_bytes().contains(&TERMINATOR) {
            return Err(Error::Protocol("a command must not contain 0x1a".into()));
        }
        let mut message = command.as_bytes().to_vec();
        message.push(TERMINATOR);
        let result = self.exchange(&message);
        if matches!(result, Err(Error::Timeout | Error::Connect(_))) {
            // An answer may still arrive and would be read as the next one.
            self.stream = None;
        }
        result
    }

    fn exchange(&mut self, message: &[u8]) -> Result<String> {
        self.stream()?
            .write_all(message)
            .map_err(|error| Error::Connect(format!("cannot send to OpenOCD: {error}")))?;
        loop {
            if let Some(end) = self.pending.iter().position(|&b| b == TERMINATOR) {
                let answer: Vec<u8> = self.pending.drain(..=end).take(end).collect();
                return String::from_utf8(answer)
                    .map_err(|_| Error::Protocol("OpenOCD answered with invalid UTF-8".into()));
            }
            let mut chunk = [0u8; 4096];
            let read = self.stream()?.read(&mut chunk).map_err(|error| {
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) {
                    Error::Timeout
                } else {
                    Error::Connect(format!("connection to OpenOCD failed: {error}"))
                }
            })?;
            if read == 0 {
                return Err(Error::Connect("OpenOCD closed the connection".into()));
            }
            self.pending.extend_from_slice(&chunk[..read]);
        }
    }

    /// Run a command; a Tcl error becomes [`Error::Command`] with its message.
    pub fn eval(&mut self, command: &str) -> Result<String> {
        let wrapped = format!(
            "set _oocd_rc [catch {{{command}}} _oocd_out]; format {{%d %s}} $_oocd_rc $_oocd_out"
        );
        let answer = self.raw(&wrapped)?;
        let (code, text) = answer.split_once(' ').unwrap_or((answer.as_str(), ""));
        match code {
            "0" | "2" => Ok(text.to_string()),
            "1" => Err(Error::Command(text.trim_end().to_string())),
            _ => Err(Error::Protocol(format!(
                "unexpected answer from OpenOCD: {answer:?}"
            ))),
        }
    }

    /// `length` bytes at `address` through the memory access of `target`
    /// (a core or a MEM-AP target such as `chip.axi`).
    pub fn read_memory(&mut self, target: &str, address: u64, length: usize) -> Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(length);
        // A few kilobytes per command keep each answer small.
        const CHUNK: usize = 4096;
        while bytes.len() < length {
            let count = CHUNK.min(length - bytes.len());
            let at = address + bytes.len() as u64;
            let answer = self.eval(&format!("{} read_memory 0x{at:X} 8 {count}", quote(target)))?;
            let values = parse_numbers(&answer)?;
            if values.len() != count {
                return Err(Error::Protocol(format!(
                    "read_memory at 0x{at:X} returned {} bytes, expected {count}",
                    values.len()
                )));
            }
            for value in values {
                let byte = u8::try_from(value).map_err(|_| {
                    Error::Protocol(format!("read_memory returned {value:#x} for a byte"))
                })?;
                bytes.push(byte);
            }
        }
        Ok(bytes)
    }
}

/// The numbers of a Tcl list such as `0x1 0x0 0x5a`.
pub fn parse_numbers(text: &str) -> Result<Vec<u64>> {
    text.split_whitespace()
        .map(|word| {
            let parsed = match word.strip_prefix("0x").or_else(|| word.strip_prefix("0X")) {
                Some(hex) => u64::from_str_radix(hex, 16),
                None => word.parse(),
            };
            parsed.map_err(|_| Error::Protocol(format!("{word:?} is not a number")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    /// A Tcl server that answers each command with the next scripted answer
    /// and records what it received.
    fn server(answers: Vec<&'static str>) -> (SocketAddr, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            let mut pending = Vec::new();
            for answer in answers {
                let command = loop {
                    if let Some(end) = pending.iter().position(|&b| b == TERMINATOR) {
                        let command: Vec<u8> = pending.drain(..=end).take(end).collect();
                        break String::from_utf8(command).unwrap();
                    }
                    let mut chunk = [0u8; 1024];
                    let read = stream.read(&mut chunk).unwrap();
                    if read == 0 {
                        return received;
                    }
                    pending.extend_from_slice(&chunk[..read]);
                };
                received.push(command);
                // Split the answer to exercise reassembly.
                let mut bytes = answer.as_bytes().to_vec();
                bytes.push(TERMINATOR);
                let (first, second) = bytes.split_at(bytes.len() / 2);
                stream.write_all(first).unwrap();
                stream.flush().unwrap();
                stream.write_all(second).unwrap();
            }
            received
        });
        (address, handle)
    }

    fn client(address: SocketAddr) -> Client {
        Client::connect(address, Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn eval_reports_results_and_tcl_errors() {
        let (address, handle) = server(vec!["0 halted", "1 invalid command name \"nope\"\n"]);
        let mut client = client(address);
        assert_eq!(client.eval("sr6p6.k0c0.core curstate").unwrap(), "halted");
        assert_eq!(
            client.eval("nope").unwrap_err(),
            Error::Command("invalid command name \"nope\"".into())
        );
        drop(client);
        let received = handle.join().unwrap();
        assert_eq!(
            received[0],
            "set _oocd_rc [catch {sr6p6.k0c0.core curstate} _oocd_out]; \
             format {%d %s} $_oocd_rc $_oocd_out"
        );
    }

    #[test]
    fn read_memory_parses_the_byte_list() {
        let (address, handle) = server(vec!["0 0x1 0x0 0x5a 0xa5"]);
        let mut client = client(address);
        assert_eq!(
            client.read_memory("chip.axi", 0x29F8_7E00, 4).unwrap(),
            [0x01, 0x00, 0x5A, 0xA5]
        );
        drop(client);
        let received = handle.join().unwrap();
        assert!(received[0].contains("{chip.axi read_memory 0x29F87E00 8 4}"));
    }

    #[test]
    fn short_or_bad_memory_answers_are_protocol_errors() {
        let (address, _handle) = server(vec!["0 0x1 0x2", "0 0x1 0x100"]);
        let mut client = client(address);
        assert!(matches!(
            client.read_memory("t", 0, 3),
            Err(Error::Protocol(_))
        ));
        assert!(matches!(
            client.read_memory("t", 0, 2),
            Err(Error::Protocol(_))
        ));
    }

    #[test]
    fn closed_connection_and_timeout() {
        let (address, handle) = server(vec![]);
        let mut client = client(address);
        handle.join().unwrap();
        assert!(matches!(client.raw("version"), Err(Error::Connect(_))));
        // The connection is dropped after a failure.
        assert!(matches!(client.raw("version"), Err(Error::Connect(_))));

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client =
            Client::connect(listener.local_addr().unwrap(), Duration::from_millis(100)).unwrap();
        let _accepted = listener.accept().unwrap();
        assert_eq!(client.raw("version"), Err(Error::Timeout));
    }

    #[test]
    fn quoting_keeps_one_literal_word() {
        assert_eq!(quote("/tmp/a.elf"), "/tmp/a.elf");
        assert_eq!(
            quote("/my dir/[x]{y}$z\"w"),
            "/my\\ dir/\\[x\\]\\{y\\}\\$z\\\"w"
        );
        assert_eq!(quote(""), "{}");
        assert_eq!(quote("a\\b;c"), "a\\\\b\\;c");
    }

    #[test]
    fn commands_with_the_terminator_are_refused() {
        let (address, _handle) = server(vec![]);
        let mut client = client(address);
        assert!(matches!(client.raw("a\u{1a}b"), Err(Error::Protocol(_))));
    }
}
