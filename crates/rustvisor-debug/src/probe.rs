//! Which probe is connected, and so which backend runs a command.
//!
//! - StellarLINK: the USB IDs of openocd-stellar's
//!   `interface/ftdi/stellarlink.cfg` (`ftdi vid_pid 0x0483 0x0499 0x263d 0x4001`).
//! - Lauterbach: USB vendor 0x0897 (the vendor ID in the TRACE32 manual's
//!   `SYStem.CONFIG.USB.SET ... /VID 0x897` example), or a PowerView already
//!   listening on the RCL port (an Ethernet probe has no USB device, and the
//!   PowerView toolbar runs rvdbg).
//! - Both: TRACE32 (the user's choice, 2026-11-06); the StellarLINK is named
//!   as ignored.
//!
//! The USB list comes from the system without any unsafe code: sysfs on
//! Linux, `ioreg` on macOS. [`choose`] works on a given list, so it is
//! tested without hardware.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::time::Duration;

use crate::config::{Backend, Config};

/// `(vendor, product)` of StellarLINK.
pub const STELLARLINK: &[(u16, u16)] = &[(0x0483, 0x0499), (0x263d, 0x4001)];
pub const LAUTERBACH_VENDOR: u16 = 0x0897;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDevice {
    pub vendor: u16,
    pub product: u16,
    pub name: String,
}

impl std::fmt::Display for UsbDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04x}:{:04x}", self.vendor, self.product)?;
        if !self.name.is_empty() {
            write!(f, " {}", self.name)?;
        }
        Ok(())
    }
}

/// What was found, for messages and `rvdbg config`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    StellarLink(UsbDevice),
    Lauterbach(UsbDevice),
    PowerView(u16),
}

impl Found {
    pub fn backend(&self) -> Backend {
        match self {
            Found::StellarLink(_) => Backend::OpenOcd,
            Found::Lauterbach(_) | Found::PowerView(_) => Backend::Trace32,
        }
    }
}

impl std::fmt::Display for Found {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Found::StellarLink(device) => write!(f, "StellarLINK (USB {device})"),
            Found::Lauterbach(device) => write!(f, "Lauterbach probe (USB {device})"),
            Found::PowerView(port) => write!(f, "PowerView on RCL port {port}"),
        }
    }
}

/// The probes among the USB devices, plus a running PowerView.
pub fn classify(devices: &[UsbDevice], powerview_port: Option<u16>) -> Vec<Found> {
    let mut found = Vec::new();
    for device in devices {
        if STELLARLINK.contains(&(device.vendor, device.product)) {
            found.push(Found::StellarLink(device.clone()));
        } else if device.vendor == LAUTERBACH_VENDOR {
            found.push(Found::Lauterbach(device.clone()));
        }
    }
    if let Some(port) = powerview_port {
        found.push(Found::PowerView(port));
    }
    found
}

/// The backend and a sentence saying why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub backend: Backend,
    pub reason: String,
}

fn list(found: &[&Found]) -> String {
    found
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `fixed` is `--backend` or `project.backend`; it always wins.
pub fn choose(fixed: Option<(Backend, &str)>, found: &[Found]) -> Result<Choice, String> {
    if let Some((backend, source)) = fixed {
        return Ok(Choice {
            backend,
            reason: format!("{} ({source})", backend.name()),
        });
    }
    let trace32: Vec<&Found> = found
        .iter()
        .filter(|item| item.backend() == Backend::Trace32)
        .collect();
    let stellarlink: Vec<&Found> = found
        .iter()
        .filter(|item| item.backend() == Backend::OpenOcd)
        .collect();
    match (trace32.is_empty(), stellarlink.is_empty()) {
        (false, true) => Ok(Choice {
            backend: Backend::Trace32,
            reason: format!("trace32: found {}", list(&trace32)),
        }),
        (true, false) => Ok(Choice {
            backend: Backend::OpenOcd,
            reason: format!("openocd: found {}", list(&stellarlink)),
        }),
        (false, false) => Ok(Choice {
            backend: Backend::Trace32,
            reason: format!(
                "trace32: found {}; ignoring {} (--backend openocd uses it)",
                list(&trace32),
                list(&stellarlink)
            ),
        }),
        (true, true) => Err(
            "no probe found: connect a StellarLINK or a Lauterbach probe (or start PowerView), \
             or pass --backend trace32|openocd"
                .into(),
        ),
    }
}

/// The RCL port PowerView would listen on: `T32_RCL_PORT`, else
/// `trace32.rcl_port`, else 20000 (tracebridge's order).
pub fn rcl_port(config: &Config) -> u16 {
    if let Some(port) = std::env::var("T32_RCL_PORT")
        .ok()
        .and_then(|text| text.trim().parse().ok())
    {
        return port;
    }
    config
        .trace32
        .get("trace32")
        .and_then(|table| table.get("rcl_port"))
        .and_then(toml::Value::as_integer)
        .and_then(|port| u16::try_from(port).ok())
        .unwrap_or(20000)
}

pub fn listening(port: u16) -> bool {
    let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&address, Duration::from_millis(300)).is_ok()
}

/// Look at the connected probes now.
pub fn detect(config: &Config) -> Result<Vec<Found>, String> {
    let devices = usb_devices()?;
    let port = rcl_port(config);
    Ok(classify(&devices, listening(port).then_some(port)))
}

#[cfg(target_os = "linux")]
pub fn usb_devices() -> Result<Vec<UsbDevice>, String> {
    let root = std::path::Path::new("/sys/bus/usb/devices");
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => {
            return Err(format!(
                "cannot list USB devices in {}: {error}",
                root.display()
            ));
        }
    };
    let mut devices = Vec::new();
    for entry in entries.flatten() {
        let read = |name: &str| std::fs::read_to_string(entry.path().join(name)).ok();
        let hex =
            |name: &str| read(name).and_then(|text| u16::from_str_radix(text.trim(), 16).ok());
        if let (Some(vendor), Some(product)) = (hex("idVendor"), hex("idProduct")) {
            let name = read("product").unwrap_or_default().trim().to_string();
            devices.push(UsbDevice {
                vendor,
                product,
                name,
            });
        }
    }
    Ok(devices)
}

#[cfg(target_os = "macos")]
pub fn usb_devices() -> Result<Vec<UsbDevice>, String> {
    let output = std::process::Command::new("/usr/sbin/ioreg")
        .args(["-p", "IOUSB", "-l", "-w0"])
        .output()
        .map_err(|error| format!("cannot list USB devices (ioreg): {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cannot list USB devices: ioreg exited with {}",
            output.status
        ));
    }
    Ok(parse_ioreg(&String::from_utf8_lossy(&output.stdout)))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn usb_devices() -> Result<Vec<UsbDevice>, String> {
    Ok(Vec::new())
}

/// The devices of `ioreg -p IOUSB -l -w0`: every `+-o` line starts one,
/// its `"idVendor"`, `"idProduct"` and `"USB Product Name"` follow.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn parse_ioreg(text: &str) -> Vec<UsbDevice> {
    #[derive(Default)]
    struct Partial {
        vendor: Option<u16>,
        product: Option<u16>,
        name: String,
    }
    fn finish(partial: Partial, devices: &mut Vec<UsbDevice>) {
        if let (Some(vendor), Some(product)) = (partial.vendor, partial.product) {
            devices.push(UsbDevice {
                vendor,
                product,
                name: partial.name,
            });
        }
    }
    let mut devices = Vec::new();
    let mut current: Option<Partial> = None;
    for line in text.lines() {
        if line.contains("+-o ") {
            if let Some(partial) = current.take() {
                finish(partial, &mut devices);
            }
            current = Some(Partial::default());
            continue;
        }
        let Some(partial) = current.as_mut() else {
            continue;
        };
        let Some((key, value)) = line.split_once(" = ") else {
            continue;
        };
        let key = key.trim().trim_start_matches('|').trim();
        let value = value.trim();
        match key {
            "\"idVendor\"" => partial.vendor = value.parse().ok(),
            "\"idProduct\"" => partial.product = value.parse().ok(),
            "\"USB Product Name\"" => partial.name = value.trim_matches('"').to_string(),
            _ => {}
        }
    }
    if let Some(partial) = current {
        finish(partial, &mut devices);
    }
    devices
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usb(vendor: u16, product: u16) -> UsbDevice {
        UsbDevice {
            vendor,
            product,
            name: String::new(),
        }
    }

    #[test]
    fn one_probe_picks_its_backend() {
        let stellar = classify(&[usb(0x05e3, 0x0610), usb(0x0483, 0x0499)], None);
        let choice = choose(None, &stellar).unwrap();
        assert_eq!(choice.backend, Backend::OpenOcd);
        assert_eq!(choice.reason, "openocd: found StellarLINK (USB 0483:0499)");
        assert_eq!(
            choose(None, &classify(&[usb(0x263d, 0x4001)], None))
                .unwrap()
                .backend,
            Backend::OpenOcd
        );
        let lauterbach = classify(&[usb(0x0897, 0x0005)], None);
        assert_eq!(choose(None, &lauterbach).unwrap().backend, Backend::Trace32);
        // An Ethernet probe: only PowerView shows.
        let powerview = classify(&[], Some(20000));
        let choice = choose(None, &powerview).unwrap();
        assert_eq!(choice.backend, Backend::Trace32);
        assert_eq!(choice.reason, "trace32: found PowerView on RCL port 20000");
    }

    #[test]
    fn both_probes_prefer_trace32() {
        let found = classify(&[usb(0x0483, 0x0499), usb(0x0897, 0x0005)], Some(20000));
        let choice = choose(None, &found).unwrap();
        assert_eq!(choice.backend, Backend::Trace32);
        assert!(
            choice
                .reason
                .contains("ignoring StellarLINK (USB 0483:0499)"),
            "{}",
            choice.reason
        );
    }

    #[test]
    fn nothing_found_is_an_error_and_a_fixed_backend_wins() {
        let error = choose(None, &classify(&[usb(0x05e3, 0x0610)], None)).unwrap_err();
        assert!(error.starts_with("no probe found"), "{error}");
        let found = classify(&[usb(0x0897, 0x0005)], None);
        let choice = choose(Some((Backend::OpenOcd, "--backend")), &found).unwrap();
        assert_eq!(choice.backend, Backend::OpenOcd);
        assert_eq!(choice.reason, "openocd (--backend)");
        assert_eq!(
            choose(Some((Backend::Trace32, "rvdbg.toml")), &[])
                .unwrap()
                .backend,
            Backend::Trace32
        );
    }

    #[test]
    fn ioreg_output_is_parsed() {
        let text = r#"+-o Root  <class IORegistryEntry, id 0x100000100, retain 31>
  +-o USB2.1 Hub@01100000  <class IOUSBHostDevice, id 0x1000a1e5>
  | {
  |   "idProduct" = 1552
  |   "USB Product Name" = "USB2.1 Hub"
  |   "idVendor" = 1507
  | }
  |
  +-o StellarLINK@01120000  <class IOUSBHostDevice, id 0x1000a2b1>
      {
        "USB Product Name" = "StellarLINK"
        "idProduct" = 1177
        "idVendor" = 1155
      }
"#;
        let devices = parse_ioreg(text);
        assert_eq!(
            devices,
            [
                UsbDevice {
                    vendor: 0x05e3,
                    product: 0x0610,
                    name: "USB2.1 Hub".into()
                },
                UsbDevice {
                    vendor: 0x0483,
                    product: 0x0499,
                    name: "StellarLINK".into()
                },
            ]
        );
    }

    #[test]
    fn the_rcl_port_comes_from_the_configuration() {
        let config = crate::config::parse(
            "[project]\nelf = \"a\"\n[trace32]\nrcl_port = 20123\n",
            std::path::Path::new("/p/rvdbg.toml"),
            std::path::Path::new("/p"),
        )
        .unwrap();
        if std::env::var_os("T32_RCL_PORT").is_none() {
            assert_eq!(rcl_port(&config), 20123);
        }
    }
}
