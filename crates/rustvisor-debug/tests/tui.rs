#![cfg(unix)]

use std::process::Command;

#[test]
fn inline_session_runs_commands_and_consoles_on_a_pty() {
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pty_tui.py");
    let result = Command::new("python3")
        .args([fixture, env!("CARGO_BIN_EXE_rvdbg")])
        .output()
        .expect("run the PTY fixture with python3");
    assert!(
        result.status.success(),
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );
}
