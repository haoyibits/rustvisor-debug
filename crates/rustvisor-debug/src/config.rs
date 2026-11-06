//! The project configuration: one file with the shared keys under
//! `[project]` and one section per backend. The file name `rvdbg.toml` is
//! provisional (decided together with `rvdbg init`).
//!
//! ```toml
//! [project]
//! backend = "openocd"                  # or "trace32"
//! elf = "target/thumbv8r-none-eabihf/release/boot_diagnostic"
//! extra_elf = ["target/thumbv8r-none-eabihf/release/scenario-guest"]
//!
//! [openocd]
//! executable = "openocd-stellar"
//! config = ["board/sr6p6_evb.cfg"]     # -f, in this order
//! search = []                          # -s, extra script directories
//! commands = []                        # -c, after the config files
//! tcl_port = 6666
//! memory_ap = "sr6p6.axi"              # MEM-AP that reads physical memory
//! timeout = 600                        # seconds for one image
//! ```
//!
//! Relative paths are relative to the directory of the file.

use std::path::{Path, PathBuf};

use toml::{Table, Value};

pub const FILE_NAME: &str = "rvdbg.toml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    Trace32,
    OpenOcd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenOcd {
    pub executable: PathBuf,
    pub config: Vec<String>,
    pub search: Vec<PathBuf>,
    pub commands: Vec<String>,
    pub tcl_port: u16,
    pub memory_ap: Option<String>,
    pub timeout: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub file: PathBuf,
    pub root: PathBuf,
    pub backend: Backend,
    pub elf: PathBuf,
    pub extra_elf: Vec<PathBuf>,
    pub openocd: Option<OpenOcd>,
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
        "no {FILE_NAME} found in {} or any parent directory; pass --config",
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
            return Err(format!("unknown key {section}.{key}"));
        }
    }
    Ok(())
}

pub fn parse(text: &str, file: &Path, root: &Path) -> Result<Config, String> {
    let document: Table = text
        .parse()
        .map_err(|error: toml::de::Error| error.message().trim_end().to_string())?;
    check_keys(&document, "", &["project", "openocd", "trace32"])
        .map_err(|_| "unknown section; expected [project], [openocd], [trace32]".to_string())?;
    let project = table(&document, "project")?.ok_or("[project] is missing")?;
    check_keys(project, "project", &["backend", "elf", "extra_elf"])?;
    let backend = match string(project, "project", "backend")?.as_deref() {
        Some("openocd") => Backend::OpenOcd,
        Some("trace32") => Backend::Trace32,
        Some(other) => {
            return Err(format!(
                "project.backend {other:?} must be \"openocd\" or \"trace32\""
            ));
        }
        None => return Err("project.backend is missing".into()),
    };
    let path = |text: &str| root.join(text);
    let elf = match string(project, "project", "elf")? {
        Some(elf) if !elf.is_empty() => path(&elf),
        _ => return Err("project.elf is missing".into()),
    };
    let extra_elf = strings(project, "project", "extra_elf")?
        .iter()
        .map(|text| path(text))
        .collect();

    let openocd = match table(&document, "openocd")? {
        None => None,
        Some(section) => {
            check_keys(
                section,
                "openocd",
                &[
                    "executable",
                    "config",
                    "search",
                    "commands",
                    "tcl_port",
                    "memory_ap",
                    "timeout",
                ],
            )?;
            let tcl_port = integer(section, "openocd", "tcl_port")?.unwrap_or(6666);
            let tcl_port = u16::try_from(tcl_port)
                .ok()
                .filter(|port| *port > 0)
                .ok_or("openocd.tcl_port must be a port number")?;
            let timeout = integer(section, "openocd", "timeout")?.unwrap_or(600);
            let timeout = u64::try_from(timeout)
                .ok()
                .filter(|seconds| *seconds > 0)
                .ok_or("openocd.timeout must be a positive number of seconds")?;
            let config = strings(section, "openocd", "config")?;
            if config.is_empty() {
                return Err("openocd.config must name at least one configuration file".into());
            }
            Some(OpenOcd {
                executable: PathBuf::from(
                    string(section, "openocd", "executable")?
                        .unwrap_or_else(|| "openocd-stellar".into()),
                ),
                config,
                search: strings(section, "openocd", "search")?
                    .iter()
                    .map(|text| path(text))
                    .collect(),
                commands: strings(section, "openocd", "commands")?,
                tcl_port,
                memory_ap: string(section, "openocd", "memory_ap")?.filter(|ap| !ap.is_empty()),
                timeout,
            })
        }
    };
    if backend == Backend::OpenOcd && openocd.is_none() {
        return Err("project.backend is \"openocd\" but there is no [openocd] section".into());
    }
    Ok(Config {
        file: file.to_path_buf(),
        root: root.to_path_buf(),
        backend,
        elf,
        extra_elf,
        openocd,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_text(text: &str) -> Result<Config, String> {
        parse(text, Path::new("/p/rvdbg.toml"), Path::new("/p"))
    }

    #[test]
    fn full_openocd_configuration() {
        let config = parse_text(
            r#"
            [project]
            backend = "openocd"
            elf = "build/app.elf"
            extra_elf = ["build/guest.elf"]
            [openocd]
            executable = "/opt/bin/openocd-stellar"
            config = ["board/sr6p6_evb.cfg"]
            search = ["scripts"]
            commands = ["adapter speed 4000"]
            tcl_port = 7777
            memory_ap = "sr6p6.axi"
            timeout = 120
            "#,
        )
        .unwrap();
        assert_eq!(config.backend, Backend::OpenOcd);
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
        assert_eq!(openocd.tcl_port, 7777);
        assert_eq!(openocd.memory_ap.as_deref(), Some("sr6p6.axi"));
        assert_eq!(openocd.timeout, 120);
    }

    #[test]
    fn defaults() {
        let config = parse_text(
            "[project]\nbackend = \"openocd\"\nelf = \"a.elf\"\n[openocd]\nconfig = [\"x.cfg\"]\n",
        )
        .unwrap();
        let openocd = config.openocd.unwrap();
        assert_eq!(openocd.executable, PathBuf::from("openocd-stellar"));
        assert_eq!((openocd.tcl_port, openocd.timeout), (6666, 600));
        assert_eq!(openocd.memory_ap, None);
    }

    #[test]
    fn errors_name_the_key() {
        let error = |text: &str| parse_text(text).unwrap_err();
        assert_eq!(
            error("x = 1"),
            "unknown section; expected [project], [openocd], [trace32]"
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
        assert_eq!(
            error("[project]\nbackend = \"openocd\"\nelf = \"a\"\n[openocd]\nconfig = []"),
            "openocd.config must name at least one configuration file"
        );
        assert_eq!(
            error(
                "[project]\nbackend = \"openocd\"\nelf = \"a\"\n[openocd]\nconfig = [\"c\"]\ntcl_port = 70000"
            ),
            "openocd.tcl_port must be a port number"
        );
        assert_eq!(
            error("[project]\nbackend = \"openocd\"\nelf = \"a\"\ntypo = 1"),
            "unknown key project.typo"
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
