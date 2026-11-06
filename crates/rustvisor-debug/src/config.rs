//! `rvdbg.toml`: the whole project configuration for both backends.
//!
//! ```toml
//! [project]
//! # backend = "openocd"                # fixes the backend; default: the connected probe
//! program = "boot_diagnostic"          # TRACE32 symbol program; default: the ELF's name
//! elf = "target/thumbv8r-none-eabihf/release/boot_diagnostic"
//! extra_elf = ["target/thumbv8r-none-eabihf/release/scenario-guest"]
//!
//! [clock]                              # board frequencies no register holds
//! XOSC = "40MHz"
//!
//! [rtt]                                # symbol, control_block_address, poll_interval
//!
//! [trace32]                            # PowerView: the keys of trace32.toml's [trace32]
//! rcl_port = 20000
//! [trace32.target]                     # trace32.toml's [target]
//! cpu = "SR6P6"
//! [trace32.flash]                      # trace32.toml's [flash]
//! [trace32.rtos]                       # trace32.toml's [rtos]
//!
//! [openocd]
//! executable = "openocd-stellar"
//! config = ["board/sr6p6_evb.cfg"]     # -f, in this order
//! search = []                          # -s, extra script directories
//! commands = []                        # -c, after the config files
//! tcl_port = 6666
//! gdb_port = 3333
//! gdb_target = "sr6p6.k0c0.core"       # the target that serves GDB
//! gdb = "arm-none-eabi-gdb"            # the GDB the IDE runs
//! memory_ap = "sr6p6.axi"              # MEM-AP that reads physical memory
//! timeout = 600                        # seconds for one image
//! otp = [{ start = 0x29F87000, size = 0x1000 }]
//! ```
//!
//! Relative paths are relative to the directory of the file. The TRACE32
//! backend is tracebridge: rvdbg hands it the `[project]`, `[clock]`,
//! `[rtt]` and `[trace32...]` tables in trace32.toml's shape, so the keys,
//! defaults and environment overrides are tracebridge's.

use std::path::{Path, PathBuf};

use toml::{Table, Value};

pub const FILE_NAME: &str = "rvdbg.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Trace32,
    OpenOcd,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Backend::Trace32 => "trace32",
            Backend::OpenOcd => "openocd",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenOcd {
    pub executable: PathBuf,
    pub config: Vec<String>,
    pub search: Vec<PathBuf>,
    pub commands: Vec<String>,
    pub tcl_port: u16,
    pub gdb_port: u16,
    /// The OpenOCD target that serves GDB on `gdb_port`; `None`: every
    /// target gets a port, numbered from `gdb_port` in creation order.
    pub gdb_target: Option<String>,
    pub gdb: String,
    pub memory_ap: Option<String>,
    pub timeout: u64,
    /// `(start, size)` of one-time-programmable areas.
    pub otp: Vec<(u64, u64)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub file: PathBuf,
    pub root: PathBuf,
    /// `project.backend`: a fixed choice; `None` means detect the probe.
    pub backend: Option<Backend>,
    pub program: String,
    pub elf: PathBuf,
    pub extra_elf: Vec<PathBuf>,
    pub openocd: Option<OpenOcd>,
    /// The tables for tracebridge, in trace32.toml's shape.
    pub trace32: Table,
}

impl Config {
    /// `<project>/.rvdbg`, for logs.
    pub fn run_dir(&self) -> PathBuf {
        self.root.join(".rvdbg")
    }

    /// The ELF and the extra images, in flashing order.
    pub fn images(&self) -> Vec<PathBuf> {
        std::iter::once(self.elf.clone())
            .chain(self.extra_elf.iter().cloned())
            .collect()
    }

    pub fn openocd(&self) -> Result<&OpenOcd, String> {
        self.openocd
            .as_ref()
            .ok_or_else(|| format!("{} has no [openocd] section", self.file.display()))
    }
}

/// `--config`, or the nearest `rvdbg.toml` in `cwd` or a parent.
pub fn find(explicit: Option<&Path>, cwd: &Path) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return Ok(cwd.join(path));
    }
    let mut directory = Some(cwd);
    while let Some(dir) = directory {
        let candidate = dir.join(FILE_NAME);
        if candidate.is_file() {
            return Ok(candidate);
        }
        directory = dir.parent();
    }
    Err(format!(
        "no {FILE_NAME} found in {} or any parent directory; run 'rvdbg init' or pass --config",
        cwd.display()
    ))
}

pub fn load(path: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let root = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    parse(&text, path, &root).map_err(|error| format!("{}: {error}", path.display()))
}

fn table<'a>(document: &'a Table, key: &str) -> Result<Option<&'a Table>, String> {
    match document.get(key) {
        None => Ok(None),
        Some(Value::Table(table)) => Ok(Some(table)),
        Some(_) => Err(format!("[{key}] must be a table")),
    }
}

fn string(table: &Table, section: &str, key: &str) -> Result<Option<String>, String> {
    match table.get(key) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(format!("{section}.{key} must be a string")),
    }
}

fn strings(table: &Table, section: &str, key: &str) -> Result<Vec<String>, String> {
    match table.get(key) {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => Ok(text.clone()),
                _ => Err(format!("{section}.{key} must be an array of strings")),
            })
            .collect(),
        Some(_) => Err(format!("{section}.{key} must be an array of strings")),
    }
}

fn integer(table: &Table, section: &str, key: &str) -> Result<Option<i64>, String> {
    match table.get(key) {
        None => Ok(None),
        Some(Value::Integer(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("{section}.{key} must be an integer")),
    }
}

fn check_keys(table: &Table, section: &str, known: &[&str]) -> Result<(), String> {
    for key in table.keys() {
        if !known.contains(&key.as_str()) {
            return Err(format!(
                "unknown key {section}.{key} (expected {})",
                known.join(", ")
            ));
        }
    }
    Ok(())
}

fn port(table: &Table, section: &str, key: &str, default: u16) -> Result<u16, String> {
    let value = integer(table, section, key)?.unwrap_or(i64::from(default));
    u16::try_from(value)
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| format!("{section}.{key} must be a port number"))
}

/// The keys tracebridge reads, per trace32.toml section.
pub const TRACE32_KEYS: &[&str] = &[
    "sys",
    "host",
    "executable",
    "binary",
    "config",
    "debug_adapter",
    "rcl_port",
    "dap_port",
    "dap_backend_port",
    "dap_backend_timeout",
    "operation_timeout",
];
pub const TARGET_KEYS: &[&str] = &["cpu", "cores", "mem_access", "jtag_clock", "dual_port"];
pub const FLASH_KEYS: &[&str] = &["chip", "script", "args"];
pub const RTOS_KEYS: &[&str] = &["config", "menu", "show_tasks"];
pub const RTT_KEYS: &[&str] = &["symbol", "control_block_address", "poll_interval"];
pub const OPENOCD_KEYS: &[&str] = &[
    "executable",
    "config",
    "search",
    "commands",
    "tcl_port",
    "gdb_port",
    "gdb_target",
    "gdb",
    "memory_ap",
    "timeout",
    "otp",
];

/// `otp = [{ start = 0x..., size = 0x... }, ...]`.
fn otp_ranges(section: &Table) -> Result<Vec<(u64, u64)>, String> {
    let shape =
        || "openocd.otp must be an array of { start = <address>, size = <bytes> }".to_string();
    let items = match section.get("otp") {
        None => return Ok(Vec::new()),
        Some(Value::Array(items)) => items,
        Some(_) => return Err(shape()),
    };
    items
        .iter()
        .map(|item| {
            let Value::Table(range) = item else {
                return Err(shape());
            };
            if range.len() != 2 {
                return Err(shape());
            }
            let number = |key: &str| match range.get(key) {
                Some(Value::Integer(value)) => u64::try_from(*value).ok(),
                _ => None,
            };
            match (number("start"), number("size")) {
                (Some(start), Some(size)) if size > 0 => Ok((start, size)),
                _ => Err(shape()),
            }
        })
        .collect()
}

fn parse_openocd(section: &Table, root: &Path) -> Result<OpenOcd, String> {
    check_keys(section, "openocd", OPENOCD_KEYS)?;
    let timeout = integer(section, "openocd", "timeout")?.unwrap_or(600);
    let timeout = u64::try_from(timeout)
        .ok()
        .filter(|seconds| *seconds > 0)
        .ok_or("openocd.timeout must be a positive number of seconds")?;
    let config = strings(section, "openocd", "config")?;
    if config.is_empty() {
        return Err("openocd.config must name at least one configuration file".into());
    }
    let tcl_port = port(section, "openocd", "tcl_port", 6666)?;
    let gdb_port = port(section, "openocd", "gdb_port", 3333)?;
    if tcl_port == gdb_port {
        return Err("openocd.tcl_port and openocd.gdb_port must be different".into());
    }
    Ok(OpenOcd {
        executable: PathBuf::from(
            string(section, "openocd", "executable")?.unwrap_or_else(|| "openocd-stellar".into()),
        ),
        config,
        search: strings(section, "openocd", "search")?
            .iter()
            .map(|text| root.join(text))
            .collect(),
        commands: strings(section, "openocd", "commands")?,
        tcl_port,
        gdb_port,
        gdb_target: string(section, "openocd", "gdb_target")?.filter(|name| !name.is_empty()),
        gdb: string(section, "openocd", "gdb")?
            .filter(|gdb| !gdb.is_empty())
            .unwrap_or_else(|| "arm-none-eabi-gdb".into()),
        memory_ap: string(section, "openocd", "memory_ap")?.filter(|ap| !ap.is_empty()),
        timeout,
        otp: otp_ranges(section)?,
    })
}

/// The document tracebridge reads, from rvdbg.toml's tables.
fn trace32_document(document: &Table, project: &Table, program: &str) -> Result<Table, String> {
    let mut shaped = Table::new();
    let mut tracebridge_project = Table::new();
    tracebridge_project.insert("program".into(), Value::String(program.to_string()));
    for key in ["elf", "extra_elf"] {
        if let Some(value) = project.get(key) {
            tracebridge_project.insert(key.into(), value.clone());
        }
    }
    shaped.insert("project".into(), Value::Table(tracebridge_project));
    if let Some(rtt) = table(document, "rtt")? {
        check_keys(rtt, "rtt", RTT_KEYS)?;
        shaped.insert("rtt".into(), Value::Table(rtt.clone()));
    }
    if let Some(clock) = table(document, "clock")? {
        shaped.insert("clock".into(), Value::Table(clock.clone()));
    }
    let mut powerview = Table::new();
    if let Some(trace32) = table(document, "trace32")? {
        for (key, value) in trace32 {
            let sub = |name: &str, keys: &[&str]| -> Result<Value, String> {
                let Value::Table(sub) = value else {
                    return Err(format!("[trace32.{name}] must be a table"));
                };
                check_keys(sub, &format!("trace32.{name}"), keys)?;
                Ok(value.clone())
            };
            match key.as_str() {
                "target" => {
                    shaped.insert("target".into(), sub("target", TARGET_KEYS)?);
                }
                "flash" => {
                    shaped.insert("flash".into(), sub("flash", FLASH_KEYS)?);
                }
                "rtos" => {
                    shaped.insert("rtos".into(), sub("rtos", RTOS_KEYS)?);
                }
                other if TRACE32_KEYS.contains(&other) => {
                    powerview.insert(key.clone(), value.clone());
                }
                other => {
                    return Err(format!(
                        "unknown key trace32.{other} (expected {}, or the tables \
                         [trace32.target], [trace32.flash], [trace32.rtos])",
                        TRACE32_KEYS.join(", ")
                    ));
                }
            }
        }
    }
    shaped.insert("trace32".into(), Value::Table(powerview));
    Ok(shaped)
}

pub fn parse(text: &str, file: &Path, root: &Path) -> Result<Config, String> {
    let document: Table = text
        .parse()
        .map_err(|error: toml::de::Error| error.message().trim_end().to_string())?;
    for key in document.keys() {
        if !matches!(
            key.as_str(),
            "project" | "clock" | "rtt" | "trace32" | "openocd"
        ) {
            return Err(format!(
                "unknown section [{key}] (expected [project], [clock], [rtt], [trace32], [openocd])"
            ));
        }
    }
    let project = table(&document, "project")?.ok_or("[project] is missing")?;
    check_keys(
        project,
        "project",
        &["backend", "program", "elf", "extra_elf"],
    )?;
    let backend = match string(project, "project", "backend")?.as_deref() {
        Some("openocd") => Some(Backend::OpenOcd),
        Some("trace32") => Some(Backend::Trace32),
        Some("") | None => None,
        Some(other) => {
            return Err(format!(
                "project.backend {other:?} must be \"openocd\" or \"trace32\""
            ));
        }
    };
    let elf_text = match string(project, "project", "elf")? {
        Some(elf) if !elf.is_empty() => elf,
        _ => return Err("project.elf is missing".into()),
    };
    let elf = root.join(&elf_text);
    let extra_elf = strings(project, "project", "extra_elf")?
        .iter()
        .map(|text| root.join(text))
        .collect();
    let program = match string(project, "project", "program")? {
        Some(program) if !program.is_empty() => program,
        _ => Path::new(&elf_text)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    let openocd = table(&document, "openocd")?
        .map(|section| parse_openocd(section, root))
        .transpose()?;
    if backend == Some(Backend::OpenOcd) && openocd.is_none() {
        return Err("project.backend is \"openocd\" but there is no [openocd] section".into());
    }
    let trace32 = trace32_document(&document, project, &program)?;
    Ok(Config {
        file: file.to_path_buf(),
        root: root.to_path_buf(),
        backend,
        program,
        elf,
        extra_elf,
        openocd,
        trace32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_text(text: &str) -> Result<Config, String> {
        parse(text, Path::new("/p/rvdbg.toml"), Path::new("/p"))
    }

    const FULL: &str = r#"
        [project]
        backend = "openocd"
        elf = "build/app.elf"
        extra_elf = ["build/guest.elf"]
        [clock]
        XOSC = "40MHz"
        [rtt]
        symbol = "_SEGGER_RTT"
        [trace32]
        rcl_port = 20001
        [trace32.target]
        cpu = "SR6P6"
        [trace32.flash]
        args = []
        [openocd]
        executable = "/opt/bin/openocd-stellar"
        config = ["board/sr6p6_evb.cfg"]
        search = ["scripts"]
        commands = ["adapter speed 4000"]
        tcl_port = 7777
        gdb_port = 4444
        gdb_target = "sr6p6.k0c0.core"
        gdb = "/opt/bin/arm-none-eabi-gdb"
        memory_ap = "sr6p6.axi"
        timeout = 120
        otp = [{ start = 0x29F87000, size = 0x1000 }]
    "#;

    #[test]
    fn full_configuration() {
        let config = parse_text(FULL).unwrap();
        assert_eq!(config.backend, Some(Backend::OpenOcd));
        assert_eq!(config.program, "app");
        assert_eq!(
            config.images(),
            [
                PathBuf::from("/p/build/app.elf"),
                PathBuf::from("/p/build/guest.elf")
            ]
        );
        assert_eq!(config.run_dir(), PathBuf::from("/p/.rvdbg"));
        let openocd = config.openocd.unwrap();
        assert_eq!(openocd.search, [PathBuf::from("/p/scripts")]);
        assert_eq!((openocd.tcl_port, openocd.gdb_port), (7777, 4444));
        assert_eq!(openocd.gdb, "/opt/bin/arm-none-eabi-gdb");
        assert_eq!(openocd.gdb_target.as_deref(), Some("sr6p6.k0c0.core"));
        assert_eq!(openocd.memory_ap.as_deref(), Some("sr6p6.axi"));
        assert_eq!(openocd.otp, [(0x29F8_7000, 0x1000)]);
    }

    #[test]
    fn tracebridge_gets_trace32_toml_shape() {
        let config = parse_text(FULL).unwrap();
        let expected: Table = r#"
            [project]
            program = "app"
            elf = "build/app.elf"
            extra_elf = ["build/guest.elf"]
            [rtt]
            symbol = "_SEGGER_RTT"
            [clock]
            XOSC = "40MHz"
            [target]
            cpu = "SR6P6"
            [flash]
            args = []
            [trace32]
            rcl_port = 20001
        "#
        .parse()
        .unwrap();
        assert_eq!(config.trace32, expected);
    }

    #[test]
    fn defaults() {
        let config = parse_text(
            "[project]\nbackend = \"openocd\"\nprogram = \"p\"\nelf = \"a.elf\"\n[openocd]\nconfig = [\"x.cfg\"]\n",
        )
        .unwrap();
        assert_eq!(config.program, "p");
        let openocd = config.openocd.unwrap();
        assert_eq!(openocd.executable, PathBuf::from("openocd-stellar"));
        assert_eq!((openocd.tcl_port, openocd.gdb_port), (6666, 3333));
        assert_eq!(
            (openocd.gdb.as_str(), openocd.timeout),
            ("arm-none-eabi-gdb", 600)
        );
        assert_eq!(openocd.memory_ap, None);
        assert_eq!(openocd.gdb_target, None);
        assert!(openocd.otp.is_empty());
        // TRACE32 without a [trace32] section: tracebridge's defaults.
        let config = parse_text("[project]\nbackend = \"trace32\"\nelf = \"a.elf\"\n").unwrap();
        assert_eq!(config.backend, Some(Backend::Trace32));
        assert!(config.openocd.is_none());
        // Without project.backend the probe decides.
        let config = parse_text("[project]\nelf = \"a.elf\"\n").unwrap();
        assert_eq!(config.backend, None);
    }

    #[test]
    fn errors_name_the_key() {
        let error = |text: &str| parse_text(text).unwrap_err();
        assert_eq!(
            error("[target]\n"),
            "unknown section [target] (expected [project], [clock], [rtt], [trace32], [openocd])"
        );
        assert_eq!(error("[openocd]\n"), "[project] is missing");
        assert_eq!(
            error("[project]\nbackend = \"jlink\"\nelf = \"a\""),
            "project.backend \"jlink\" must be \"openocd\" or \"trace32\""
        );
        assert_eq!(
            error("[project]\nbackend = \"openocd\"\nelf = \"a\""),
            "project.backend is \"openocd\" but there is no [openocd] section"
        );
        let openocd = |rest: &str| {
            error(&format!(
                "[project]\nbackend = \"openocd\"\nelf = \"a\"\n[openocd]\nconfig = [\"c\"]\n{rest}"
            ))
        };
        assert_eq!(
            openocd("tcl_port = 70000"),
            "openocd.tcl_port must be a port number"
        );
        assert_eq!(
            openocd("gdb_port = 6666"),
            "openocd.tcl_port and openocd.gdb_port must be different"
        );
        let shape = "openocd.otp must be an array of { start = <address>, size = <bytes> }";
        assert_eq!(openocd("otp = 1"), shape);
        assert_eq!(openocd("otp = [{ start = 1 }]"), shape);
        assert_eq!(openocd("otp = [{ start = 1, size = 0 }]"), shape);
        let trace32 = |rest: &str| {
            error(&format!(
                "[project]\nbackend = \"trace32\"\nelf = \"a\"\n{rest}"
            ))
        };
        assert!(
            trace32("[trace32]\nrcl = 1").starts_with("unknown key trace32.rcl (expected sys,")
        );
        assert!(
            trace32("[trace32.target]\ncpus = \"x\"")
                .starts_with("unknown key trace32.target.cpus (expected cpu,")
        );
        assert_eq!(
            trace32("[trace32]\ntarget = 1"),
            "[trace32.target] must be a table"
        );
        assert!(trace32("[rtt]\nsym = 1").starts_with("unknown key rtt.sym"));
        assert!(
            error("[project]\nbackend = \"openocd\"\nelf = \"a\"\ntypo = 1")
                .starts_with("unknown key project.typo")
        );
    }

    #[test]
    fn the_file_is_found_upwards() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(dir.path().join(FILE_NAME), "").unwrap();
        assert_eq!(find(None, &nested).unwrap(), dir.path().join(FILE_NAME));
        assert_eq!(
            find(Some(Path::new("x.toml")), &nested).unwrap(),
            nested.join("x.toml")
        );
        let empty = tempfile::tempdir().unwrap();
        assert!(
            find(None, empty.path())
                .unwrap_err()
                .starts_with("no rvdbg.toml found")
        );
    }
}
