//! The workspace's one-way dependency rule, checked against `cargo metadata`:
//!
//! ```text
//! rustvisor-debug → tracebridge, stellarbridge, probecore
//! tracebridge     → t32rcl, probecore
//! stellarbridge   → oocd, probecore
//! probecore, t32rcl, oocd → (no workspace crates)
//! fdx-oracle      → probecore (tests only)
//! ```
//!
//! Only `rustvisor-debug` knows about rustvisor; the generic crates must never
//! depend on it, and the protocol clients (`t32rcl`, `oocd`) are reached only
//! through their backend. Every kind of dependency (normal, build, dev) counts.
//! A new workspace crate fails this test until it is added to [`ALLOWED`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use serde_json::Value;

/// Workspace crate → workspace crates it may depend on.
const ALLOWED: &[(&str, &[&str])] = &[
    (
        "rustvisor-debug",
        &["tracebridge", "stellarbridge", "probecore"],
    ),
    ("tracebridge", &["t32rcl", "probecore"]),
    ("stellarbridge", &["oocd", "probecore"]),
    ("probecore", &[]),
    ("t32rcl", &[]),
    ("oocd", &[]),
    ("fdx-oracle", &["probecore"]),
];

/// Returns one message per violation of [`ALLOWED`] in `cargo metadata` output.
fn violations(metadata: &Value) -> Vec<String> {
    let allowed: BTreeMap<&str, BTreeSet<&str>> = ALLOWED
        .iter()
        .map(|(name, deps)| (*name, deps.iter().copied().collect()))
        .collect();
    let member_ids: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .expect("workspace_members")
        .iter()
        .map(|id| id.as_str().expect("member id"))
        .collect();
    let members: Vec<&Value> = metadata["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .filter(|package| member_ids.contains(package["id"].as_str().expect("package id")))
        .collect();
    let member_names: BTreeSet<&str> = members
        .iter()
        .map(|package| package["name"].as_str().expect("package name"))
        .collect();

    let mut problems = Vec::new();
    for name in &member_names {
        if !allowed.contains_key(name) {
            problems.push(format!(
                "workspace crate {name} has no entry in the dependency rule"
            ));
        }
    }
    for package in members {
        let name = package["name"].as_str().expect("package name");
        let Some(permitted) = allowed.get(name) else {
            continue;
        };
        for dep in package["dependencies"].as_array().expect("dependencies") {
            let dep_name = dep["name"].as_str().expect("dependency name");
            if !member_names.contains(dep_name) || permitted.contains(dep_name) {
                continue;
            }
            let kind = dep["kind"].as_str().unwrap_or("normal");
            problems.push(format!("{name} must not depend on {dep_name} ({kind})"));
        }
    }
    problems
}

#[test]
fn workspace_follows_the_dependency_rule() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .arg("--manifest-path")
        .arg(&manifest)
        .output()
        .expect("run cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout).expect("metadata JSON");

    let names: BTreeSet<&str> = metadata["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .map(|package| package["name"].as_str().expect("package name"))
        .collect();
    for (name, _) in ALLOWED {
        assert!(names.contains(name), "{name} is not in the workspace");
    }

    let problems = violations(&metadata);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

fn package(name: &str, deps: &[(&str, Option<&str>)]) -> Value {
    let deps: Vec<Value> = deps
        .iter()
        .map(|(dep, kind)| serde_json::json!({ "name": dep, "kind": kind }))
        .collect();
    serde_json::json!({ "id": format!("{name}-id"), "name": name, "dependencies": deps })
}

fn metadata(packages: Vec<Value>) -> Value {
    let ids: Vec<Value> = packages.iter().map(|p| p["id"].clone()).collect();
    serde_json::json!({ "packages": packages, "workspace_members": ids })
}

#[test]
fn generic_crate_depending_on_the_front_end_is_reported() {
    let metadata = metadata(vec![
        package("rustvisor-debug", &[("tracebridge", None)]),
        package(
            "tracebridge",
            &[("rustvisor-debug", Some("dev")), ("clap", None)],
        ),
    ]);
    assert_eq!(
        violations(&metadata),
        ["tracebridge must not depend on rustvisor-debug (dev)"]
    );
}

#[test]
fn backends_and_protocol_clients_stay_apart() {
    let metadata = metadata(vec![
        package("rustvisor-debug", &[("t32rcl", None)]),
        package("stellarbridge", &[("t32rcl", None), ("tracebridge", None)]),
        package("probecore", &[("oocd", Some("build"))]),
        package("t32rcl", &[]),
        package("tracebridge", &[]),
        package("oocd", &[]),
    ]);
    assert_eq!(
        violations(&metadata),
        [
            "rustvisor-debug must not depend on t32rcl (normal)",
            "stellarbridge must not depend on t32rcl (normal)",
            "stellarbridge must not depend on tracebridge (normal)",
            "probecore must not depend on oocd (build)",
        ]
    );
}

#[test]
fn new_crate_without_a_rule_is_reported() {
    let metadata = metadata(vec![package("newcrate", &[])]);
    assert_eq!(
        violations(&metadata),
        ["workspace crate newcrate has no entry in the dependency rule"]
    );
}
