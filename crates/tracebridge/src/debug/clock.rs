//! `clock`: the engine is `probecore::clock`; here it reads the registers
//! through PowerView, and the descriptions live in tracebridge's library.

pub use probecore::clock::*;

use std::path::{Path, PathBuf};

use super::probe::{DResult, Probe};
use super::t32target::T32Target;
use crate::pycompat::Env;

/// `~/.config/rvdbg/clock`, honouring `XDG_CONFIG_HOME`.
pub fn library_dir(env: &Env) -> PathBuf {
    crate::flash::config_dir(env).join("clock")
}

/// [`probecore::clock::load`] with tracebridge's error type.
pub fn load(path: &Path) -> DResult<Tree> {
    Ok(probecore::clock::load(path)?)
}

/// [`probecore::clock::find`] with tracebridge's error type.
pub fn find(chip: &str, directory: &Path) -> DResult<(PathBuf, Tree)> {
    Ok(probecore::clock::find(chip, directory)?)
}

/// [`probecore::clock::parse_inputs`] with tracebridge's error type.
pub fn parse_inputs(arguments: &[String]) -> DResult<Vec<(String, f64)>> {
    Ok(probecore::clock::parse_inputs(arguments)?)
}

/// Read the registers with `Data.Long()` and compute every clock.
pub fn evaluate(probe: &mut dyn Probe, tree: &Tree, inputs: &[(String, f64)]) -> DResult<Report> {
    let mut report = probecore::clock::evaluate(&mut T32Target::new(probe), tree, inputs)?;
    report.inputs_hint = format!("[clock] in {}", crate::ui::config_file());
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debug::probe::fake::FakeProbe;
    use crate::style::{Style, strip};

    const EXAMPLE: &str = include_str!("../../../../docs/clock-example.toml");

    /// The registers of docs/clock-example.toml with the PLL running from the
    /// crystal: 8 MHz / 2 x 50 = 200 MHz.
    fn example_probe() -> FakeProbe {
        FakeProbe::with(&[
            // PLLON, XOSCON, IRCON.
            ("Data.Long(AD:0x40001000)", t32rcl::Value::Int(0x0000_0007)),
            // LOCK.
            ("Data.Long(AD:0x40001004)", t32rcl::Value::Int(0x0000_0004)),
            // PREDIV 2, MFD 50.
            ("Data.Long(AD:0x40001008)", t32rcl::Value::Int(0x0000_2032)),
            // PLL source = XOSC (bit 24), system clock = PLL (bits 1:0 = 2).
            ("Data.Long(AD:0x4000100C)", t32rcl::Value::Int(0x0100_0002)),
            // Bus divider enabled, DIV = 3; timer divider disabled.
            ("Data.Long(AD:0x40001010)", t32rcl::Value::Int(0x8003_0000)),
            ("Data.Long(AD:0x40001014)", t32rcl::Value::Int(0x0001_0000)),
        ])
    }

    fn report(probe: &mut FakeProbe, inputs: &[(&str, f64)]) -> Report {
        let tree = parse(EXAMPLE).unwrap();
        let inputs: Vec<(String, f64)> = inputs
            .iter()
            .map(|(name, hz)| (name.to_string(), *hz))
            .collect();
        evaluate(probe, &tree, &inputs).unwrap()
    }

    #[test]
    fn example_tree_with_the_crystal_frequency() {
        let mut probe = example_probe();
        let report = report(&mut probe, &[("XOSC", 8e6)]);
        assert_eq!(
            report.human(Style::PLAIN),
            "\
IRC             16 MHz   nominal  ; internal RC oscillator
XOSC            8 MHz    given  ; crystal: the board decides
└─ PLL          200 MHz  CLKSEL[24]=1 x50 /2
   └─ SYSCLK    200 MHz  CLKSEL[1:0]=2  ; core clock
      ├─ BUS    50 MHz   /4  ; peripheral bus
      └─ TIMER  off      (TIMDIV[31] = 0)"
        );
        assert_eq!(report.exit_code(), 0);
        // Every register once, nothing else.
        assert_eq!(probe.log.len(), 6);
        assert!(
            probe
                .log
                .iter()
                .all(|line| line.starts_with("fnc Data.Long(AD:0x400010"))
        );
        let json = report.to_json();
        assert_eq!(json["clocks"][2]["name"], "PLL");
        assert_eq!(json["clocks"][2]["source"], "XOSC");
        assert_eq!(json["clocks"][2]["hz"], 200e6);
        assert_eq!(json["clocks"][5]["state"], "off");
        let registers = json["registers"].as_array().unwrap();
        let pll = registers.iter().find(|r| r["name"] == "PLLDV").unwrap();
        assert_eq!(pll["address"], "AD:0x40001008");
        assert_eq!(pll["hex"], "0x00002032");
    }

    #[test]
    fn a_missing_input_leaves_its_clocks_unknown() {
        let mut probe = example_probe();
        let report = report(&mut probe, &[]);
        let text = report.human(Style::PLAIN);
        assert!(
            text.contains("XOSC            ?       (frequency not given)"),
            "{text}"
        );
        assert!(
            text.contains("└─ PLL          ?       CLKSEL[24]=1 x50 /2  (XOSC is unknown)"),
            "{text}"
        );
        assert!(
            text.ends_with(
                "XOSC not given: pass XOSC=<frequency> (e.g. XOSC=40MHz) or set it under \
                 [clock] in trace32.toml"
            ),
            "{text}"
        );
        assert_eq!(report.missing, ["XOSC"]);
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn the_tree_follows_the_selectors() {
        let mut probe = example_probe();
        // PLL from the IRC, system clock straight from the crystal, PLL unlocked.
        probe.set("Data.Long(AD:0x4000100C)", t32rcl::Value::Int(0x0000_0001));
        probe.set("Data.Long(AD:0x40001004)", t32rcl::Value::Int(0));
        let report = report(&mut probe, &[("xosc", 8e6)]);
        assert_eq!(
            report.human(Style::PLAIN),
            "\
IRC          16 MHz   nominal  ; internal RC oscillator
└─ PLL       400 MHz  CLKSEL[24]=0 x50 /2  ! not locked
XOSC         8 MHz    given  ; crystal: the board decides
└─ SYSCLK    8 MHz    CLKSEL[1:0]=1  ; core clock
   ├─ BUS    2 MHz    /4  ; peripheral bus
   └─ TIMER  off      (TIMDIV[31] = 0)"
        );
    }

    #[test]
    fn off_unknown_sources_and_read_errors() {
        let mut probe = example_probe();
        // PLL off, system clock stopped.
        probe.set("Data.Long(AD:0x40001000)", t32rcl::Value::Int(0x3));
        probe.set("Data.Long(AD:0x4000100C)", t32rcl::Value::Int(0x0100_0003));
        probe.values.remove("Data.Long(AD:0x40001010)");
        probe.errors.insert(
            "Data.Long(AD:0x40001010)".into(),
            "bus error at address AD:0x40001010".into(),
        );
        let report = report(&mut probe, &[("XOSC", 8e6)]);
        let text = report.human(Style::PLAIN);
        assert!(
            text.contains("└─ PLL    off     CLKSEL[24]=1  (CTL[2] = 0)"),
            "{text}"
        );
        assert!(
            text.contains("SYSCLK    off     CLKSEL[1:0]=3  (CLKSEL[1:0] != 3 is false)"),
            "{text}"
        );
        assert!(
            text.contains("├─ BUS    error   BUSDIV[31]: cannot read BUSDIV (AD:0x40001010): "),
            "{text}"
        );
        // A divider under a clock that is off only says so.
        assert!(
            text.contains("└─ TIMER  off     (TIMDIV[31] = 0)"),
            "{text}"
        );
        assert_eq!(report.exit_code(), 1);
        // A clock without a known source is listed at the top level.
        assert!(text.lines().any(|line| line.starts_with("SYSCLK")));
    }

    #[test]
    fn a_selection_that_is_not_described_is_unknown() {
        let tree = parse(
            "[reg]\nSEL = \"AD:0x10\"\nDIV = \"AD:0x14\"\n\
             [[clock]]\nname = \"OSC\"\nhz = 8000000\n\
             [[clock]]\nname = \"MUX\"\nselect = \"SEL[3:0]\"\nsources = { \"0\" = \"OSC\" }\n\
             [[clock]]\nname = \"OUT\"\nfrom = \"MUX\"\ndiv = \"DIV[3:0]\"\n",
        )
        .unwrap();
        let mut probe = FakeProbe::with(&[
            ("Data.Long(AD:0x10)", t32rcl::Value::Int(0xB)),
            ("Data.Long(AD:0x14)", t32rcl::Value::Int(4)),
        ]);
        let report = evaluate(&mut probe, &tree, &[]).unwrap();
        assert_eq!(
            report.human(Style::PLAIN),
            "\
OSC     8 MHz  nominal
MUX     ?      SEL[3:0]=11  (this source is not described)
└─ OUT  ?      /4  (MUX is unknown)"
        );
        // Not a missing input: there is nothing to pass.
        assert!(report.missing.is_empty());
        assert_eq!(report.exit_code(), 0);

        // A divider field of 0 stops the clock instead of dividing by zero.
        probe.set("Data.Long(AD:0x10)", t32rcl::Value::Int(0));
        probe.set("Data.Long(AD:0x14)", t32rcl::Value::Int(0));
        let report = evaluate(&mut probe, &tree, &[]).unwrap();
        assert!(
            report
                .human(Style::PLAIN)
                .ends_with("   └─ OUT  off    (DIV[3:0] = 0)"),
            "{}",
            report.human(Style::PLAIN)
        );
    }

    #[test]
    fn inputs_must_name_a_source_clock() {
        let mut probe = example_probe();
        let tree = parse(EXAMPLE).unwrap();
        let error = evaluate(&mut probe, &tree, &[("PLL".into(), 1e6)])
            .err()
            .unwrap();
        assert_eq!(
            error.message,
            "PLL is not a source clock of this tree; the sources are IRC, XOSC"
        );
        assert!(probe.log.is_empty());
        assert_eq!(
            parse_inputs(&["XOSC=40MHz".into(), "IRC=16e6".into()]).unwrap(),
            [("XOSC".to_string(), 40e6), ("IRC".to_string(), 16e6)]
        );
        assert_eq!(
            parse_inputs(&["XOSC".into()]).unwrap_err().message,
            "\"XOSC\" must be <clock>=<frequency>, e.g. XOSC=40MHz"
        );
    }

    #[test]
    fn a_selector_loop_is_an_error_not_a_hang() {
        let tree = parse(
            "[reg]\nR = \"AD:0x0\"\n\
             [[clock]]\nname = \"A\"\nselect = \"R[0]\"\nsources = { \"0\" = \"B\" }\n\
             [[clock]]\nname = \"B\"\nfrom = \"A\"\n",
        )
        .unwrap();
        let mut probe = FakeProbe::with(&[("Data.Long(AD:0x0)", t32rcl::Value::Int(0))]);
        let report = evaluate(&mut probe, &tree, &[]).unwrap();
        assert_eq!(report.exit_code(), 0);
        assert!(
            report
                .clocks
                .iter()
                .all(|clock| clock.state == State::Unknown("A failed".into())
                    || clock.state == State::Unknown("B is unknown".into())),
            "{:?}",
            report.clocks
        );
    }

    #[test]
    fn colours_keep_the_columns() {
        let mut probe = example_probe();
        probe.set("Data.Long(AD:0x40001004)", t32rcl::Value::Int(0));
        let report = report(&mut probe, &[]);
        let coloured = report.human(Style::COLOR);
        assert_eq!(strip(&coloured), report.human(Style::PLAIN));
        assert!(
            coloured.contains("\x1b[36mIRC           \x1b[0m  \x1b[1m16 MHz\x1b[0m"),
            "{coloured}"
        );
        assert!(coloured.contains("\x1b[2moff   \x1b[0m"), "{coloured}");
        // The branch lines recede; the name keeps its colour.
        assert!(
            coloured.contains("\x1b[2m      └─ \x1b[0m\x1b[36mTIMER\x1b[0m"),
            "{coloured}"
        );
        assert!(coloured.contains("\x1b[33m?     \x1b[0m"), "{coloured}");
    }
}
