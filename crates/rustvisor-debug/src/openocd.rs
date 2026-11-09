//! The StellarLINK backend: OpenOCD (openocd-stellar) through stellarbridge.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use probecore::style::Style;
use serde_json::{Value, json};

use crate::EXIT_FAILED;
use crate::config::{Config, OpenOcd};

/// The VS Code task that runs `rvdbg debug`; the launch configuration
/// starts it first.
pub const TASK_LABEL: &str = "rvdbg: debug server";
pub const LAUNCH_NAME: &str = "StellarLINK: Attach";
/// `rvdbg debug` prints these; the task waits for them.
const STARTING: &str = "[rvdbg] starting OpenOCD";
const READY: &str = "[rvdbg] GDB server ready on 127.0.0.1:";

pub(crate) fn server_config(
    openocd: &OpenOcd,
    log: Option<PathBuf>,
    gdb: bool,
) -> stellarbridge::ServerConfig {
    stellarbridge::ServerConfig {
        executable: openocd.executable.clone(),
        search: openocd.search.clone(),
        config: openocd.config.clone(),
        commands: openocd.commands.clone(),
        tcl_port: openocd.tcl_port,
        gdb_port: gdb.then_some(openocd.gdb_port),
        gdb_target: if gdb {
            openocd.gdb_target.clone()
        } else {
            None
        },
        log,
        startup_timeout: Duration::from_secs(30),
        own_process_group: false,
    }
}

/// Flash through `server` (the session's OpenOCD, a running one, or one
/// started for this command).
pub fn flash(
    config: &Config,
    server: &mut stellarbridge::Server,
    verify: bool,
    run: bool,
    write_otp: bool,
) -> Result<u8, String> {
    let style = Style::stdout();
    let images = config.images();
    let openocd = config.openocd()?;
    if openocd.memory_ap.is_none() {
        if !openocd.otp.is_empty() {
            return Err(
                "openocd.otp needs openocd.memory_ap (the MEM-AP target that reads physical \
                 memory) to check the OTP areas before writing"
                    .into(),
            );
        }
        if verify {
            return Err(
                "comparing needs openocd.memory_ap (the MEM-AP target that reads physical \
                 memory); set it or pass --no-verify"
                    .into(),
            );
        }
    }
    let log = server
        .log()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "OpenOCD's output".into());
    let options = stellarbridge::FlashOptions {
        memory_ap: openocd.memory_ap.clone(),
        verify,
        otp: openocd
            .otp
            .iter()
            .map(|&(start, size)| stellarbridge::OtpRange { start, size })
            .collect(),
        write_otp,
        run,
        timeout: Duration::from_secs(openocd.timeout),
    };
    let started = std::time::Instant::now();
    let mut display = FlashDisplay::new();
    let result = stellarbridge::flash(server, &images, &options, &mut |event| {
        display.update(event, style);
    });
    display.finish();
    let report = result.map_err(|e| e.0)?;

    for check in &report.otp {
        let status = if check.differing == 0 {
            style.good("same as on the board")
        } else {
            style.warn(format!("{} bytes changed (--write-otp)", check.differing))
        };
        println!(
            "  {}  {}  {:>7} bytes  OTP {status}",
            short(&check.image),
            style.address(format!("0x{:08X}", check.address)),
            check.size
        );
    }
    for (image, segments) in &report.verified {
        for segment in segments {
            let status = if segment.differing == 0 {
                style.good("match")
            } else {
                style.bad(format!(
                    "MISMATCH {} bytes, first at 0x{:08X}",
                    segment.differing,
                    segment.first_difference.unwrap_or_default()
                ))
            };
            println!(
                "  {}  {}  {:>7} bytes  {status}",
                short(image),
                style.address(format!("0x{:08X}", segment.paddr)),
                segment.size
            );
        }
    }
    let seconds = started.elapsed().as_secs_f64();
    if report.differing() > 0 {
        println!(
            "{}",
            style.bad(format!("flash differs from the images; log {log}"))
        );
        return Ok(EXIT_FAILED);
    }
    let what = if report.verified.is_empty() {
        "written (not compared)"
    } else {
        "written and compared"
    };
    let after = if run { ", running" } else { ", halted" };
    println!(
        "{}",
        style.good(format!(
            "flashed {} image(s) in {seconds:.1} s: {what}{after}",
            report.images.len()
        ))
    );
    Ok(0)
}

/// A single updating line on terminals; bounded, plain progress lines in logs.
struct FlashDisplay {
    terminal: bool,
    active: bool,
    last: Option<(&'static str, usize)>,
    started: std::time::Instant,
}

impl FlashDisplay {
    fn new() -> Self {
        Self {
            terminal: std::io::stdout().is_terminal()
                && std::env::var("TERM").as_deref() != Ok("dumb"),
            active: false,
            last: None,
            started: std::time::Instant::now(),
        }
    }

    fn finish(&mut self) {
        if self.active {
            println!();
            self.active = false;
        }
    }

    fn update(&mut self, event: stellarbridge::FlashProgress, style: Style) {
        match event {
            stellarbridge::FlashProgress::Step(step) => {
                self.finish();
                println!("{} {step}", style.label("step"));
            }
            stellarbridge::FlashProgress::Bytes {
                phase,
                completed,
                total,
            } => {
                let percent = if total == 0 {
                    100
                } else {
                    ((completed as u128 * 100) / total as u128).min(100) as usize
                };
                let new_phase = self.last.is_none_or(|last| last.0 != phase);
                if new_phase {
                    self.finish();
                    self.started = std::time::Instant::now();
                }
                // Logs advance in 10% steps; terminals in 1% steps.
                let bucket = if self.terminal { percent } else { percent / 10 };
                if !new_phase && self.last == Some((phase, bucket)) {
                    return;
                }
                self.last = Some((phase, bucket));
                let filled = percent / 5;
                let line = format!(
                    "{phase:7} [{}{}] {percent:3}%  {:.1}/{:.1} KiB  {:.1}s",
                    "#".repeat(filled),
                    "-".repeat(20 - filled),
                    completed as f64 / 1024.0,
                    total as f64 / 1024.0,
                    self.started.elapsed().as_secs_f64()
                );
                if self.terminal {
                    print!("\r\x1b[K{line}");
                    let _ = std::io::stdout().flush();
                    self.active = true;
                } else {
                    println!("{line}");
                }
            }
        }
    }
}

fn short(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The Tcl word for the target that serves GDB.
fn gdb_target_word(openocd: &OpenOcd) -> String {
    match &openocd.gdb_target {
        Some(target) => stellarbridge::quote(target),
        None => "[target current]".to_string(),
    }
}

/// The commands `rvdbg debug` sends once OpenOCD is up.
pub(crate) fn debug_commands(openocd: &OpenOcd, keep: bool) -> Vec<String> {
    // The code runs from flash, where GDB's software breakpoints cannot be
    // written; OpenOCD turns every breakpoint into a hardware one.
    let mut commands = vec!["gdb_breakpoint_override hard".to_string()];
    if !keep {
        // One debug session per server: VS Code starts the server for each
        // session and does not stop it afterwards.
        commands.push(format!(
            "{} configure -event gdb-detach {{ shutdown }}",
            gdb_target_word(openocd)
        ));
    }
    commands
}

/// Run OpenOCD with its GDB server in the foreground (`rvdbg debug`) until
/// GDB detaches (or, with `keep`, until Ctrl-C). OpenOCD's output goes to
/// the terminal.
pub fn debug(config: &Config, keep: bool) -> Result<u8, String> {
    let openocd = config.openocd()?;
    let target = openocd
        .gdb_target
        .clone()
        .unwrap_or_else(|| "the first target".into());
    // An interactive rvdbg session already runs OpenOCD with its GDB
    // server: say so in the lines the VS Code task waits for, and leave it.
    if stellarbridge::Server::running(openocd.tcl_port)
        && stellarbridge::Server::running(openocd.gdb_port)
    {
        println!(
            "{STARTING}: using the OpenOCD that is already running (Tcl port {})",
            openocd.tcl_port
        );
        println!(
            "{READY}{} ({target}); it belongs to the rvdbg session that started it",
            openocd.gdb_port
        );
        let _ = std::io::stdout().flush();
        return Ok(0);
    }
    let server_config = server_config(openocd, None, true);
    println!(
        "{STARTING}: {} {}",
        openocd.executable.display(),
        server_config.arguments().join(" ")
    );
    let _ = std::io::stdout().flush();
    let mut server = stellarbridge::Server::start(&server_config).map_err(|e| e.0)?;
    for command in debug_commands(openocd, keep) {
        server.eval(&command).map_err(|e| e.0)?;
    }
    let until = if keep {
        "Ctrl-C stops it"
    } else {
        "it stops when GDB detaches"
    };
    println!("{READY}{} ({target}); {until}", openocd.gdb_port);
    let _ = std::io::stdout().flush();
    let status = server.wait().map_err(|e| e.0)?;
    if status.success() {
        Ok(0)
    } else {
        Err(format!("OpenOCD exited with {status}"))
    }
}

/// An executable named without a directory, looked up in PATH like the shell
/// does; VS Code started from the Dock may not have the user's PATH.
fn absolute_program(program: &str) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return Some(path.to_path_buf());
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
}

fn regex_escape(text: &str) -> String {
    let mut escaped = String::new();
    for c in text.chars() {
        if "\\^$.|?*+()[]{}".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// A file name for a GDB command line.
fn gdb_word(path: &Path) -> String {
    let text = path.display().to_string();
    if text.contains([' ', '"', '\'', '\\']) {
        format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        text
    }
}

/// The `tasks.json` and `launch.json` documents for cortex-debug.
pub fn vscode_templates(config: &Config, exe: &Path, gdb: &Path) -> (Value, Value) {
    let openocd = config.openocd.as_ref().expect("checked by the caller");
    let task = json!({
        "label": TASK_LABEL,
        "detail": format!("Started by {LAUNCH_NAME}: OpenOCD with its GDB server"),
        "hide": true,
        "type": "process",
        "isBackground": true,
        "command": exe.display().to_string(),
        "args": ["--config", config.file.display().to_string(), "--backend", "openocd", "debug"],
        "presentation": {"reveal": "always", "focus": false, "panel": "dedicated", "clear": true},
        "problemMatcher": [{
            "owner": "rvdbg",
            "pattern": {"regexp": "^(Error): (.*)$", "severity": 1, "message": 2},
            "background": {
                "activeOnStart": true,
                "beginsPattern": format!("^{}", regex_escape(STARTING)),
                "endsPattern": format!("^{}{}", regex_escape(READY), openocd.gdb_port),
            }
        }]
    });
    let tasks = json!({"version": "2.0.0", "tasks": [task]});

    let mut launch = json!({
        "name": LAUNCH_NAME,
        "type": "cortex-debug",
        "request": "attach",
        "servertype": "external",
        "gdbTarget": format!("localhost:{}", openocd.gdb_port),
        "gdbPath": gdb.display().to_string(),
        "executable": config.elf.display().to_string(),
        "cwd": "${workspaceFolder}",
        "preLaunchTask": TASK_LABEL,
    });
    let file_name = gdb
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let (Some(prefix), Some(directory)) = (file_name.strip_suffix("-gdb"), gdb.parent()) {
        // objdump and nm next to GDB, for the disassembly and static variables.
        if !directory.as_os_str().is_empty() {
            launch["armToolchainPath"] = json!(directory.display().to_string());
        }
        launch["toolchainPrefix"] = json!(prefix);
    }
    if !config.extra_elf.is_empty() {
        let commands: Vec<String> = config
            .extra_elf
            .iter()
            .map(|path| format!("add-symbol-file {}", gdb_word(path)))
            .collect();
        launch["postAttachCommands"] = json!(commands);
    }
    let launch = json!({"version": "0.2.0", "configurations": [launch]});
    (tasks, launch)
}

/// `rvdbg vscode` for OpenOCD: cortex-debug attaches to `rvdbg debug`.
/// `created`: files this command wrote moments ago; they are not backed up.
pub fn vscode(config: &Config, exe: &Path, created: &[PathBuf]) -> Result<u8, String> {
    let openocd = config.openocd()?;
    let style = Style::stdout();
    let gdb = match absolute_program(&openocd.gdb) {
        Some(path) => path,
        None => {
            eprintln!(
                "{} {} is not in PATH; launch.json names it as it is",
                style.warn("rvdbg: warning:"),
                openocd.gdb
            );
            PathBuf::from(&openocd.gdb)
        }
    };
    let (tasks, launch) = vscode_templates(config, exe, &gdb);
    let directory = config.root.join(".vscode");
    for (name, file, document) in [
        (
            "tasks.json",
            tracebridge::backend::VscodeFile::Tasks,
            &tasks,
        ),
        (
            "launch.json",
            tracebridge::backend::VscodeFile::Launch,
            &launch,
        ),
    ] {
        let path = directory.join(name);
        let backup = !created.contains(&path);
        tracebridge::backend::merge_vscode_file(&path, file, document, backup)
            .map_err(|e| e.to_string())?;
    }
    println!(
        "\nDone. In VS Code: install the \"Cortex-Debug\" extension (marus25.cortex-debug), \
         then pick '{LAUNCH_NAME}' in Run and Debug and press F5. The session starts at the \
         reset vector (the board script resets the core when GDB attaches); flash with \
         'rvdbg flash' from a terminal first."
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        crate::config::parse(
            r#"
            [project]
            backend = "openocd"
            elf = "build/app"
            extra_elf = ["build/my guest"]
            [openocd]
            config = ["board/sr6p6_evb.cfg"]
            gdb_port = 4444
            gdb_target = "sr6p6.k0c0.core"
            "#,
            Path::new("/p/rvdbg.toml"),
            Path::new("/p"),
        )
        .unwrap()
    }

    #[test]
    fn debug_commands_stop_on_detach_unless_kept() {
        let config = config();
        let openocd = config.openocd().unwrap();
        assert_eq!(
            debug_commands(openocd, false),
            [
                "gdb_breakpoint_override hard",
                "sr6p6.k0c0.core configure -event gdb-detach { shutdown }"
            ]
        );
        assert_eq!(
            debug_commands(openocd, true),
            ["gdb_breakpoint_override hard"]
        );
        let mut anonymous = openocd.clone();
        anonymous.gdb_target = None;
        assert_eq!(
            debug_commands(&anonymous, false)[1],
            "[target current] configure -event gdb-detach { shutdown }"
        );
        let arguments = server_config(openocd, None, true).arguments();
        assert!(
            arguments.contains(&"sr6p6.k0c0.core configure -gdb-port 4444".to_string()),
            "{arguments:?}"
        );
        assert!(server_config(openocd, None, false).arguments()[1].contains("gdb_port disabled"));
    }

    #[test]
    fn vscode_templates_match_the_debug_output() {
        let config = config();
        let (tasks, launch) = vscode_templates(
            &config,
            Path::new("/bin/rvdbg"),
            Path::new("/opt/homebrew/bin/arm-none-eabi-gdb"),
        );
        let task = &tasks["tasks"][0];
        assert_eq!(task["label"], TASK_LABEL);
        assert_eq!(task["command"], "/bin/rvdbg");
        assert_eq!(
            task["args"],
            json!(["--config", "/p/rvdbg.toml", "--backend", "openocd", "debug"])
        );
        let background = &task["problemMatcher"][0]["background"];
        let ends = background["endsPattern"].as_str().unwrap();
        assert_eq!(
            ends,
            "^\\[rvdbg\\] GDB server ready on 127\\.0\\.0\\.1:4444"
        );
        assert_eq!(
            background["beginsPattern"].as_str().unwrap(),
            "^\\[rvdbg\\] starting OpenOCD"
        );
        let entry = &launch["configurations"][0];
        assert_eq!(entry["name"], LAUNCH_NAME);
        assert_eq!(entry["type"], "cortex-debug");
        assert_eq!(entry["gdbTarget"], "localhost:4444");
        assert_eq!(entry["executable"], "/p/build/app");
        assert_eq!(entry["preLaunchTask"], TASK_LABEL);
        assert_eq!(entry["armToolchainPath"], "/opt/homebrew/bin");
        assert_eq!(entry["toolchainPrefix"], "arm-none-eabi");
        assert_eq!(
            entry["postAttachCommands"],
            json!(["add-symbol-file \"/p/build/my guest\""])
        );
    }

    #[test]
    fn vscode_files_are_merged() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rvdbg.toml");
        let text = "[project]\nbackend = \"openocd\"\nelf = \"app\"\n\
                    [openocd]\nconfig = [\"b.cfg\"]\ngdb = \"/x/arm-none-eabi-gdb\"\n";
        std::fs::write(&file, text).unwrap();
        let config = crate::config::parse(text, &file, dir.path()).unwrap();
        std::fs::create_dir(dir.path().join(".vscode")).unwrap();
        std::fs::write(
            dir.path().join(".vscode/launch.json"),
            "{\n // mine\n \"version\": \"0.2.0\",\n \"configurations\": [{\"name\": \"Other\", \"type\": \"x\"},],\n}\n",
        )
        .unwrap();
        vscode(&config, Path::new("/bin/rvdbg"), &[]).unwrap();
        let launch: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".vscode/launch.json")).unwrap(),
        )
        .unwrap();
        let names: Vec<&str> = launch["configurations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Other", LAUNCH_NAME]);
        let tasks: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".vscode/tasks.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(tasks["tasks"][0]["label"], TASK_LABEL);
    }

    #[test]
    fn files_the_command_created_are_not_backed_up() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rvdbg.toml");
        let text = "[project]\nelf = \"app\"\n[openocd]\nconfig = [\"b.cfg\"]\ngdb = \"/x/gdb\"\n";
        let config = crate::config::parse(text, &file, dir.path()).unwrap();
        let vscode_dir = dir.path().join(".vscode");
        std::fs::create_dir(&vscode_dir).unwrap();
        for (name, document) in [
            ("tasks.json", r#"{"version": "2.0.0", "tasks": []}"#),
            (
                "launch.json",
                r#"{"version": "0.2.0", "configurations": []}"#,
            ),
        ] {
            std::fs::write(vscode_dir.join(name), document).unwrap();
        }
        let created = [vscode_dir.join("tasks.json")];
        vscode(&config, Path::new("/bin/rvdbg"), &created).unwrap();
        let backups = |prefix: &str| {
            std::fs::read_dir(&vscode_dir)
                .unwrap()
                .filter(|entry| {
                    let name = entry.as_ref().unwrap().file_name();
                    name.to_string_lossy().starts_with(prefix)
                })
                .count()
        };
        assert_eq!(backups("tasks.json.bak."), 0);
        assert_eq!(backups("launch.json.bak."), 1);
    }

    #[test]
    fn programs_are_found_in_path() {
        assert_eq!(
            absolute_program("/x/y/gdb"),
            Some(PathBuf::from("/x/y/gdb"))
        );
        assert!(absolute_program("sh").is_some_and(|path| path.is_absolute()));
        assert_eq!(absolute_program("no-such-program-rvdbg"), None);
    }
}
