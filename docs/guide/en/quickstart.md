# From installation to your first flash

[中文](../cn/quickstart.md)

Follow one SR6P6 workflow: start with successfully built ELF images, connect the board and probe, generate configuration, flash and inspect the result. rvdbg does not compile firmware.

## Install rvdbg and its backend

On Apple Silicon macOS or Linux, use Homebrew:

```sh
brew install haoyibits/tap/rvdbg
rvdbg --version
```

The formula also installs `openocd-stellar`. TRACE32 users must separately install PowerView and `t32debugadapter`.

Alternatively, install a release binary:

```sh
curl -fsSL https://github.com/haoyibits/rustvisor-debug/releases/latest/download/install.sh | sh
```

The script defaults to `~/.local/bin`, which must be on `PATH`. Set `RVDBG_INSTALL_DIR` for another directory or `RVDBG_VERSION` for a specific release. It does not install `openocd-stellar`. To install from source, run in this repository:

```sh
cargo install --path crates/rustvisor-debug
```

If Gatekeeper blocks a macOS binary downloaded in a browser, verify its release source, then remove quarantine from the actual binary:

```sh
xattr -d com.apple.quarantine /path/to/rvdbg
```

## Prepare the probe

**TRACE32:** PowerView is discovered under `~/t32` by default; set `trace32.sys` for another installation. Remote API must use TCP. An existing `config.t32` can contain:

```text
RCL=NETTCP
PORT=20000
REMOTEHOSTDENY
```

When no `RCL=` is defined, rvdbg supplies TCP Remote API startup arguments restricted to local access. If `RCL=` is already present, its protocol and port must match `trace32.rcl_port`. Remote API has no authentication and allows PRACTICE and host commands; retain the local access restriction.

**StellarLINK:** use `openocd-stellar`, rather than an OpenOCD build without Stellar support. The default SR6P6 EVB script is `board/sr6p6_evb.cfg`. VS Code needs Cortex-Debug; direct GDB use needs `arm-none-eabi-gdb`.

## Generate configuration in the firmware project

Run these commands in the project being debugged, rather than rvdbg's own source tree:

```sh
cd ~/work/rustvisor-sdk
rvdbg init
$EDITOR rvdbg.toml
```

`init` refuses to overwrite an existing file. Check the generated SR6P6 `[project]` defaults:

```toml
[project]
program = "boot_diagnostic"
elf = "target/thumbv8r-none-eabihf/release/boot_diagnostic"
extra_elf = ["target/thumbv8r-none-eabihf/release/scenario-guest"]
```

Replace paths with your actual build products. Use `extra_elf = []` for no extra images. Declare hypervisor and Guest images together; see [multiple-image flashing](flashing.md).

## Check what will be operated on

```sh
rvdbg config
```

Check the configuration path, image existence, detected probes and selected backend. With both probes connected TRACE32 wins by default. To select StellarLINK for this invocation:

```sh
rvdbg --backend openocd config
```

A successful `config` check does not validate target power, JTAG or Flash operations. See [configuration](configuration.md) for backend and core selection.

## Flash and inspect

After confirming the images and board:

```sh
rvdbg flash
rvdbg verify
rvdbg status
```

`flash` erases/writes and resets the target, running it after normal completion. Without an explicit path, `verify` checks every configured image; exit code 0 means the loadable contents match. `status` reports running/halted state and PC/CPSR when halted.

A content match does not prove successful boot or correct application behavior. Continue with [RTT](channels.md) or [project checks](inspection.md). On failure, use [troubleshooting](troubleshooting.md); skipping verification or enabling OTP writes is not a general fix.

Run `rvdbg vscode` for IDE configuration or bare `rvdbg` for an [interactive session](debugging.md).
