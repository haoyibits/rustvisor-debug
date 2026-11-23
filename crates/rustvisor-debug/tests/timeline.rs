//! `rvdbg swtrace --from … --html` on records from the board (SR6P6,
//! 2026-11-23, `tools/timeline/samples`): the EL2 + IPC run of the five-VM
//! image, hypervisor with el2-trace and the ping-*-traced guests.

use rustvisor_debug::{swtrace, timeline};

const FIVE_VM: [&str; 10] = [
    "C0C0=VM1",
    "C0C1=VM2",
    "C1C0=VM3",
    "C1C1=VM4",
    "C2C0=VM5",
    "guest0=VM1:C0C0",
    "guest1=VM2:C0C1",
    "guest2=VM3:C1C0",
    "guest3=VM4:C1C1",
    "guest4=VM5:C2C0",
];

#[test]
fn the_board_run_becomes_five_panels_and_every_message_pairs() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tools/timeline/samples/el2-ping-traced.csv"
    );
    let text = std::fs::read_to_string(path).unwrap();
    let found = swtrace::from_csv(&text, swtrace::SYMBOL).unwrap();
    assert_eq!(found.len(), 8);
    let map = timeline::Map::parse(FIVE_VM).unwrap();
    let texts = timeline::Texts {
        name: "el2-ping-traced".into(),
        source: "el2-ping-traced.csv".into(),
    };
    let doc = timeline::data(&found, swtrace::SYMBOL, &map, &texts).unwrap();
    let run = &doc["runs"][0];
    let vms: Vec<&str> = run["units"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["vm"].as_str().unwrap())
        .collect();
    assert_eq!(vms, ["VM1", "VM2", "VM3", "VM4", "VM5"]);
    assert_eq!(run["units"][0]["ring"], "EL2 ring C0C0 + guest0 ring");
    assert_eq!(run["msgs"].as_array().unwrap().len(), 3604);
    assert_eq!(run["unpaired"], 0);
    assert_eq!(doc["resolution_ns"], 50);
    // One block per EL2_EXIT in the file (each has its EL2_ENTRY).
    let el2: usize = run["units"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|u| u["segs"].as_array().unwrap())
        .filter(|s| s[2] == "el2")
        .count();
    assert_eq!(el2, 8);
    assert!(timeline::page(&doc).contains("\"rustvisor-timeline/3\""));
}
