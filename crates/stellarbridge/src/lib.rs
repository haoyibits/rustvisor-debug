//! StellarLINK backend: starts a (patched) OpenOCD as a subprocess and
//! controls it through its Tcl RPC port with [`oocd`].
//!
//! Generic: which OpenOCD, which configuration scripts and which MEM-AP reads
//! physical memory are the caller's data; this crate knows no board.
//! OpenOCD is GPL and is only run as a separate program.
#![forbid(unsafe_code)]

use std::fs::File;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use probecore::elf::{self, Segment, SegmentResult};
use probecore::target::{self, Target, TargetAddress};

/// A Tcl word that stands for its text literally, for building commands.
pub use oocd::quote;

/// A failed backend operation, as one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

macro_rules! fail {
    ($($arg:tt)*) => { return Err(Error(format!($($arg)*))) };
}

/// How to start OpenOCD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    /// The OpenOCD program, e.g. `openocd-stellar`.
    pub executable: PathBuf,
    /// Extra script directories (`-s`), searched before the built-in ones.
    pub search: Vec<PathBuf>,
    /// Configuration files (`-f`), e.g. `board/sr6p6_evb.cfg`.
    pub config: Vec<String>,
    /// Commands (`-c`) after the configuration files and before `init`.
    pub commands: Vec<String>,
    /// The Tcl RPC port on 127.0.0.1.
    pub tcl_port: u16,
    /// The GDB server port; `None` turns the GDB server off.
    pub gdb_port: Option<u16>,
    /// The one target that gets the GDB server on `gdb_port` (e.g. the
    /// core). `None`: OpenOCD numbers a server for every target from
    /// `gdb_port` upwards, MEM-APs included.
    pub gdb_target: Option<String>,
    /// OpenOCD's stdout and stderr go here (appended); `None` leaves them
    /// on this process's terminal.
    pub log: Option<PathBuf>,
    /// How long `init` may take until the Tcl port answers.
    pub startup_timeout: Duration,
}

impl ServerConfig {
    /// The OpenOCD arguments. The ports are set first; the telnet server is
    /// off, and so is the GDB server unless `gdb_port` is given. With a
    /// `gdb_target`, only that target serves GDB: the global port stays
    /// disabled and the target's `-gdb-port` is set after the configuration
    /// files (which create it) and before `init` (which starts the servers).
    pub fn arguments(&self) -> Vec<String> {
        let mut arguments = Vec::new();
        for directory in &self.search {
            arguments.push("-s".to_string());
            arguments.push(directory.display().to_string());
        }
        arguments.push("-c".into());
        let gdb = match (self.gdb_port, &self.gdb_target) {
            (Some(port), None) => port.to_string(),
            _ => "disabled".to_string(),
        };
        arguments.push(format!(
            "tcl_port {}; gdb_port {gdb}; telnet_port disabled",
            self.tcl_port
        ));
        for file in &self.config {
            arguments.push("-f".into());
            arguments.push(file.clone());
        }
        if let (Some(port), Some(target)) = (self.gdb_port, &self.gdb_target) {
            arguments.push("-c".into());
            arguments.push(format!(
                "{} configure -gdb-port {port}",
                oocd::quote(target)
            ));
        }
        for command in &self.commands {
            arguments.push("-c".into());
            arguments.push(command.clone());
        }
        arguments
    }
}

/// A running OpenOCD and the connection to its Tcl server. Dropping it
/// shuts OpenOCD down.
pub struct Server {
    child: Child,
    client: oocd::Client,
    log: Option<PathBuf>,
}

/// Where OpenOCD's output is, and its last lines, for error messages.
fn log_note(path: Option<&Path>) -> String {
    match path {
        Some(path) => format!(
            "end of the OpenOCD log ({}):\n{}",
            path.display(),
            log_tail(path, 12)
        ),
        None => "see OpenOCD's output above".to_string(),
    }
}

fn port_free(port: u16) -> bool {
    TcpListener::bind(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))).is_ok()
}

/// The last lines of OpenOCD's log, for error messages.
pub fn log_tail(path: &Path, lines: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

impl Server {
    pub fn start(config: &ServerConfig) -> Result<Server> {
        let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, config.tcl_port));
        // Another OpenOCD on the port would answer in place of ours.
        for (name, port) in [
            ("tcl_port", Some(config.tcl_port)),
            ("gdb_port", config.gdb_port),
        ] {
            if let Some(port) = port {
                if !port_free(port) {
                    fail!(
                        "port {port} ({name}) is in use (another OpenOCD or debug session?); \
                         stop it or choose another port"
                    );
                }
            }
        }
        let (stdout, stderr) = match &config.log {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| Error(format!("cannot create {}: {e}", parent.display())))?;
                }
                let log = File::options()
                    .create(true)
                    .append(true)
                    .open(path)
                    .map_err(|e| Error(format!("cannot open {}: {e}", path.display())))?;
                let log_err = log
                    .try_clone()
                    .map_err(|e| Error(format!("cannot open {}: {e}", path.display())))?;
                (Stdio::from(log), Stdio::from(log_err))
            }
            None => (Stdio::inherit(), Stdio::inherit()),
        };
        let mut child = Command::new(&config.executable)
            .args(config.arguments())
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .map_err(|e| Error(format!("cannot start {}: {e}", config.executable.display())))?;

        let deadline = Instant::now() + config.startup_timeout;
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                fail!(
                    "OpenOCD exited during start-up ({status}); {}",
                    log_note(config.log.as_deref())
                );
            }
            if let Ok(client) = oocd::Client::connect(address, Duration::from_secs(5)) {
                return Ok(Server {
                    child,
                    client,
                    log: config.log.clone(),
                });
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                fail!(
                    "OpenOCD did not open its Tcl port {} within {:?}; {}",
                    config.tcl_port,
                    config.startup_timeout,
                    log_note(config.log.as_deref())
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn log(&self) -> Option<&Path> {
        self.log.as_deref()
    }

    /// Run a Tcl command; errors carry the end of OpenOCD's log, where the
    /// flash driver writes the reason.
    pub fn eval(&mut self, command: &str) -> Result<String> {
        self.client.eval(command).map_err(|error| {
            Error(format!(
                "{command}: {error}\n{}",
                log_note(self.log.as_deref())
            ))
        })
    }

    pub fn set_timeout(&mut self, timeout: Duration) -> Result<()> {
        self.client
            .set_timeout(timeout)
            .map_err(|error| Error(error.to_string()))
    }

    /// Wait until OpenOCD exits by itself (e.g. `shutdown` from an event
    /// handler, or Ctrl-C), without asking it to.
    pub fn wait(mut self) -> Result<std::process::ExitStatus> {
        self.child
            .wait()
            .map_err(|error| Error(format!("waiting for OpenOCD: {error}")))
    }

    /// Ask OpenOCD to exit and wait for it.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        let _ = self.client.raw("shutdown");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            self.stop();
        }
    }
}

/// Physical memory through a MEM-AP target of OpenOCD (e.g. `chip.axi`), as
/// a [`probecore::Target`]. Only memory reads are available so far.
pub struct MemoryTarget<'a> {
    pub server: &'a mut Server,
    /// The OpenOCD target that reads physical memory.
    pub memory_ap: String,
}

fn unsupported<T>(what: &str) -> target::Result<T> {
    Err(target::Error::new(format!(
        "{what} is not available through OpenOCD yet"
    )))
}

impl Target for MemoryTarget<'_> {
    fn read_u32(&mut self, address: &TargetAddress) -> target::Result<u32> {
        if !matches!(
            address.class.to_ascii_uppercase().as_str(),
            "" | "A" | "AD" | "D"
        ) {
            return Err(target::Error::new(format!(
                "access class {} is not supported by OpenOCD",
                address.class
            )));
        }
        let bytes = self.read_memory(address.value, 4)?;
        Ok(u32::from_le_bytes(bytes.try_into().expect("4 bytes")))
    }

    fn read_core_register(&mut self, _: &str) -> target::Result<u64> {
        unsupported("reading core registers")
    }

    fn read_memory(&mut self, address: u64, length: usize) -> target::Result<Vec<u8>> {
        let ap = self.memory_ap.clone();
        self.server
            .client
            .read_memory(&ap, address, length)
            .map_err(|error| target::Error {
                message: error.to_string(),
                lost: matches!(error, oocd::Error::Connect(_) | oocd::Error::Timeout),
            })
    }

    fn is_running(&mut self) -> target::Result<bool> {
        unsupported("the run state")
    }

    fn halt(&mut self) -> target::Result<()> {
        unsupported("halting")
    }

    fn symbol_range(&mut self, _: &str) -> target::Result<(u64, u64)> {
        unsupported("symbol lookup")
    }

    fn break_command(&self) -> &str {
        "rvdbg halt"
    }
}

/// A one-time-programmable flash area (e.g. SR6P6's UTEST with the boot
/// record): its bits can be programmed from 1 to 0 but never erased.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OtpRange {
    pub start: u64,
    pub size: u64,
}

impl OtpRange {
    fn end(&self) -> u64 {
        self.start.saturating_add(self.size)
    }
}

/// The part of an image segment that lies in an OTP area, compared with
/// what the board holds there before anything is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtpCheck {
    pub image: PathBuf,
    pub address: u64,
    pub size: usize,
    /// Bytes that differ from the board.
    pub differing: usize,
    /// The first difference: address, board byte, image byte.
    pub first_difference: Option<(u64, u8, u8)>,
    /// The image needs a bit that is 0 on the board to be 1, which no
    /// programming can do.
    pub impossible: bool,
}

/// Compare the parts of `images` that fall into `ranges` with the board.
pub fn check_otp(
    target: &mut dyn Target,
    images: &[(PathBuf, Vec<Segment>)],
    ranges: &[OtpRange],
) -> target::Result<Vec<OtpCheck>> {
    let mut checks = Vec::new();
    for (image, segments) in images {
        for segment in segments {
            let segment_end = segment.paddr.saturating_add(segment.data.len() as u64);
            for range in ranges {
                let start = segment.paddr.max(range.start);
                let end = segment_end.min(range.end());
                if start >= end {
                    continue;
                }
                let offset = (start - segment.paddr) as usize;
                let wanted = &segment.data[offset..offset + (end - start) as usize];
                let board = target
                    .read_memory(start, wanted.len())
                    .map_err(|error| error.context(format!("cannot read OTP at 0x{start:X}")))?;
                let mut check = OtpCheck {
                    image: image.clone(),
                    address: start,
                    size: wanted.len(),
                    differing: 0,
                    first_difference: None,
                    impossible: false,
                };
                for (index, (&have, &want)) in board.iter().zip(wanted).enumerate() {
                    if have != want {
                        check.differing += 1;
                        check
                            .first_difference
                            .get_or_insert((start + index as u64, have, want));
                        check.impossible |= want & !have != 0;
                    }
                }
                checks.push(check);
            }
        }
    }
    Ok(checks)
}

/// Why `check` stops the flashing, if it does.
pub fn otp_refusal(check: &OtpCheck, write_otp: bool) -> Option<String> {
    let (at, board, image) = check.first_difference?;
    let place = format!(
        "{}: {} of {} bytes at 0x{:08X} lie in one-time-programmable flash and differ from \
         the board (first at 0x{at:08X}: board 0x{board:02X}, image 0x{image:02X})",
        check.image.display(),
        check.differing,
        check.size,
        check.address
    );
    if check.impossible {
        Some(format!(
            "{place}; the image needs bits that are 0 on the board to be 1, which cannot be \
             programmed. Nothing was written."
        ))
    } else if !write_otp {
        Some(format!(
            "{place}; writing them would change the board permanently. Nothing was written; \
             allow it only on purpose (rvdbg flash --write-otp)."
        ))
    } else {
        None
    }
}

/// What `flash` does besides writing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashOptions {
    /// The OpenOCD MEM-AP target that reads physical memory (e.g.
    /// `chip.axi`); needed to compare and to check OTP areas.
    pub memory_ap: Option<String>,
    /// Compare every loadable segment with target memory afterwards.
    pub verify: bool,
    /// One-time-programmable areas: the image's bytes there are compared
    /// with the board before anything is written.
    pub otp: Vec<OtpRange>,
    /// Write OTP bytes that differ from the board (when they can be
    /// programmed at all).
    pub write_otp: bool,
    /// `reset run` at the end, so the new program starts.
    pub run: bool,
    /// How long one image may take to erase and write.
    pub timeout: Duration,
}

/// The images that were written and their comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashReport {
    pub images: Vec<PathBuf>,
    /// The OTP parts of the images, checked before writing.
    pub otp: Vec<OtpCheck>,
    /// Per image, when verified.
    pub verified: Vec<(PathBuf, Vec<SegmentResult>)>,
}

impl FlashReport {
    pub fn differing(&self) -> usize {
        self.verified
            .iter()
            .flat_map(|(_, segments)| segments)
            .map(|segment| segment.differing)
            .sum()
    }
}

/// Write `images` to flash: `reset init` (the target configuration's
/// preparation, e.g. the watchdog), `flash write_image erase` for each ELF
/// (load addresses, `p_paddr`), then the comparison, then `reset run`.
///
/// Before anything is written, the parts of the images in OTP areas are
/// compared with the board: a difference stops the flashing unless
/// `write_otp` allows it, and a difference that would need a 0 bit to
/// become 1 always does.
///
/// The images are compared only after all of them are written, so an image
/// that erased a sector of an earlier one shows up as a difference.
/// `progress` receives one line per step.
pub fn flash(
    server: &mut Server,
    images: &[PathBuf],
    options: &FlashOptions,
    progress: &mut dyn FnMut(&str),
) -> Result<FlashReport> {
    if images.is_empty() {
        fail!("no image to flash");
    }
    let mut segments = Vec::new();
    for image in images {
        let bytes = std::fs::read(image)
            .map_err(|e| Error(format!("cannot read {}: {e}", image.display())))?;
        let loadable =
            elf::load_segments(&bytes).map_err(|e| Error(format!("{}: {e}", image.display())))?;
        if loadable.is_empty() {
            fail!("{} has no loadable segment", image.display());
        }
        segments.push(loadable);
    }
    let needs_ap = options.verify || !options.otp.is_empty();
    let memory_ap = match &options.memory_ap {
        Some(ap) => ap.clone(),
        None if needs_ap => fail!(
            "comparing with flash and checking OTP areas need the MEM-AP target that reads \
             physical memory (memory_ap)"
        ),
        None => String::new(),
    };

    let mut otp = Vec::new();
    if !options.otp.is_empty() {
        progress("check OTP areas");
        let named: Vec<(PathBuf, Vec<Segment>)> = images
            .iter()
            .cloned()
            .zip(segments.iter().cloned())
            .collect();
        let mut target = MemoryTarget {
            server,
            memory_ap: memory_ap.clone(),
        };
        otp = check_otp(&mut target, &named, &options.otp).map_err(|error| Error(error.message))?;
        if let Some(refusal) = otp
            .iter()
            .find_map(|check| otp_refusal(check, options.write_otp))
        {
            return Err(Error(refusal));
        }
    }

    server.set_timeout(Duration::from_secs(60))?;
    progress("reset init");
    server.eval("reset init")?;
    server.set_timeout(options.timeout)?;
    for image in images {
        progress(&format!("write {}", image.display()));
        server.eval(&format!(
            "flash write_image erase {} 0 elf",
            oocd::quote(&image.display().to_string())
        ))?;
    }

    let mut verified = Vec::new();
    if options.verify {
        let mut target = MemoryTarget {
            server,
            memory_ap: memory_ap.clone(),
        };
        for (image, loadable) in images.iter().zip(&segments) {
            progress(&format!("compare {}", image.display()));
            let results =
                elf::compare(&mut target, loadable).map_err(|error| Error(error.message))?;
            verified.push((image.clone(), results));
        }
    }

    if options.run {
        server.set_timeout(Duration::from_secs(60))?;
        progress("reset run");
        server.eval("reset run")?;
    }
    Ok(FlashReport {
        images: images.to_vec(),
        otp,
        verified,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Board memory for the OTP check; everything else is unused.
    struct Board(Vec<(u64, Vec<u8>)>, Vec<String>);

    impl Target for Board {
        fn read_u32(&mut self, _: &TargetAddress) -> target::Result<u32> {
            unreachable!()
        }
        fn read_core_register(&mut self, _: &str) -> target::Result<u64> {
            unreachable!()
        }
        fn read_memory(&mut self, address: u64, length: usize) -> target::Result<Vec<u8>> {
            self.1.push(format!("read 0x{address:X} {length}"));
            let (base, bytes) = self
                .0
                .iter()
                .find(|(base, bytes)| *base <= address && address < base + bytes.len() as u64)
                .ok_or_else(|| target::Error::new("bus error"))?;
            let offset = (address - base) as usize;
            Ok(bytes[offset..offset + length].to_vec())
        }
        fn is_running(&mut self) -> target::Result<bool> {
            unreachable!()
        }
        fn halt(&mut self) -> target::Result<()> {
            unreachable!()
        }
        fn symbol_range(&mut self, _: &str) -> target::Result<(u64, u64)> {
            unreachable!()
        }
        fn break_command(&self) -> &str {
            "-"
        }
    }

    const UTEST: OtpRange = OtpRange {
        start: 0x29F8_7000,
        size: 0x1000,
    };

    fn image(segments: &[(u64, &[u8])]) -> Vec<(PathBuf, Vec<Segment>)> {
        vec![(
            PathBuf::from("app.elf"),
            segments
                .iter()
                .map(|(paddr, data)| Segment {
                    paddr: *paddr,
                    vaddr: *paddr,
                    data: data.to_vec(),
                })
                .collect(),
        )]
    }

    fn board(record: &[u8]) -> Board {
        let mut utest = vec![0xFF; 0x1000];
        utest[0xE00..0xE00 + record.len()].copy_from_slice(record);
        Board(vec![(0x29F8_7000, utest)], Vec::new())
    }

    #[test]
    fn only_the_otp_part_of_a_segment_is_read() {
        let mut board = board(&[1, 0, 0x5A, 0xA5]);
        // Straddles the end of the area: only 4 bytes are inside.
        let images = image(&[(0x2800_0000, b"CODE"), (0x29F8_7FFC, &[0xFF; 8])]);
        let checks = check_otp(&mut board, &images, &[UTEST]).unwrap();
        assert_eq!(board.1, ["read 0x29F87FFC 4"]);
        assert_eq!(checks.len(), 1);
        assert_eq!((checks[0].size, checks[0].differing), (4, 0));
    }

    #[test]
    fn the_same_record_passes() {
        let mut board = board(&[1, 0, 0x5A, 0xA5]);
        let images = image(&[(0x29F8_7E00, &[1, 0, 0x5A, 0xA5])]);
        let checks = check_otp(&mut board, &images, &[UTEST]).unwrap();
        assert_eq!(checks[0].differing, 0);
        assert_eq!(otp_refusal(&checks[0], false), None);
    }

    #[test]
    fn a_different_record_is_refused_unless_allowed() {
        // Blank board: programming clears bits only, so it is possible.
        let mut board = board(&[0xFF, 0xFF, 0xFF, 0xFF]);
        let images = image(&[(0x29F8_7E00, &[1, 0, 0x5A, 0xA5])]);
        let checks = check_otp(&mut board, &images, &[UTEST]).unwrap();
        assert!(!checks[0].impossible);
        assert_eq!(checks[0].first_difference, Some((0x29F8_7E00, 0xFF, 0x01)));
        let refusal = otp_refusal(&checks[0], false).unwrap();
        assert!(
            refusal.starts_with(
                "app.elf: 4 of 4 bytes at 0x29F87E00 lie in one-time-programmable flash"
            ),
            "{refusal}"
        );
        assert!(refusal.contains("--write-otp"), "{refusal}");
        assert_eq!(otp_refusal(&checks[0], true), None);
    }

    #[test]
    fn a_record_that_needs_set_bits_is_always_refused() {
        let mut board = board(&[1, 0, 0x5A, 0xA5]);
        let images = image(&[(0x29F8_7E00, &[1, 0, 0x5A, 0xA7])]);
        let checks = check_otp(&mut board, &images, &[UTEST]).unwrap();
        assert!(checks[0].impossible);
        let refusal = otp_refusal(&checks[0], true).unwrap();
        assert!(refusal.contains("cannot be programmed"), "{refusal}");
    }

    #[test]
    fn arguments_put_the_ports_first_and_keep_the_order() {
        let config = ServerConfig {
            executable: "openocd-stellar".into(),
            search: vec!["/scripts".into()],
            config: vec!["board/a.cfg".into(), "extra.cfg".into()],
            commands: vec!["adapter speed 4000".into()],
            tcl_port: 6666,
            gdb_port: None,
            gdb_target: None,
            log: Some("/tmp/x.log".into()),
            startup_timeout: Duration::from_secs(1),
        };
        assert_eq!(
            config.arguments(),
            [
                "-s",
                "/scripts",
                "-c",
                "tcl_port 6666; gdb_port disabled; telnet_port disabled",
                "-f",
                "board/a.cfg",
                "-f",
                "extra.cfg",
                "-c",
                "adapter speed 4000",
            ]
        );
        let with_gdb = ServerConfig {
            gdb_port: Some(3333),
            ..config
        };
        assert_eq!(
            with_gdb.arguments()[3],
            "tcl_port 6666; gdb_port 3333; telnet_port disabled"
        );
        let one_target = ServerConfig {
            gdb_target: Some("chip.core".into()),
            ..with_gdb
        };
        let arguments = one_target.arguments();
        assert_eq!(
            arguments[3],
            "tcl_port 6666; gdb_port disabled; telnet_port disabled"
        );
        assert_eq!(
            arguments[8..],
            [
                "-c",
                "chip.core configure -gdb-port 3333",
                "-c",
                "adapter speed 4000"
            ]
        );
    }

    fn config(dir: &Path, executable: &str, port: u16) -> ServerConfig {
        ServerConfig {
            executable: executable.into(),
            search: Vec::new(),
            config: Vec::new(),
            commands: Vec::new(),
            tcl_port: port,
            gdb_port: None,
            gdb_target: None,
            log: Some(dir.join("openocd.log")),
            startup_timeout: Duration::from_secs(2),
        }
    }

    #[test]
    fn a_missing_program_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let error = Server::start(&config(dir.path(), "/nonexistent/openocd", port))
            .err()
            .unwrap();
        assert!(
            error.0.starts_with("cannot start /nonexistent/openocd"),
            "{error}"
        );
    }

    #[test]
    fn a_busy_port_is_refused_before_starting() {
        let dir = tempfile::tempdir().unwrap();
        let busy = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = busy.local_addr().unwrap().port();
        let error = Server::start(&config(dir.path(), "/nonexistent/openocd", port))
            .err()
            .unwrap();
        assert!(error.0.contains("is in use"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn an_early_exit_shows_the_log() {
        // /bin/sh takes `-c "tcl_port ..."` as a script and fails on it at
        // once; no executable is written, so no other test's fork can hold
        // it open (ETXTBSY).
        let dir = tempfile::tempdir().unwrap();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let error = Server::start(&config(dir.path(), "/bin/sh", port))
            .err()
            .unwrap();
        assert!(error.0.contains("exited during start-up"), "{error}");
        assert!(error.0.contains("tcl_port"), "{error}");
    }
}
