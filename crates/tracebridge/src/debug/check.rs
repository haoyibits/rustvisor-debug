//! `check <file>`: the engine is `probecore::check`; here it runs against
//! PowerView, with `reg` read through the PER file and `expr` in PRACTICE.

pub use probecore::check::*;

use std::path::Path;

use super::probe::{DResult, PerSnapshot, Probe};
use super::t32target::T32Target;

/// [`probecore::check::load`] with tracebridge's error type.
pub fn load(path: &Path) -> DResult<CheckFile> {
    Ok(probecore::check::load(path)?)
}

/// Run the checks through PowerView.
pub fn run(
    probe: &mut dyn Probe,
    per: &mut PerSnapshot,
    file: &CheckFile,
    options: &Options,
    clocks: Option<&Clocks>,
) -> DResult<Report> {
    Ok(probecore::check::run(
        &mut T32Target::with_per(probe, per),
        file,
        options,
        clocks,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debug::clock;
    use crate::debug::probe::TargetAddress;
    use crate::debug::probe::fake::FakeProbe;
    use crate::style::Style;
    use t32rcl::Value;

    const EXAMPLE: &str = include_str!("../../../../docs/check-example.toml");

    fn per_ready(probe: &mut FakeProbe, running: bool) {
        probe.set("SYStem.Mode()", Value::Int(11));
        probe.set("STATE.RUN()", Value::Bool(running));
        probe.set("PER.FILENAME()", Value::Text("perx.per".into()));
    }

    #[test]
    fn parses_every_read_and_expect_form() {
        let file = parse(
            r#"
            description = "demo"
            [[check]]
            name = "a"
            read = { reg = "CTRL" }
            expect = { eq = 0x1234, mask = 0xFF }
            [[check]]
            name = "b"
            read = { addr = "AD:0x100" }
            expect = { ne = 0 }
            [[check]]
            name = "c"
            read = { core = "PC" }
            expect = { in_symbol = "main" }
            [[check]]
            name = "d"
            read = { expr = "Data.Long(AD:0x0)" }
            expect = { nonzero = true }
            [[check]]
            name = "e"
            read = { reg = "BLOCK.CR" }
            expect = { range = [1, 0x10] }
            [[check]]
            name = "f"
            read = { core = "R0" }
            expect = { one_of = [0, "sym:vectors", "sym:vectors+0x10", "0xFFFFFFFF"] }
            [[check]]
            name = "g"
            read = { core = "LR" }
            expect = { eq = "sym:handler-4" }
            variants = ["x", "y"]
            "#,
        )
        .unwrap();
        assert_eq!(file.description.as_deref(), Some("demo"));
        let expects: Vec<&Expect> = file.checks.iter().map(|c| &c.expect).collect();
        assert_eq!(
            expects,
            [
                &Expect::Eq {
                    value: Operand::Int(0x1234),
                    mask: Some(0xFF)
                },
                &Expect::Ne {
                    value: Operand::Int(0),
                    mask: None
                },
                &Expect::InSymbol("main".into()),
                &Expect::NonZero,
                &Expect::Range(1, 0x10),
                &Expect::OneOf(vec![
                    Operand::Int(0),
                    Operand::Symbol {
                        name: "vectors".into(),
                        offset: 0
                    },
                    Operand::Symbol {
                        name: "vectors".into(),
                        offset: 0x10
                    },
                    Operand::Int(0xFFFF_FFFF),
                ]),
                &Expect::Eq {
                    value: Operand::Symbol {
                        name: "handler".into(),
                        offset: -4
                    },
                    mask: None
                },
            ]
        );
        assert_eq!(file.checks[0].read, ReadSpec::Reg("CTRL".into()));
        assert_eq!(
            file.checks[1].read,
            ReadSpec::Addr(TargetAddress::parse("AD:0x100").unwrap())
        );
        assert_eq!(file.checks[2].read, ReadSpec::Core("PC".into()));
        assert_eq!(
            file.checks[3].read,
            ReadSpec::Expr("Data.Long(AD:0x0)".into())
        );
        assert_eq!(
            file.checks[6].variants,
            Some(vec!["x".to_string(), "y".to_string()])
        );
    }

    #[test]
    fn parse_errors_name_the_check() {
        let error = |text: &str| parse(text).unwrap_err().message;
        assert_eq!(
            error(
                "[[check]]\nname = \"a\"\nread = { reg = \"X\", core = \"PC\" }\nexpect = { nonzero = true }"
            ),
            "check #1 (a): read must have exactly one of reg, addr, core, expr, clock"
        );
        assert_eq!(
            error(
                "[[check]]\nname = \"a\"\nread = { addr = \"0x100\" }\nexpect = { nonzero = true }"
            ),
            "check #1 (a): read.addr \"0x100\" must be <access class>:<address>, e.g. AD:0x20000000"
        );
        assert_eq!(
            error("[[check]]\nname = \"a\"\nread = { core = \"PC\" }\nexpect = { eq = 1, ne = 2 }"),
            "check #1 (a): expect must have exactly one of eq, ne, nonzero, range, in_symbol, one_of (plus mask with eq/ne)"
        );
        assert_eq!(
            error("[[check]]\nname = \"a\"\nread = { core = \"PC\" }\nexpect = { eq = \"sym:\" }"),
            "check #1 (a): expect.eq: invalid symbol \"sym:\""
        );
        assert_eq!(
            error("[[check]]\nname = \"a\"\nread = { core = \"PC\" }\nexpect = { range = [2, 1] }"),
            "check #1 (a): expect.range: low is greater than high"
        );
        assert_eq!(
            error(
                "[[check]]\nname = \"a\"\nread = { core = \"PC\" }\nexpect = { nonzero = true }\ntypo = 1"
            ),
            "check #1 (a): unknown key 'typo'"
        );
        assert_eq!(error("description = \"x\""), "no [[check]] entries");
    }

    #[test]
    fn example_file_parses() {
        let file = parse(EXAMPLE).unwrap();
        assert!(file.checks.len() >= 5);
    }

    fn file(text: &str) -> CheckFile {
        parse(text).unwrap()
    }

    #[test]
    fn variant_selection() {
        let file = file(
            r#"
            [[check]]
            name = "all"
            read = { expr = "1" }
            expect = { eq = 1 }
            [[check]]
            name = "only-a"
            read = { expr = "1" }
            expect = { eq = 1 }
            variants = ["a"]
            [[check]]
            name = "a-or-b"
            read = { expr = "1" }
            expect = { eq = 1 }
            variants = ["a", "b"]
            "#,
        );
        let names = |variant: Option<&str>| -> Vec<&str> {
            file.checks
                .iter()
                .filter(|c| c.selected(variant))
                .map(|c| c.name.as_str())
                .collect()
        };
        assert_eq!(names(None), ["all"]);
        assert_eq!(names(Some("a")), ["all", "only-a", "a-or-b"]);
        assert_eq!(names(Some("b")), ["all", "a-or-b"]);
        assert_eq!(names(Some("c")), ["all"]);

        let mut probe = FakeProbe::with(&[("1", Value::Int(1))]);
        let report = run(
            &mut probe,
            &mut PerSnapshot::default(),
            &file,
            &Options {
                variant: Some("b".into()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(report.results.len(), 2);
        assert_eq!(report.skipped, 1);
        assert_eq!(
            report.summary(),
            "2 passed, 0 failed, 0 errors, 1 skipped (not in variant \"b\")"
        );
    }

    const MIXED: &str = r#"
        [[check]]
        name = "ctrl"
        read = { reg = "CTRL" }
        expect = { eq = 0x30, mask = 0xF0 }
        [[check]]
        name = "word"
        read = { addr = "AD:0x100" }
        expect = { one_of = [1, "sym:table+4"] }
        [[check]]
        name = "pc"
        read = { core = "PC" }
        expect = { in_symbol = "main" }
    "#;

    fn mixed_probe(running: bool) -> FakeProbe {
        let mut probe = FakeProbe::with(&[
            ("PER.ADDRESS(\".CTRL\")", Value::Text("C15:0x10".into())),
            ("ADDRESS.OFFSET(PER.ADDRESS(\".CTRL\"))", Value::Int(4)),
            ("PER.VALUE(\".CTRL\")", Value::Int(0x1234)),
            ("Data.Long(AD:0x100)", Value::Int(0x2004)),
            ("sYmbol.BEGIN(table)", Value::Text("D:0x2000".into())),
            ("sYmbol.BEGIN(main)", Value::Text("P:0x1000".into())),
            ("sYmbol.END(main)", Value::Text("P:0x10FF".into())),
            ("Register(PC)", Value::Int(0x1010)),
        ]);
        per_ready(&mut probe, running);
        probe
    }

    #[test]
    fn passing_checks_exit_zero() {
        let mut probe = mixed_probe(false);
        let report = run(
            &mut probe,
            &mut PerSnapshot::default(),
            &file(MIXED),
            &Options::default(),
            None,
        )
        .unwrap();
        assert!(report.passed(), "{report:#?}");
        assert_eq!(report.exit_code(), 0);
        assert_eq!(
            report.results[1].expect,
            "one of [0x00000001, table+0x4 (0x00002004)]"
        );
        assert_eq!(report.results[2].expect, "in main [0x00001000..0x000010FF]");
        assert_eq!(probe.commands(), ["PER.Set.CONDitions"]);
    }

    #[test]
    fn a_failing_check_exits_non_zero() {
        let mut probe = mixed_probe(false);
        probe.set("Register(PC)", Value::Int(0x2000));
        let report = run(
            &mut probe,
            &mut PerSnapshot::default(),
            &file(MIXED),
            &Options::default(),
            None,
        )
        .unwrap();
        assert_eq!(report.results[2].status, Status::Fail);
        assert_eq!(report.exit_code(), 3);
        let text = report.human(Style::PLAIN);
        assert!(text.contains("FAIL  pc"), "{text}");
        let coloured = report.human(Style::COLOR);
        assert!(coloured.contains("\x1b[1;31mFAIL \x1b[0m"), "{coloured}");
        assert!(coloured.contains("\x1b[32mok   \x1b[0m"), "{coloured}");
        assert!(
            coloured.ends_with("\x1b[1;31m2 passed, 1 failed, 0 errors\x1b[0m"),
            "{coloured}"
        );
        assert_eq!(crate::style::strip(&coloured), text);
    }

    #[test]
    fn unresolved_symbol_is_an_error_not_a_crash() {
        let mut probe = mixed_probe(false);
        probe.values.remove("sYmbol.BEGIN(main)");
        let report = run(
            &mut probe,
            &mut PerSnapshot::default(),
            &file(MIXED),
            &Options::default(),
            None,
        )
        .unwrap();
        assert_eq!(report.results[2].status, Status::Error);
        assert!(
            report.results[2]
                .error
                .as_deref()
                .unwrap()
                .starts_with("symbol main not found")
        );
        assert!(!probe.log.contains(&"fnc Register(PC)".to_string()));
        assert_eq!(report.exit_code(), 3);
    }

    #[test]
    fn cp15_read_while_running_needs_halt() {
        let mut probe = mixed_probe(true);
        let error = run(
            &mut probe,
            &mut PerSnapshot::default(),
            &file(MIXED),
            &Options::default(),
            None,
        )
        .unwrap_err();
        assert_eq!(
            error.message,
            "the core is running, and 2 check(s) read CP15 or core registers, which needs a \
             halted core (ctrl, pc); stop it with 'tracebridge debug break' or pass --halt"
        );
        assert!(!probe.commands().contains(&"Break"));
        assert!(!probe.log.iter().any(|l| l.starts_with("fnc PER.VALUE")));
    }

    #[test]
    fn halt_option_breaks_first() {
        let mut probe = mixed_probe(true);
        let report = run(
            &mut probe,
            &mut PerSnapshot::default(),
            &file(MIXED),
            &Options {
                halt: true,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert!(report.halted);
        assert!(
            report
                .human(Style::PLAIN)
                .contains("core halted for CP15/core register reads")
        );
        let commands = probe.commands();
        assert_eq!(commands[0], "PER.Set.CONDitions");
        assert_eq!(commands[1], "Break");
        let brk = probe.log.iter().position(|l| l == "cmd Break").unwrap();
        let first_read = probe
            .log
            .iter()
            .position(|l| l == "fnc PER.VALUE(\".CTRL\")")
            .unwrap();
        assert!(brk < first_read);
    }

    #[test]
    fn plain_memory_checks_do_not_need_a_halted_core() {
        let mut probe = mixed_probe(true);
        let file =
            file("[[check]]\nname = \"w\"\nread = { addr = \"AD:0x100\" }\nexpect = { ne = 0 }");
        let report = run(
            &mut probe,
            &mut PerSnapshot::default(),
            &file,
            &Options::default(),
            None,
        )
        .unwrap();
        assert!(report.passed());
        assert!(probe.commands().is_empty());
    }

    #[test]
    fn dry_run_resolves_without_reading() {
        let mut probe = mixed_probe(true);
        let report = run(
            &mut probe,
            &mut PerSnapshot::default(),
            &file(MIXED),
            &Options {
                dry_run: true,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert!(report.passed());
        assert_eq!(report.results[0].read, "reg .CTRL (C15:0x0001)");
        assert!(report.results.iter().all(|r| r.value.is_none()));
        let reads: Vec<&String> = probe
            .log
            .iter()
            .filter(|l| {
                l.starts_with("fnc PER.VALUE")
                    || l.starts_with("fnc Data.Long")
                    || l.starts_with("fnc Register")
                    || l.starts_with("read ")
            })
            .collect();
        assert!(reads.is_empty(), "{reads:?}");
        assert!(
            report
                .summary()
                .starts_with("dry run: 3 resolved, 0 errors")
        );
    }

    /// OSC (board input) and IRC (16 MHz) feed PLL through SEL[0]; PLL runs
    /// at 4 x its source, CORE from PLL with a warning while LOCK[0] is 0.
    const CLOCK_TREE: &str = r#"
        chips = ["DEMO"]
        [reg]
        SEL = "AD:0x100"
        LOCK = "AD:0x104"
        [[clock]]
        name = "OSC"
        [[clock]]
        name = "IRC"
        hz = "16MHz"
        [[clock]]
        name = "PLL"
        select = "SEL[0]"
        sources = { "0" = "IRC", "1" = "OSC" }
        mul = "4"
        [[clock]]
        name = "CORE"
        from = "PLL"
        warn = [{ when = "LOCK[0] == 0", text = "not locked" }]
        [[clock]]
        name = "OFF"
        from = "IRC"
        enable = "0"
    "#;

    const CLOCK_CHECKS: &str = r#"
        [[check]]
        name = "pll"
        read = { clock = "PLL" }
        expect = { hz = 40_000_000, source = "OSC" }
        [[check]]
        name = "core"
        read = { clock = "CORE" }
        expect = { state = "on", warnings = [] }
        [[check]]
        name = "off"
        read = { clock = "OFF" }
        expect = { state = "off" }
        [[check]]
        name = "pll from irc"
        read = { clock = "PLL" }
        expect = { source = "IRC" }
        [[check]]
        name = "ctrl"
        read = { addr = "AD:0x100" }
        expect = { eq = 1 }
    "#;

    #[test]
    fn clock_checks_parse_and_reject_mixed_forms() {
        let parsed = file(CLOCK_CHECKS);
        assert_eq!(parsed.checks[0].read, ReadSpec::Clock("PLL".into()));
        assert_eq!(
            parsed.checks[0].expect,
            Expect::Clock(ClockExpect {
                hz: Some(Hz(40e6)),
                source: Some("OSC".into()),
                ..Default::default()
            })
        );
        assert!(parsed.reads_clocks(None));
        let error = |text: &str| parse(text).unwrap_err().message;
        let check = |read: &str, expect: &str| {
            format!("[[check]]\nname = \"x\"\nread = {read}\nexpect = {expect}\n")
        };
        assert!(
            error(&check(r#"{ clock = "PLL" }"#, "{ eq = 1 }")).ends_with(
                "unknown expectation 'eq' for read.clock (expected state, hz, source, warnings)"
            )
        );
        assert!(
            error(&check(r#"{ addr = "AD:0x0" }"#, "{ hz = 1 }"))
                .ends_with("expect.hz only works with read.clock")
        );
        assert!(error(&check(r#"{ clock = "PLL" }"#, "{}")).ends_with(
            "expect must have at least one of state, hz, source, warnings for read.clock"
        ));
        assert!(
            error(&check(
                r#"{ clock = "PLL" }"#,
                r#"{ state = "off", hz = 1 }"#
            ))
            .ends_with("expect.hz with state \"off\"")
        );
        assert!(
            error(&check(r#"{ clock = "PLL" }"#, r#"{ state = "maybe" }"#))
                .ends_with("expect.state must be \"on\" or \"off\"")
        );
    }

    #[test]
    fn clock_checks_judge_the_evaluated_tree() {
        let tree = clock::parse(CLOCK_TREE).unwrap();
        let inputs = vec![("OSC".to_string(), 10e6)];
        let clocks = Clocks {
            tree: &tree,
            inputs: &inputs,
        };
        let mut probe = FakeProbe::with(&[
            ("Data.Long(AD:0x100)", Value::Int(1)),
            ("Data.Long(AD:0x104)", Value::Int(0)),
            ("STATE.RUN()", Value::Bool(true)),
        ]);
        let report = run(
            &mut probe,
            &mut PerSnapshot::default(),
            &file(CLOCK_CHECKS),
            &Options::default(),
            Some(&clocks),
        )
        .unwrap();
        let status: Vec<(&str, Status)> = report
            .results
            .iter()
            .map(|r| (r.name.as_str(), r.status))
            .collect();
        assert_eq!(
            status,
            [
                ("pll", Status::Pass),
                ("core", Status::Fail),
                ("off", Status::Pass),
                ("pll from irc", Status::Fail),
                ("ctrl", Status::Pass),
            ]
        );
        assert_eq!(report.results[0].value, Some(40_000_000));
        assert_eq!(
            report.results[0].text.as_deref(),
            Some("on 40 MHz from OSC")
        );
        assert_eq!(
            report.results[1].text.as_deref(),
            Some("on 40 MHz from PLL ! not locked")
        );
        assert_eq!(report.results[0].expect, "40 MHz, from OSC");
        assert_eq!(report.results[1].expect, "on, no warnings");
        let json = report.to_json();
        assert_eq!(json["results"][0]["hex"], serde_json::Value::Null);
        assert_eq!(json["results"][0]["text"], "on 40 MHz from OSC");
        assert_eq!(json["results"][4]["hex"], "0x00000001");
        // The tree is read once for all clock checks: one read per register.
        let reads = probe
            .log
            .iter()
            .filter(|line| line.as_str() == "fnc Data.Long(AD:0x100)")
            .count();
        assert_eq!(reads, 2, "{:?}", probe.log); // tree + the addr check
        let text = report.human(Style::PLAIN);
        assert!(
            text.contains("FAIL  core          on 40 MHz from PLL ! not locked  on, no warnings"),
            "{text}"
        );

        let unknown = file(
            "[[check]]\nname = \"x\"\nread = { clock = \"NOPE\" }\nexpect = { state = \"on\" }\n",
        );
        let report = run(
            &mut probe,
            &mut PerSnapshot::default(),
            &unknown,
            &Options::default(),
            Some(&clocks),
        )
        .unwrap();
        assert_eq!(
            report.results[0].error.as_deref(),
            Some("clock NOPE is not in the clock tree description")
        );
    }
}
