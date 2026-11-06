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

use probecore::elf::{self, SegmentResult};
use probecore::target::{self, Target, TargetAddress};

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
    /// OpenOCD's stdout and stderr go here (appended).
    pub log: PathBuf,
    /// How long `init` may take until the Tcl port answers.
    pub startup_timeout: Duration,
}

impl ServerConfig {
    /// The OpenOCD arguments. The Tcl port is set first; the GDB and telnet
    /// servers are off, so a second instance does not collide on them.
    pub fn arguments(&self) -> Vec<String> {
        let mut arguments = Vec::new();
        for directory in &self.search {
            arguments.push("-s".to_string());
            arguments.push(directory.display().to_string());
        }
        arguments.push("-c".into());
        arguments.push(format!(
            "tcl_port {}; gdb_port disabled; telnet_port disabled",
            self.tcl_port
        ));
        for file in &self.config {
            arguments.push("-f".into());
            arguments.push(file.clone());
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
    log: PathBuf,
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
        if TcpListener::bind(address).is_err() {
            fail!(
                "port {} is in use (another OpenOCD?); stop it or choose another tcl_port",
                config.tcl_port
            );
        }
        if let Some(parent) = config.log.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error(format!("cannot create {}: {e}", parent.display())))?;
        }
        let log = File::options()
            .create(true)
            .append(true)
            .open(&config.log)
            .map_err(|e| Error(format!("cannot open {}: {e}", config.log.display())))?;
        let log_err = log
            .try_clone()
            .map_err(|e| Error(format!("cannot open {}: {e}", config.log.display())))?;
        let mut child = Command::new(&config.executable)
            .args(config.arguments())
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(log_err)
            .spawn()
            .map_err(|e| Error(format!("cannot start {}: {e}", config.executable.display())))?;

        let deadline = Instant::now() + config.startup_timeout;
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                fail!(
                    "OpenOCD exited during start-up ({status}); its log {}:\n{}",
                    config.log.display(),
                    log_tail(&config.log, 15)
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
                    "OpenOCD did not open its Tcl port {} within {:?}; its log {}:\n{}",
                    config.tcl_port,
                    config.startup_timeout,
                    config.log.display(),
                    log_tail(&config.log, 15)
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn log(&self) -> &Path {
        &self.log
    }

    /// Run a Tcl command; errors carry the end of OpenOCD's log, where the
    /// flash driver writes the reason.
    pub fn eval(&mut self, command: &str) -> Result<String> {
        self.client.eval(command).map_err(|error| {
            Error(format!(
                "{command}: {error}\nend of the OpenOCD log ({}):\n{}",
                self.log.display(),
                log_tail(&self.log, 10)
            ))
        })
    }

    pub fn set_timeout(&mut self, timeout: Duration) -> Result<()> {
        self.client
            .set_timeout(timeout)
            .map_err(|error| Error(error.to_string()))
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

/// What `flash` does besides writing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashOptions {
    /// Compare every loadable segment with target memory afterwards,
    /// through this MEM-AP target. `None` skips the comparison.
    pub verify_with: Option<String>,
    /// `reset run` at the end, so the new program starts.
    pub run: bool,
    /// How long one image may take to erase and write.
    pub timeout: Duration,
}

/// The images that were written and their comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashReport {
    pub images: Vec<PathBuf>,
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
    if let Some(memory_ap) = &options.verify_with {
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
        verified,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_put_the_ports_first_and_keep_the_order() {
        let config = ServerConfig {
            executable: "openocd-stellar".into(),
            search: vec!["/scripts".into()],
            config: vec!["board/a.cfg".into(), "extra.cfg".into()],
            commands: vec!["adapter speed 4000".into()],
            tcl_port: 6666,
            log: "/tmp/x.log".into(),
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
    }

    fn config(dir: &Path, executable: &str, port: u16) -> ServerConfig {
        ServerConfig {
            executable: executable.into(),
            search: Vec::new(),
            config: Vec::new(),
            commands: Vec::new(),
            tcl_port: port,
            log: dir.join("openocd.log"),
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
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-openocd");
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'Error: no device found' >&2\nexit 1\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&script, permissions).unwrap();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let error = Server::start(&config(dir.path(), script.to_str().unwrap(), port))
            .err()
            .unwrap();
        assert!(error.0.contains("exited during start-up"), "{error}");
        assert!(error.0.contains("Error: no device found"), "{error}");
    }
}
