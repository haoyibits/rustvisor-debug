//! `rvdbg init`: write `rvdbg.toml` for a rustvisor project on the SR6P6.
//!
//! The defaults are rustvisor's: the diagnostic boot image and its Guest,
//! the SR6P6 TRACE32 settings, the SR6P6 EVB OpenOCD script, the UTEST boot
//! record as the OTP area.

use std::path::{Path, PathBuf};

use toml::Value;

use crate::config::{self, FILE_NAME};

/// `(key, default)` pairs in the order they are written.
type Defaults = &'static [(&'static str, Default)];

#[derive(Clone, Copy)]
enum Default {
    Text(&'static str),
    Texts(&'static [&'static str]),
    Integer(i64),
    Float(f64),
}

impl Default {
    fn value(self) -> Value {
        match self {
            Default::Text(text) => Value::String(text.into()),
            Default::Texts(items) => Value::Array(
                items
                    .iter()
                    .map(|item| Value::String((*item).into()))
                    .collect(),
            ),
            Default::Integer(value) => Value::Integer(value),
            Default::Float(value) => Value::Float(value),
        }
    }
}

const PROJECT: Defaults = &[
    ("program", Default::Text("boot_diagnostic")),
    (
        "elf",
        Default::Text("target/thumbv8r-none-eabihf/release/boot_diagnostic"),
    ),
    (
        "extra_elf",
        Default::Texts(&["target/thumbv8r-none-eabihf/release/scenario-guest"]),
    ),
];
const RTT: Defaults = &[
    ("symbol", Default::Text("_SEGGER_RTT")),
    ("control_block_address", Default::Text("")),
    ("poll_interval", Default::Float(0.02)),
];
const TRACE32: Defaults = &[
    ("sys", Default::Text("")),
    ("host", Default::Text("")),
    ("executable", Default::Text("t32marm-qt")),
    ("binary", Default::Text("")),
    ("config", Default::Text("")),
    ("debug_adapter", Default::Text("")),
    ("rcl_port", Default::Integer(20000)),
    ("dap_port", Default::Integer(58870)),
    ("dap_backend_port", Default::Integer(58871)),
    ("dap_backend_timeout", Default::Integer(30)),
    ("operation_timeout", Default::Integer(600)),
];
const TARGET: Defaults = &[
    ("cpu", Default::Text("SR6P6")),
    ("cores", Default::Text("1.")),
    ("mem_access", Default::Text("AXI")),
    ("jtag_clock", Default::Text("10MHz")),
    ("dual_port", Default::Text("ON")),
];
const FLASH: Defaults = &[
    ("chip", Default::Text("")),
    ("script", Default::Text("")),
    ("args", Default::Texts(&[])),
];

/// A key = value line: the value as TOML writes it.
fn line(key: &str, value: &Value) -> String {
    format!("{key} = {value}\n")
}

/// The keys of one section with their defaults, in order.
fn section(defaults: Defaults) -> String {
    defaults
        .iter()
        .map(|(key, default)| line(key, &default.value()))
        .collect()
}

/// The text of `rvdbg.toml`.
pub fn render() -> String {
    let mut text = String::new();
    text.push_str(
        "# rvdbg project configuration, written by 'rvdbg init'.\n\
         # Relative paths are relative to this file. 'rvdbg flash' flashes the images,\n\
         # 'rvdbg debug' starts the debug server, 'rvdbg vscode' writes the IDE files.\n",
    );
    text.push_str(
        "\n[project]\n\
         # The backend follows the connected probe: a Lauterbach probe or a running\n\
         # PowerView means trace32 (TRACE32), a StellarLINK means openocd; with both,\n\
         # trace32. 'rvdbg --backend <name>' or this key fixes it.\n\
         # backend = \"openocd\"\n",
    );
    text.push_str(&section(PROJECT));

    text.push_str("\n# Frequencies only the board knows (debug clock, check files).\n[clock]\n");
    text.push_str("XOSC = \"40MHz\"\n");

    text.push_str("\n# SEGGER RTT.\n[rtt]\n");
    text.push_str(&section(RTT));

    text.push_str(
        "\n# The images' FDX link ('rvdbg fdx', the board runner): two FDX channels\n\
         # (BufferE) back to back in one static.\n[fdx]\n",
    );
    text.push_str(&format!("symbol = \"{}\"\n", rustvisor_debug::fdx::SYMBOL));

    text.push_str(
        "\n# TRACE32 (tracebridge). Empty paths are derived from sys (default ~/t32).\n[trace32]\n",
    );
    text.push_str(&section(TRACE32));
    text.push_str("\n[trace32.target]\n");
    text.push_str(&section(TARGET));
    text.push_str(
        "\n# Empty script: chosen by chip (flash.chip, else target.cpu) from\n\
         # ~/.config/rvdbg/flash and the TRACE32 installation.\n[trace32.flash]\n",
    );
    text.push_str(&section(FLASH));

    text.push_str(
        "\n# StellarLINK through OpenOCD: brew install haoyibits/tap/openocd-stellar, or\n\
         # curl -fsSL https://github.com/haoyibits/OpenOCD/releases/latest/download/install.sh | sh\n\
         [openocd]\n\
         executable = \"openocd-stellar\"\n\
         config = [\"board/sr6p6_evb.cfg\"]\n\
         search = []\n\
         commands = []\n\
         tcl_port = 6666\n\
         gdb_port = 3333\n\
         # The core GDB debugs; only it gets a GDB server. halt, go, fault and\n\
         # check use it too (set core = \"...\" for another one).\n\
         gdb_target = \"sr6p6.k0c0.core\"\n\
         gdb = \"arm-none-eabi-gdb\"\n\
         # Reads physical memory: the comparison after flashing and the OTP check.\n\
         memory_ap = \"sr6p6.axi\"\n\
         timeout = 600\n\
         # One-time-programmable flash: UTEST with the boot record. Flashing stops\n\
         # when an image would change it (rvdbg flash --write-otp allows it).\n\
         otp = [{ start = 0x29F87000, size = 0x1000 }]\n",
    );
    text
}

/// Write `rvdbg.toml` into `directory`; never overwrites.
pub fn init(directory: &Path) -> Result<PathBuf, String> {
    let path = directory.join(FILE_NAME);
    if path.exists() {
        return Err(format!(
            "{} already exists; edit it, or remove it to start over",
            path.display()
        ));
    }
    let text = render();
    // What is written must load.
    config::parse(&text, &path, directory).map_err(|error| {
        format!("internal error: the generated {FILE_NAME} is invalid: {error}")
    })?;
    std::fs::write(&path, text)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_load_for_both_backends() {
        {
            let text = render();
            let config = config::parse(&text, Path::new("/p/rvdbg.toml"), Path::new("/p")).unwrap();
            assert_eq!(config.backend, None);
            assert_eq!(config.program, "boot_diagnostic");
            assert_eq!(config.extra_elf.len(), 1);
            let openocd = config.openocd.unwrap();
            assert_eq!(openocd.otp, [(0x29F8_7000, 0x1000)]);
            assert_eq!(openocd.memory_ap.as_deref(), Some("sr6p6.axi"));
            assert_eq!(openocd.gdb_target.as_deref(), Some("sr6p6.k0c0.core"));
            assert_eq!(config.trace32["target"]["cpu"].as_str(), Some("SR6P6"));
            assert_eq!(config.trace32["clock"]["XOSC"].as_str(), Some("40MHz"));
        }
    }

    #[test]
    fn init_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = init(dir.path()).unwrap();
        assert!(path.is_file());
        let error = init(dir.path()).unwrap_err();
        assert!(error.contains("already exists"), "{error}");
    }
}
