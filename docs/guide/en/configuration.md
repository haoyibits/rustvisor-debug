# Configure a project and select a backend

[中文](../cn/configuration.md)

Configuration determines the images, backend and core an operation uses. After changes, run `rvdbg config` before commands that change target state.

## Configuration discovery and paths

rvdbg searches upward from the current directory for the nearest `rvdbg.toml`. Its directory is the project root. Select another file explicitly:

```sh
rvdbg --config /work/firmware/rvdbg.toml config
```

Project-relative paths, including image paths, are based on the configuration directory. OpenOCD `config` entries are script names resolved by OpenOCD, such as `board/sr6p6_evb.cfg`; add script directories through `search`. Bare executable names in `executable`/`gdb` are searched on `PATH`.

Use `rvdbg init` for the full commented template. Unknown sections and keys are rejected; remove obsolete fields.

## Backend selection

The first explicit choice wins:

1. `--backend trace32` or `--backend openocd` on the command line.
2. `[project] backend = "trace32"` or `"openocd"`.
3. Probe discovery: Lauterbach or a running PowerView selects TRACE32; StellarLINK selects OpenOCD; both selects TRACE32.

```sh
rvdbg --backend openocd status
rvdbg --backend trace32 status
```

An explicit backend does not make a disconnected probe or unavailable server usable. TRACE32 inspection needs PowerView already running. OpenOCD inspection reuses an existing Tcl service or starts an instance without resetting the target. See [debugging](debugging.md) for session lifetimes.

## Fields to check first

| Section | Fields | Purpose |
| --- | --- | --- |
| `[project]` | `elf`, `extra_elf`, `program`, `backend` | Images, TRACE32 symbol program and backend |
| `[trace32]` | `sys`, `config`, `host`, `rcl_port` | Installation, startup configuration and Remote API |
| `[trace32.target]` | `cpu`, `cores`, `mem_access`, `jtag_clock`, `dual_port` | Target and runtime access |
| `[trace32.flash]` | `chip`, `script`, `args` | Flash script selection and arguments |
| `[trace32.rtos]` | `config`, `menu`, `show_tasks` | Optional RTOS awareness |
| `[openocd]` | `executable`, `config`, `search`, `commands` | OpenOCD and board initialization |
| `[openocd]` | `tcl_port`, `gdb_port`, `gdb_target`, `core` | Control connection, GDB core and inspection core |
| `[openocd]` | `memory_ap`, `timeout`, `otp` | Physical memory access, operation timeout, OTP ranges |
| `[rtt]` | `symbol`, `control_block_address`, `poll_interval` | Control block and polling |
| `[fdx]` | `symbol` | Link symbol, default `RUSTVISOR_FDX` |
| `[clock]` | e.g. `XOSC = "40MHz"` | Board frequencies not encoded in registers |

An absent/empty `program` is derived from the primary ELF filename. It identifies a TRACE32 symbol program, not an image path. `trace32.host` is a host platform directory within the TRACE32 installation, not a Remote API network address; normally leave it empty for platform detection.

## StellarLINK cores and memory ports

The generated SR6P6 defaults include:

```toml
[openocd]
executable = "openocd-stellar"
config = ["board/sr6p6_evb.cfg"]
search = []
commands = []
tcl_port = 6666
gdb_port = 3333
gdb_target = "sr6p6.k0c0.core"
gdb = "arm-none-eabi-gdb"
memory_ap = "sr6p6.axi"
timeout = 600
otp = [{ start = 0x29F87000, size = 0x1000 }]
```

`gdb_target` selects the GDB core. When `core` is unset, inspection/control commands such as `halt`, `go`, `fault` and `check` use that core too. A different `core` does not move the IDE's GDB connection. `memory_ap` names the physical-memory MEM-AP and cannot be replaced with a core name. Names come from the board script.

Tcl and GDB ports must differ. Use separate ports for simultaneous projects to avoid reusing another project's service.

## Environment overrides

Image overrides apply to both backends; TRACE32 overrides follow backend configuration rules. Environment values take precedence over TOML and defaults:

```sh
ELF=build/el2.elf EXTRA_ELF="build/guest.elf" rvdbg config
```

Apply the same prefix to the actual operation or export it in the shell. An override on `config` alone does not persist to the next command.

| Variable | Effect and empty values |
| --- | --- |
| `ELF`, `PROJECT_ROOT` | Primary image or image-resolution root; empty ignored; the latter does not select a config file |
| `EXTRA_ELF` | Shell-style image list; empty explicitly clears it |
| `PROGRAM_NAME` | TRACE32 symbol program; empty applies |
| `T32_SYS`, then `T32SYS` | Installation directory; empty ignored |
| `T32_HOST`, `T32_EXE`, `T32_BIN`, `T32_CONFIG`, `T32_DEBUG_ADAPTER` | Host platform directory and executable paths; empty ignored |
| `T32_CPU`, `T32_CORES`, `T32_MEMACCESS`, `T32_JTAG_CLOCK`, `T32_DUALPORT` | Target settings; empty applies |
| `T32_FLASH_CHIP` | Script matching chip; empty ignored |
| `T32_FLASH_SCRIPT`, `T32_FLASH_ARGS` | Explicit script and shell-style arguments; empty clears |
| `T32_RCL_PORT`, `T32_DAP_PORT`, `T32_DAP_BACKEND_PORT` | Remote API and frontend/backend DAP ports |
| `T32_DAP_BACKEND_TIMEOUT`, `T32_TIMEOUT` | Adapter and TRACE32 operation timeouts |
| `RTT_SYMBOL` | RTT symbol; empty ignored |

Clearing `EXTRA_ELF` changes the [flash/verification image set](flashing.md). TRACE32 erase ranges come from the script and do not shrink automatically with that list.

## Local files

| Location | Contents |
| --- | --- |
| `<project>/.rvdbg/` | `history`, `openocd.log` |
| `<project>/.tracebridge/` | PowerView logs, toolbar scripts, clock HTML and trace exports |
| `~/.config/rvdbg/flash/` | Local flash scripts |
| `~/.config/rvdbg/clock/` | Local clock descriptions |

With `XDG_CONFIG_HOME`, libraries live under `$XDG_CONFIG_HOME/rvdbg/`. Keep licensed or confidential board material in local libraries. Public examples use fictional registers and are not working board configurations.
