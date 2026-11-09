//! A closed stdout (`rvdbg config | head -1`) ends rvdbg quietly with 141
//! instead of a panic message.

#![cfg(unix)]

use std::process::{Command, Stdio};

#[test]
fn a_closed_stdout_ends_quietly_with_141() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("rvdbg.toml");
    std::fs::write(&config, "[project]\nelf = \"app.elf\"\n").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_rvdbg"))
        .arg("--config")
        .arg(&config)
        .args(["--backend", "openocd", "config"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The read end closes right after exec, long before rvdbg has loaded
    // the configuration and prints its first line.
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(141), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
}
