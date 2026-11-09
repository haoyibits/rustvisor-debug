//! Console output shared by the commands.

use std::path::{Path, PathBuf};

/// What stdin holds right now, without blocking: empty when nothing is
/// there, `None` at the end of the input. A terminal in its usual (line)
/// mode only shows a line once Return is pressed.
pub fn read_stdin_now() -> Option<Vec<u8>> {
    #[cfg(unix)]
    {
        use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
        use std::os::fd::AsFd;
        let stdin = std::io::stdin();
        let mut fds = [PollFd::new(stdin.as_fd(), PollFlags::POLLIN)];
        if !matches!(poll(&mut fds, PollTimeout::ZERO), Ok(count) if count > 0) {
            return Some(Vec::new());
        }
        let mut buffer = [0u8; 256];
        match nix::unistd::read(stdin.as_fd(), &mut buffer) {
            Ok(0) => None,
            Ok(count) => Some(buffer[..count].to_vec()),
            Err(_) => Some(Vec::new()),
        }
    }
    #[cfg(not(unix))]
    {
        Some(Vec::new())
    }
}

/// The running program's name for messages and hints: `rvdbg` when this
/// executable is rvdbg (which runs tracebridge built in), else
/// `tracebridge`.
pub fn program() -> &'static str {
    static PROGRAM: std::sync::LazyLock<&'static str> = std::sync::LazyLock::new(|| {
        let rvdbg = std::env::current_exe()
            .ok()
            .is_some_and(|exe| exe.file_stem().is_some_and(|stem| stem == "rvdbg"));
        if rvdbg { "rvdbg" } else { "tracebridge" }
    });
    *PROGRAM
}

/// `info` in cli.py: a cyan `[tracebridge]` (`[rvdbg]`) prefix on stdout.
pub fn info(message: &str) {
    println!("\x1b[1;36m[{}]\x1b[0m {message}", program());
}

/// The absolute path of this executable, used wherever tracebridge calls itself
/// (PowerView toolbar, VS Code task, RustRover run configuration).
///
/// A Homebrew install runs from `<prefix>/Cellar/tracebridge/<version>/bin`,
/// which `brew upgrade` deletes; the stable `<prefix>/opt/tracebridge/bin`
/// link is used instead so generated files keep working after upgrades.
pub fn current_exe() -> crate::errors::Result<PathBuf> {
    let exe = std::env::current_exe().map_err(|error| {
        crate::bridge_error!("cannot determine the tracebridge executable: {error}")
    })?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    Ok(homebrew_stable_path(&exe)
        .filter(|path| path.is_file())
        .unwrap_or(exe))
}

/// A tracebridge command (`debug break`, `debug do <script>`, `load`) as the
/// running program spells it: rvdbg has the `debug` commands at its top
/// level and calls `debug break` `halt`.
pub fn command(tracebridge: &str) -> String {
    command_for(program() == "rvdbg", tracebridge)
}

fn command_for(rvdbg: bool, tracebridge: &str) -> String {
    if !rvdbg {
        return format!("tracebridge {tracebridge}");
    }
    match tracebridge.strip_prefix("debug ").unwrap_or(tracebridge) {
        "break" => "rvdbg halt".to_string(),
        other => format!("rvdbg {other}"),
    }
}

/// The configuration file the running program reads, for hints.
pub fn config_file() -> &'static str {
    if program() == "rvdbg" {
        "rvdbg.toml"
    } else {
        crate::config::CONFIG_FILE_NAME
    }
}

/// A trace32.toml key (`flash.chip`, `target.cpu`) as the running program's
/// configuration file spells it: rvdbg.toml nests the TRACE32 tables under
/// `[trace32]`.
pub fn config_key(key: &str) -> String {
    config_key_for(program() == "rvdbg", key)
}

fn config_key_for(rvdbg: bool, key: &str) -> String {
    let nested = ["target.", "flash.", "rtos."]
        .iter()
        .any(|table| key.starts_with(table));
    if rvdbg && nested {
        format!("trace32.{key}")
    } else {
        key.to_string()
    }
}

/// The command that halts the core, for hints.
pub static HALT_COMMAND: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| command("debug break"));

/// The error when no PowerView answers on `port`, in the words of the
/// program that runs (`tracebridge`, `rvdbg`): `flash` and `load` start
/// PowerView themselves and go on, so they come first; `open` only starts it.
pub fn no_powerview(port: u16) -> String {
    let program = program();
    format!(
        "no PowerView on RCL port {port}; '{program} flash' or '{program} load' starts it \
         (or '{program} open', which leaves the target alone); this command never starts it"
    )
}

/// `<prefix>/Cellar/<name>/<version>/bin/<exe>` → `<prefix>/opt/<name>/bin/<exe>`.
pub fn homebrew_stable_path(exe: &Path) -> Option<PathBuf> {
    let file = exe.file_name()?;
    let bin = exe.parent()?;
    let version = bin.parent()?;
    let name = version.parent()?;
    let cellar = name.parent()?;
    if bin.file_name()? != "bin" || cellar.file_name()? != "Cellar" {
        return None;
    }
    Some(
        cellar
            .parent()?
            .join("opt")
            .join(name.file_name()?)
            .join("bin")
            .join(file),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_name_the_running_programs_command() {
        assert_eq!(command_for(false, "debug break"), "tracebridge debug break");
        assert_eq!(command_for(true, "debug break"), "rvdbg halt");
        assert_eq!(command_for(true, "debug do <script>"), "rvdbg do <script>");
        assert_eq!(command_for(true, "load"), "rvdbg load");
        assert_eq!(command_for(false, "load"), "tracebridge load");
    }

    #[test]
    fn hints_name_the_running_programs_config_keys() {
        assert_eq!(config_key_for(false, "flash.chip"), "flash.chip");
        assert_eq!(config_key_for(true, "flash.chip"), "trace32.flash.chip");
        assert_eq!(config_key_for(true, "target.cpu"), "trace32.target.cpu");
        assert_eq!(config_key_for(true, "project.program"), "project.program");
    }

    #[test]
    fn homebrew_cellar_path_maps_to_opt() {
        assert_eq!(
            homebrew_stable_path(Path::new(
                "/opt/homebrew/Cellar/tracebridge/0.1.1/bin/tracebridge"
            )),
            Some(PathBuf::from(
                "/opt/homebrew/opt/tracebridge/bin/tracebridge"
            ))
        );
        assert_eq!(
            homebrew_stable_path(Path::new(
                "/home/linuxbrew/.linuxbrew/Cellar/tracebridge/0.2.0/bin/tracebridge"
            )),
            Some(PathBuf::from(
                "/home/linuxbrew/.linuxbrew/opt/tracebridge/bin/tracebridge"
            ))
        );
        assert_eq!(
            homebrew_stable_path(Path::new("/Users/me/.local/bin/tracebridge")),
            None
        );
        assert_eq!(
            homebrew_stable_path(Path::new("/x/target/release/tracebridge")),
            None
        );
    }
}
