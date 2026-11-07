# rustvisor-debug

`rvdbg` flashes, inspects and debugs rustvisor boards through a Lauterbach
TRACE32 probe or ST's StellarLINK. It reads one `rvdbg.toml` and runs each
command on the probe that is plugged in: TRACE32 through the built-in
tracebridge code, StellarLINK through a patched OpenOCD
([`openocd-stellar`](https://github.com/haoyibits/OpenOCD/tree/openocd-automotive-mcu-r3),
run as a separate program). One static binary, no Python, nothing copied
into your project except the configuration file.

*中文：[README.zh-CN.md](README.zh-CN.md)*

Supported hosts: macOS on Apple silicon and Linux (x86_64, aarch64). The
[agent skill](skills/rvdbg/SKILL.md) tells an AI agent which command does
what.

## Install

With Homebrew (macOS or Linux):

```sh
brew install haoyibits/tap/rvdbg     # also installs openocd-stellar
```

Or with the install script:

```sh
curl -fsSL https://github.com/haoyibits/rustvisor-debug/releases/latest/download/install.sh | sh
```

This installs `rvdbg` into `~/.local/bin` (set `RVDBG_INSTALL_DIR` to change
it, `RVDBG_VERSION=v0.1.22` to pin a release). Make sure the directory is on
your `PATH`. It does not install openocd-stellar, which the StellarLINK
backend needs; TRACE32 works without it.

Manual install: download `rvdbg-<target>.tar.gz` from the releases page,
check it against the `.sha256` file, and copy the binary somewhere on your
`PATH`.

**macOS and browser downloads:** a binary downloaded with a web browser is
quarantined and Gatekeeper refuses to run it ("cannot be opened because the
developer cannot be verified"). Remove the attribute once:

```sh
xattr -d com.apple.quarantine /path/to/rvdbg
```

`install.sh` (curl) does not set the attribute and removes it if present.

From source: `cargo install --path crates/rustvisor-debug`.

## Requirements

**TRACE32**

- TRACE32 PowerView with `t32debugadapter` (the default layout under `~/t32`
  is detected automatically).
- The Remote API over TCP. Either enable it in `config.t32`:

  ```text
  RCL=NETTCP
  PORT=20000
  REMOTEHOSTDENY
  ```

  or leave it out: when `config.t32` has no `RCL=` section, rvdbg starts
  PowerView with `--t32-api-rcl-remotehost=DENY --t32-api-rcl=TCP:<rcl_port>`.
  `REMOTEHOSTDENY` keeps the port on localhost: the Remote API has no
  authentication, and whoever reaches it can run any command, including
  `OS.*` on your machine.

**StellarLINK**

- `openocd-stellar` (installed with the Homebrew formula).
- For `rvdbg debug`: `arm-none-eabi-gdb`, or VS Code with the
  [Cortex-Debug](https://marketplace.visualstudio.com/items?itemName=marus25.cortex-debug)
  extension.

## Quick start

```sh
cd ~/work/rustvisor-sdk      # the project root
rvdbg init                   # writes rvdbg.toml with the SR6P6 defaults
$EDITOR rvdbg.toml           # program, elf, extra_elf, ...
rvdbg config                 # the images, the probe it found, the backend it picks
rvdbg flash                  # program the images and run
rvdbg vscode                 # or: rvdbg rustrover (TRACE32)
```

Run the commands anywhere inside the project: like git, rvdbg finds the
nearest `rvdbg.toml` in the current directory or a parent. `--config <path>`
selects a file explicitly.

Day to day:

1. Build the images with your own build system.
2. `rvdbg flash` (with TRACE32, `rvdbg load` when only the symbols changed).
3. Debug from the IDE (below), from GDB, or in PowerView.
4. `rvdbg rtt` for the target's RTT console.
5. `rvdbg status`, `fault`, `verify`, `check` to see what the board does and
   whether it runs the images you built. Only `flash` and `reset` reset the
   target.

## Commands

| Command | What it does | TRACE32 | StellarLINK (OpenOCD) |
|---|---|---|---|
| `init` | write a commented `rvdbg.toml` | | |
| `config` | the configuration, the probes found, the backend chosen | | |
| `flash` | program the ELF and the extra images, then run | flash script (below) | erase, write, compare every segment; refuses to change OTP areas unless `--write-otp` |
| `load` | load symbols without flashing or resetting | yes | — (GDB reads the ELF) |
| `debug` | the debug server for the IDE, in the foreground | DAP adapter | GDB server on `gdb_target` |
| `vscode` | write both backends' debug configurations into `.vscode/` | `TRACE32: Attach` | `StellarLINK: Attach` (Cortex-Debug) |
| `rustrover` | write `.run/TRACE32 Attach.run.xml` (LSP4IJ) | yes | — |
| `status`, `halt`, `go` | run state; PC and CPSR when halted; stop; resume | yes | yes |
| `fault` | AArch32 Hyp fault report | also `--trace [N]` | the same report; the core must be halted in Hyp mode |
| `mem <addr\|symbol> [n]` | 32-bit words | yes | through the MEM-AP, symbols from the ELF |
| `verify [ELF]` | target memory against the images | also `--t32` | through the MEM-AP |
| `check FILE` | a check file of expected values | every read kind | `addr`, `core` and `clock` reads |
| `clock` | the clock tree with frequencies | also `--html` | yes |
| `rtt` | SEGGER RTT terminal on channel 0 | yes | through the MEM-AP while the core runs |
| `reset` | reset and halt at the reset vector | yes | `reset halt` |
| `attach` | connect without resetting | `SYStem.Mode Attach` | yes |
| `run-until <symbol\|addr>` | run a halted core to a symbol or address | on-chip breakpoint | hardware breakpoint |
| `open`, `chips`, `reg`, `eval`, `cmd`, `watch`, `trace`, `do`, `down` | PowerView, flash scripts, PER files, PRACTICE, trace | yes | — |
| `skill` | install the agent skill (`~/.claude/skills/rvdbg`) | | |

A TRACE32-only command on StellarLINK fails with "… is available with
TRACE32 only".

The backend is `--backend trace32|openocd`, else `project.backend` in
`rvdbg.toml`, else the connected probe: a StellarLINK (USB 0483:0499 or
263d:4001) means OpenOCD, a Lauterbach probe or a running PowerView means
TRACE32, both mean TRACE32. With StellarLINK the inspect commands use the
OpenOCD that `rvdbg debug` or the session runs (its Tcl port), or start one
that does not reset the target.

`--json` (after the command's arguments) prints one JSON document per
inspect command. Exit codes: 0 ok, 1 error, 2 usage, 3 a check failed,
`verify` found a difference, or `run-until` stopped somewhere else.

On a terminal the output is coloured; the text and the columns are the same
with and without colours. Piped output and `--json` are plain. `NO_COLOR=1`
turns the colours off, and `CLICOLOR_FORCE=1` turns them on for a pipe
(`rvdbg fault | less -R`).

### Interactive session: `rvdbg` alone

`rvdbg` without a command finds `rvdbg.toml`, looks for the probe, and
lists the commands, grouped, with the ones this probe cannot run dimmed
(TRACE32 only, or everything that needs a probe when none is connected).
Then type the commands without `rvdbg` (a leading `rvdbg` is accepted too):

```text
$ rvdbg
rvdbg 0.1.22  /work/rustvisor-sdk/rvdbg.toml
backend  openocd (StellarLINK on USB 0483:0499)
  inspect   status  mem  fault  verify  check  clock  reg  trace  eval
  control   halt  go  reset  attach  run-until  down  do  cmd
  ...
rvdbg [openocd running]> halt
rvdbg [openocd halted]> mem _SEGGER_RTT 4
rvdbg [openocd halted]> help run-until
rvdbg [openocd halted]> quit
```

- The prompt shows the backend and, with OpenOCD, whether the core runs
  (`rvdbg [trace32]>`, `rvdbg [trace32 no-PowerView]>`, `rvdbg [no probe]>`).
- With StellarLINK the session keeps one OpenOCD running, with its GDB
  server open on `gdb_port`. GDB, or VS Code's `StellarLINK: Attach`, can
  connect while the session runs, and the session's commands use the same
  OpenOCD. `debug` in the session only says where the GDB server is.
- With TRACE32 each command talks to PowerView as on the command line.
- Session commands: `help [command]`, `backend trace32|openocd`,
  `reconnect` (look for the probe again, e.g. after plugging it in), and
  `quit` / `exit` / Ctrl-D. Ctrl-C stops a waiting command (`rtt`,
  `run-until`), not the session. A failing command does not end it.
- Tab completes command names. The history is kept in `.rvdbg/history`.
- Read from a pipe, the session runs one command per line without a prompt.

## Configuration

`rvdbg.toml` marks the project root. Every relative path in it is relative
to the directory that contains it. `rvdbg init` writes a commented file with
the SR6P6 defaults.

| Section | Keys |
|---|---|
| `[project]` | `backend` (optional), `program` (TRACE32 program name), `elf`, `extra_elf` (further images flashed, loaded and verified together with `elf`) |
| `[clock]` | frequencies only the board knows, for `clock` and check files: `XOSC = "40MHz"` |
| `[rtt]` | `symbol`, `control_block_address`, `poll_interval` |
| `[trace32]` | `sys`, `host`, `executable`, `binary`, `config`, `debug_adapter`, `rcl_port`, `dap_port`, `dap_backend_port`, `dap_backend_timeout`, `operation_timeout` |
| `[trace32.target]` | `cpu`, `cores`, `mem_access`, `jtag_clock`, `dual_port` |
| `[trace32.flash]` | `chip` or `script` (see below), `args` |
| `[trace32.rtos]` | `config`, `menu`, `show_tasks` (empty = no RTOS awareness) |
| `[openocd]` | `executable`, `config` (board scripts), `search`, `commands`, `tcl_port`, `gdb_port`, `gdb_target` (the core GDB debugs), `core` (the core the inspect commands use, default `gdb_target`), `gdb`, `memory_ap` (reads physical memory), `timeout`, `otp` (one-time-programmable ranges) |

Environment variables override the TRACE32 part for one run. The precedence
is environment, then `rvdbg.toml`, then defaults:

| Variable | Overrides | Empty value |
|---|---|---|
| `T32_SYS`, then `T32SYS` | `trace32.sys` | ignored |
| `T32_HOST`, `T32_EXE`, `T32_BIN`, `T32_CONFIG`, `T32_DEBUG_ADAPTER` | `trace32.*` paths | ignored |
| `PROJECT_ROOT`, `ELF` | project directory, `project.elf` | ignored |
| `RTT_SYMBOL` | `rtt.symbol` | ignored |
| `T32_FLASH_CHIP` | `trace32.flash.chip` | ignored |
| `PROGRAM_NAME`, `T32_CPU`, `T32_CORES`, `T32_MEMACCESS`, `T32_JTAG_CLOCK`, `T32_DUALPORT`, `T32_FLASH_SCRIPT` | the same keys | **used** (clears the value) |
| `T32_FLASH_ARGS` | `trace32.flash.args`, split like a shell command line | **used** (no arguments) |
| `EXTRA_ELF` | `project.extra_elf`, split like a shell command line | **used** (no extra image) |
| `T32_RCL_PORT`, `T32_DAP_PORT`, `T32_DAP_BACKEND_PORT`, `T32_DAP_BACKEND_TIMEOUT`, `T32_TIMEOUT` | ports and timeouts | ignored |
| `T32_DAP_DEBUG=1` | debug logging of `t32debugadapter` | |

Runtime files are written to `<project>/.rvdbg/` (the session history,
`openocd.log`) and,
with TRACE32, `<project>/.tracebridge/` (the PowerView log, the toolbar
script, `clock.html`, `trace.ad`). Each contains its own `.gitignore`.

Board knowledge that does not belong in a public repository lives in
`~/.config/rvdbg/` (or `$XDG_CONFIG_HOME/rvdbg`): flash scripts
in `flash/`, clock descriptions in `clock/`.

## Flashing

### StellarLINK

`rvdbg flash` runs `reset init`, then `flash write_image erase` for every
image, reads back every loadable segment through `memory_ap` and compares it
with the ELF (`--no-verify` skips that), and ends with `reset run`
(`--no-run` leaves the core halted).

**OTP guard.** Before anything is written, the segments inside the
`[openocd] otp` ranges are compared with the board. A difference stops the
flash unless `--write-otp` is given, and a change of a bit from 0 to 1 is
always refused, because OTP cannot be erased. `rvdbg init` lists the SR6P6
UTEST area, which holds the boot record:

```toml
otp = [{ start = 0x29F87000, size = 0x1000 }]
```

### TRACE32: choosing the flash script

`rvdbg flash` needs a flash script that supports Lauterbach's
`PREPAREONLY` convention: set up the target, declare the flash, and return
without programming. When the system is already up, rvdbg first runs
`SYStem.Down`: flash scripts reset and initialize the chip only when the
system is down, and otherwise run the flash algorithm in whatever state the
application left (for example with an MPU that makes the algorithm's RAM
execute-never). After the script, rvdbg runs:

```text
FLASH.ReProgram ALL /Erase
Data.LOAD.Elf <elf>
Data.LOAD.Elf <extra_elf> /NoClear     (once per extra image, if any)
FLASH.ReProgram OFF
SYStem.Down
SYStem.Up
```

Name the chip instead of a file and rvdbg picks the script:

```toml
[trace32.flash]
chip = ""        # empty: use trace32.target.cpu
script = ""      # a path here wins over chip
```

It looks, in this order, in

1. **your library**, `~/.config/rvdbg/flash/*.cmm`, for scripts you
   share between projects, such as a vendor-provided or modified script that
   is not part of the TRACE32 release;
2. **the TRACE32 installation**, `<trace32.sys>/demo/*/flash/*.cmm`, about a
   thousand chip scripts.

A script qualifies when its header has a matching `; @Chip:` line
(Lauterbach's format, wildcards allowed, e.g. `; @Chip: SR6P6*`) and it
supports `PREPAREONLY`. An exact pattern beats a wildcard and a longer
wildcard beats a shorter one; the internal-flash script beats memory
variants (`-qspi`, `-spi`, `-emmc`, `-optionbyte`, ...). Remaining ties are
reported; set `trace32.flash.script` then.

TRACE32's scripts cover a whole family and take the derivative as
`CPU=<name>`. When the chosen script accepts `CPU=` and
`trace32.flash.args` does not set it, rvdbg passes `CPU=<chip>`. Other
arguments such as `DUALPORT=` keep the script's default, so `args` is
usually empty. rvdbg sets `trace32.target.jtag_clock` itself after the
script has run.

```sh
rvdbg chips SR6P6                    # which script, and related ones
rvdbg flash --chip SR6P6             # one-off override
rvdbg flash --script ~~/demo/arm/flash/my_chip.cmm
```

To add a script to the library, copy it there and make sure its header has a
`; @Chip: <name>` line. When you modify an official script, keep its
arguments (`PREPAREONLY`, `DUALPORT=`, ...) as they are. Keep scripts that
come with a TRACE32-only license out of public repositories; the library is
the place for them.

When rvdbg starts PowerView itself, it also adds **Flash** and **Load ELF**
buttons to the PowerView toolbar.

### Several images: `extra_elf`

rustvisor runs more than one separately linked program, a hypervisor and its
guests, each in its own part of the flash. List the others next to `elf`:

```toml
[project]
elf = "target/thumbv8r-none-eabihf/release/boot_diagnostic"
extra_elf = ["target/thumbv8r-none-eabihf/release/scenario-guest"]
```

With TRACE32, `FLASH.ReProgram ALL /Erase` erases every sector the flash
script declares that the session does not write, so flashing the images one
after another would erase the earlier ones. Every command therefore handles
all the images together:

| Command | What happens to the extra images |
|---|---|
| `flash` | programmed together with `elf` (TRACE32: in the same `FLASH.ReProgram` session, `Data.LOAD.Elf ... /NoClear`) |
| `load` | their symbols are loaded after `elf`'s (`/NoCODE /NoClear`) |
| `verify` (no path) | each image is compared in turn; the result matches only if every one does |
| `vscode` | `StellarLINK: Attach` adds their symbols with `add-symbol-file` |
| `config` | listed, with whether the file exists |

`EXTRA_ELF` overrides the list for one run. It is split like a shell
command line, and set but empty means no extra image:

```sh
EXTRA_ELF="build/guest.elf build/other.elf" rvdbg flash
EXTRA_ELF= rvdbg flash      # only elf; with TRACE32 the other images' sectors are erased
```

## Debugging from an IDE or GDB

### VS Code

```sh
rvdbg vscode
```

Merges two configurations into `.vscode/launch.json` and `.vscode/tasks.json`
(existing entries are kept; the files are backed up first as
`*.bak.<timestamp>`). Open **Run and Debug** and press F5 on:

- **TRACE32: Attach**. A hidden task starts `rvdbg adapter`, a DAP proxy in
  front of `t32debugadapter`. PowerView must be running (`rvdbg flash`,
  `load` or `open`). The proxy handles **Restart** (reset through the Remote
  API, then continue) and answers Locals requests with an empty list,
  because some `t32debugadapter` versions crash on FreeRTOS interrupt
  frames; watch expressions, registers, the call stack, breakpoints and
  stepping work normally.
- **StellarLINK: Attach** (needs the Cortex-Debug extension). A hidden task
  runs `rvdbg debug`. The SR6P6 board script resets the core when GDB
  attaches, so the session starts at the reset vector; flash with
  `rvdbg flash` first.

### RustRover (and other JetBrains IDEs), TRACE32

```sh
rvdbg rustrover
```

1. Install the **LSP4IJ** plugin (Settings → Plugins → Marketplace). It adds
   Debug Adapter Protocol support to JetBrains IDEs.
2. The command writes `.run/TRACE32 Attach.run.xml`; the IDE lists it as the
   **TRACE32: Attach** run configuration.
3. Select it and press **Debug**. LSP4IJ starts the adapter and connects when
   it is ready.

Breakpoints can be set in files matching `*.c *.h *.cpp *.hpp *.cc *.s *.S
*.rs` (the configuration's **Mappings** tab).

The paths in the generated files are absolute (rvdbg and `rvdbg.toml`);
rerun `vscode` / `rustrover` after moving the project or reinstalling rvdbg
somewhere else.

### GDB (StellarLINK)

`rvdbg debug` runs OpenOCD in the foreground with a GDB server for
`gdb_target` on `gdb_port` and `gdb_breakpoint_override hard`. It ends when
GDB detaches (`--keep` serves until Ctrl-C). If a session already serves the
ports, it prints where they are and reuses them.

```sh
rvdbg debug &
arm-none-eabi-gdb target/thumbv8r-none-eabihf/release/boot_diagnostic -ex 'target extended-remote :3333'
```

With TRACE32, `rvdbg debug` runs the DAP adapter in the foreground instead.

## RTT

```sh
rvdbg rtt                 # control block from the symbol _SEGGER_RTT
rvdbg rtt --cb 0x60000000 # explicit control block address
rvdbg rtt --output-only   # do not forward keyboard input
rvdbg rtt --replay        # show what is already buffered first
```

Channel 0 is used in both directions while the core runs: with TRACE32
through run-time memory access (`dual_port = "ON"`), with StellarLINK
through the MEM-AP. The terminal switches to character-at-a-time input and
restores its settings on exit.

## Inspecting the target

The inspect commands connect to what runs already: PowerView over the
Remote API, or OpenOCD over its Tcl port. **Only `reset` resets the target**
(and `flash`): connecting and every other command issue no reset, and with
TRACE32 they do not start PowerView (run `rvdbg open`, `flash` or `load`
first).

A typical session:

```text
rvdbg flash                    # program the board
rvdbg
rvdbg [...]> verify            # does the board run the images you built?
rvdbg [...]> status            # run state
rvdbg [...]> halt              # coprocessor and core registers need a halted core
rvdbg [...]> status            # now also PC with symbol+offset, decoded CPSR
rvdbg [...]> fault             # after a crash: vector, HSR decoded, fault address
rvdbg [...]> mem my_buffer 4   # memory at a symbol or address
rvdbg [...]> check checks/boot.toml
rvdbg [...]> go
```

| Command | Kind | What it does |
|---|---|---|
| `status` | R | Run state; when halted, PC with symbol+offset and the decoded CPSR (TRACE32: also debugger mode, power, CPU) |
| `mem <address\|symbol> [count]` | R | `count` 32-bit words (default 1) as a hex dump; `C15:0x4025` reads a coprocessor register |
| `fault [--trace [N]]` | R | AArch32 Hyp fault report: vector slot (also when PC is in a table other than the one HVBAR points at), HSR decoded, HDFAR or HIFAR, ELR_hyp with symbol, SPSR_hyp. `--trace` (TRACE32) adds the newest N trace records (default 32) |
| `verify [elf] [--t32]` | R | Does target memory hold the ELF's loadable content? |
| `check <file> [--variant V] [--dry-run]` | R | Data-driven acceptance check (below) |
| `check <file> --halt` | **S** | The same, but stops the core first when checks read coprocessor or core registers |
| `clock [CLOCK=FREQUENCY…] [--tree FILE] [--html]` | R | The clock tree with frequencies, computed from the clock registers (below) |
| `reg <name\|address>…` | R | TRACE32: registers by PER-file name (`HSR`), field (`HSCTLR.C`, also prints the BITFLD choice), full PER path, or raw address |
| `trace [--last N]` | R | TRACE32: the newest N records (default 100) of the trace PowerView recorded: record number, address, cycles since the previous record, symbol |
| `eval <expression>` | R | TRACE32: the value of any PRACTICE expression |
| `watch <file\|name…>` | UI | TRACE32: a PER.Watch window with exactly these registers (PowerView build 176763, 09/2025, or newer) |
| `halt`, `go` | S | Stop or resume the core |
| `attach` | S | Connect without reset; the core keeps running or stays halted |
| `reset` | S | Reset the chip and halt at the reset vector. TRACE32: `SYStem.Down`, `SYStem.RESet`, `SYStem.CPU`, `SYStem.MemAccess`, `CORE.ASSIGN`, `SYStem.Mode Up`, `SYStem.JtagClock`, refused while a PRACTICE script is active. OpenOCD: `reset halt` |
| `run-until [--timeout S] <symbol\|address>` | S | Runs a halted core until it reaches a symbol, `symbol+offset` or an address, through a temporary hardware breakpoint (TRACE32: `Go.direct … /Onchip`, also a source line `\module\120`; OpenOCD: `bp … hw`, A32). Exit code 3 when the core stops somewhere else |
| `down` | S | TRACE32: `SYStem.Down` |
| `do [--timeout S] <script> [args…]` | S | TRACE32: runs a PRACTICE script with `DO` and waits until it ends. A script that stops at an error is ended and the error reported; one that waits in a dialog or at `STOP` is left for you |
| `cmd <PRACTICE command>` | S | TRACE32: runs **any** command, including reset and flash |

R commands read only, S commands change the target's state.

### How values are read

**TRACE32.** Everything is evaluated by PowerView as PRACTICE functions
(`Data.Long(...)`, `Register(...)`, `PER.VALUE(...)`, `sYmbol.BEGIN(...)`),
so addresses and access classes mean exactly what they mean on the PowerView
command line. Only `verify` uses the raw memory API, and only for plain
memory (`AD:`).

- Register names are looked up with `PER.ADDRESS()`/`PER.VALUE()` in the
  CPU's PER file: `HSR` is searched as `.HSR`, `A.B` as `.A.B` and then as a
  full path. Names are case sensitive; path elements with spaces are quoted:
  `'"TMR (Timer Unit)".TMR_0.CTRL'`. A register name that occurs in many
  blocks (such as `CR`) needs the full path; the error lists the candidates.
- A name defined more than once at the same address is read with a note; at
  different addresses, `reg` lists the full paths. Read-only `rgroup`
  entries, on which `PER.ADDRESS()` fails, are read with `PER.VALUE()`.
- For a BITFLD field, `reg` also prints the choice text for the value, e.g.
  `HSCTLR.C ... 0x00000001  "Enabled"`, usually taken from the PER file
  (`choice_source` in `--json`).
- Before the first PER lookup, rvdbg runs `PER.Set.CONDitions` (again after
  the debugger state changes), and `PER.ReProgram` once per connection when a
  lookup reports "No default peripheral file". Both change debugger state
  only, never the target.
- For `C15:` and `C14:` registers PER.ADDRESS() returns 4 × the
  command-line address; `reg` prints the command-line form and checks it by
  reading it back (`address_check` in `--json`: `confirmed`, `unconfirmed`
  for a value of 0 or 0xFFFFFFFF, or `failed`).
- **Hyp registers while a guest is halted.** TRACE32 executes the MRC at the
  core's current exception level, so a Hyp register (op1 = 4, e.g. HCR
  `C15:0x4011`) gives a bus error in EL1. rvdbg then reads it with `HC15:`,
  which works on the Cortex-R52.

The PER file is also scanned as plain text for these fallbacks, choice texts
and error messages; values always come from TRACE32.

**StellarLINK.** Memory is read through `memory_ap` and works while the core
runs. `C15:`/`C14:` addresses are read with `aarch64 mrc` on the halted
core, in its current mode: Hyp registers need the core halted in EL2, e.g.
at a breakpoint in the hypervisor (there is no `HC15:`). Core registers come
from `get_reg -force`, symbols from the ELF's `.symtab`.

Coprocessor and core registers can only be read from a halted core. The
read-only commands never halt it; they say so instead.

### Trace (TRACE32)

rvdbg sets up no trace: the trace sink, the ETM and their board settings
belong in your own PRACTICE script, run with `rvdbg do`. `trace` and
`fault --trace` only export what `Trace.METHOD` recorded, with
`Trace.EXPORT.Ascii … /ShowRecord` into `.tracebridge/trace.ad`, and name
each address's symbol again, because the export cuts long symbols. The core
must be halted. The cycle column needs cycle-accurate trace
(`ETM.TImeMode CycleAccurate`).

```sh
rvdbg halt
rvdbg do scripts/trace_setup.cmm
rvdbg run-until batch_done --timeout 30
rvdbg trace --last 3000 --json > trace.json
```

### `verify` and `check`

**`verify`** compares every `PT_LOAD` segment with file content at its load
address (LMA, `p_paddr`), not its run address, so initialized data that the
startup code copies to RAM is compared in NVM. It prints `match`, or the first
differing address and the number of differing bytes. `--t32` also runs
TRACE32's own comparison (`Data.LOAD.Elf <elf> /DIFF /PHYSLOAD /NoRegister
/NosYmbol /NoClear`, which changes neither memory, PC nor the loaded symbols)
and reports whether the two agree.

**`check`** runs a TOML file that lives in your project; rvdbg contains no
board facts. [`docs/check-example.toml`](docs/check-example.toml) shows
every form:

```toml
description = "Boot acceptance"

[[check]]
name = "system control"
read = { reg = "SCTRL" }                 # or addr = "AD:0x...", core = "PC", expr = "..."
expect = { eq = 0x00C50078 }             # eq/ne (+ mask), nonzero, range, in_symbol, one_of
variants = ["debug"]                     # optional: only with --variant debug

[[check]]
name = "core clock"
read = { clock = "CORE" }                # a clock of the `clock` tree
expect = { hz = 400_000_000, source = "PLL", warnings = [] }  # any of state, hz, source, warnings
```

`reg` and `expr` reads need TRACE32; `addr`, `core` and `clock` work on both
backends. A `read.clock` check judges the tree that `clock` computes (the
same description and `[clock]` frequencies): `state` is `"on"` or `"off"`,
`hz` must match within half a hertz (and implies "on"), `source` is the
clock it runs from, `warnings` the exact list (e.g. `"not locked"`).

`eq`, `ne` and `one_of` accept `"sym:<name>[+offset]"` in place of a number.
The output is one line per check and a summary. `--dry-run` resolves every
name and symbol but reads nothing, so it also works while the core runs.
When a check reads coprocessor or core registers and the core is running,
`check` stops with an error unless `--halt` is given; with `--halt`, it
stops the core, says so, and leaves it halted (`rvdbg go` resumes it).

### `clock`

`rvdbg clock` prints the whole clock tree as it is configured right now:
every clock under the clock it runs from, with its frequency and how that
comes about.

```text
$ rvdbg clock XOSC=8MHz
/home/me/.config/rvdbg/clock/example.toml (Example MCU (made up))
IRC             16 MHz   nominal  ; internal RC oscillator
XOSC            8 MHz    given  ; crystal: the board decides
└─ PLL          200 MHz  CLKSEL[24]=1 x50 /2
   └─ SYSCLK    200 MHz  CLKSEL[1:0]=2  ; core clock
      ├─ BUS    50 MHz   /4  ; peripheral bus
      └─ TIMER  off      (TIMDIV[31] = 0)
```

- `off` is a clock that is disabled or whose divider field is 0; `?` is a
  clock whose frequency cannot be told (a frequency was not given, or a
  selector shows a source the description does not list); `! …` is a warning
  of the description, such as a PLL that is not locked.
- The only thing you supply is what no register holds, usually the crystal:
  as an argument (`XOSC=40MHz`), or once in `rvdbg.toml`'s `[clock]`.
- The last column says how the frequency comes about: `nominal` or `given`
  for a source clock, `FIELD=N` for the value of a selector, and `xN` `/N`
  for the multiplier and divider applied to the clock one level up.
- It only reads memory-mapped registers, each once, so it works while the
  core runs. A register that cannot be read marks its clocks as `error`
  (exit code 1); the rest is still printed.
- **`--html`** (TRACE32) also writes a diagram to
  `<project>/.tracebridge/clock.html` and prints its `file://` address. It is
  one file that loads nothing from the network and follows the browser's
  light or dark setting. It shows the clocks **by module** (one panel per
  `group` of the description, every selector with all the sources it can
  pick) and **by source** (one tree from the source clocks, the paths in use
  in bold, unused selector inputs named).

The tree comes from a **description file**: `--tree <file>`, or the file in
`~/.config/rvdbg/clock/*.toml` whose `chips` patterns match
`trace32.flash.chip`, or `trace32.target.cpu` when that is empty.
[`docs/clock-example.toml`](docs/clock-example.toml) shows every form:

```toml
chips = ["MYCHIP*"]

[reg]                                    # 32-bit memory-mapped registers
PLLDV = "AD:0x40001008"

[[clock]]
name = "XOSC"                            # no hz: the board gives the frequency

[[clock]]
name = "PLL"
select = "CLKSEL[24]"                    # or: from = "XOSC"
sources = { "0" = "IRC", "1" = "XOSC" }
mul = "PLLDV[6:0]"                       # frequency = source * mul / div
div = "PLLDV[14:12]"
enable = "CTL[2]"                        # 0: the clock is off
warn = [{ when = "PLLSR[2] == 0", text = "not locked" }]
group = "PLL"                            # optional: its panel in the diagram
```

Expressions take numbers, register fields (`REG[high:low]`, `REG[bit]`,
`REG`), `+ - * / ^`, `== !=` and parentheses. Write the description from the
chip's reference manual; vendor documentation is often confidential, so such
a file belongs in your library, not in a public repository.

## AI agents

`rvdbg skill` writes `~/.claude/skills/rvdbg/SKILL.md`
(`$CLAUDE_CONFIG_DIR/skills` when set, or `--dir <skills directory>` for
other agents). It tells the agent which rvdbg command does what, which
commands change the target (ask first; never `--write-otp`), and that
running rvdbg is wanted. Run it again after upgrading rvdbg.

With TRACE32, Lauterbach's [t32mcp](https://gitlab.com/lauterbach/t32mcp)
and its agent skills (`lauterbach-*`, installed from Lauterbach's t32chat)
cover generic debugging. Both talk to the same PowerView over the same
Remote API port:

| Work | Tool |
|---|---|
| Step, breakpoints, source, stack, locals, any PRACTICE command | t32mcp with `lauterbach-trace32-debug-basic` |
| Flash, load, reset to the reset vector, RTT, run to a symbol | `rvdbg flash`, `load`, `reset`, `rtt`, `run-until` |
| Register by PER name, Hyp fault, target memory against the ELF, check files, clock tree | `rvdbg reg`, `fault`, `verify`, `check`, `clock` |

Register t32mcp with the port in `rvdbg.toml` (`trace32.rcl_port`, default
20000). For Claude Code:

```sh
claude mcp add-json -s user t32mcp '{"type":"stdio","command":"/path/to/t32mcp","args":["--execution-policy","unrestricted","--port","20000"]}'
```

Side by side: PowerView runs one PRACTICE script at a time. `flash`, `load`,
`reset` and `do` stop with `PowerView is busy` while a script is active (one
t32mcp runs, or any script paused in a dialog or at an error) instead of
acting under it. rvdbg never selects, creates or closes AREA windows, so
t32mcp's output capture keeps working.

With StellarLINK there is no MCP server; the agent uses rvdbg, and
`rvdbg debug` serves GDB.

### Allowing the read-only commands (Claude Code)

These commands have no side effects, so an agent can run them without
asking. In the project's `.claude/settings.json`:

```json
{
  "permissions": {
    "allow": [
      "Bash(rvdbg config)",
      "Bash(rvdbg chips:*)",
      "Bash(rvdbg status:*)",
      "Bash(rvdbg mem:*)",
      "Bash(rvdbg reg:*)",
      "Bash(rvdbg fault:*)",
      "Bash(rvdbg verify:*)",
      "Bash(rvdbg clock:*)",
      "Bash(rvdbg trace:*)",
      "Bash(rvdbg check checks/boot.toml)",
      "Bash(rvdbg check checks/boot.toml --dry-run)",
      "Bash(rvdbg check checks/boot.toml --variant release)"
    ]
  }
}
```

`check` is allowed with **exact** command lines rather than a `:*` prefix: a
prefix rule would also allow `check … --halt`, which stops the core.
Everything else (`flash`, `load`, `open`, `rtt`, `attach`, `reset`, `down`,
`halt`, `go`, `run-until`, `do`, `cmd`, `check --halt`) still asks. For these
rules to match, run the commands inside the project (no `--config` or
`--backend` before the command) and put `--json` after the command's
arguments.

## Troubleshooting

- **`no probe found`**: connect a StellarLINK or a Lauterbach probe, start
  PowerView, or pass `--backend trace32|openocd`. `rvdbg config` shows what
  was found.
- **`… is available with TRACE32 only`**: the command needs PowerView
  (`reg`, `eval`, `cmd`, `do`, `trace`, ...). With both probes connected,
  `--backend trace32`.
- **`… can only be read while the core is halted`** (`fault`, `status`,
  `check`): coprocessor and core registers need a stopped core. Run
  `rvdbg halt`, or `check --halt`. With StellarLINK, Hyp registers also need
  the core halted in EL2.
- **`port 20000 is open but is not a usable TRACE32 RCL endpoint`**: another
  program uses the port, or `RCL=` in `config.t32` is not `NETTCP`.
- **PowerView does not become ready**: check that `RCL=`/`PORT=` in
  `config.t32` match `trace32.rcl_port`, and read `.tracebridge/powerview.log`.
- **OpenOCD does not start**: read `.rvdbg/openocd.log`; `rvdbg config` shows
  the executable and ports it uses.
- **`no PowerView on RCL port …`**: start it with `rvdbg open`, `flash` or
  `load` before `rtt`, the inspect commands or the IDE debugger.
- **`reg <name>` fails for every name**: check the warnings about
  `PER.ReProgram` and `PER.Set.CONDitions`. `rvdbg eval 'PER.FILENAME()'`
  shows which PER file PowerView uses.
- **`PowerView is busy: a PRACTICE script is active`** (`flash`, `load`,
  `reset`, `do`): a script runs, waits in a dialog or stopped at an error,
  e.g. one an agent started through t32mcp. Let it finish, or end it in
  PowerView (`END`), then retry.
- **Timeouts during flash**: raise `trace32.operation_timeout` or
  `openocd.timeout`; with TRACE32 look at the PowerView AREA window.
- **The List window shows only assembly**: the ELF has no DWARF line table.
  Build with debug info; for Rust, a release build has none unless the
  profile asks for it: `[profile.release] debug = true` (it does not change
  the code that is flashed, only the ELF).
- **RTT waits forever**: the firmware has not initialized RTT yet, or (with
  TRACE32) `project.program` does not match the loaded program name; pass
  `--cb` to bypass the symbol.

## Development

```sh
cargo test --all           # unit, fake-server and end-to-end tests
cargo clippy --all-targets -- -D warnings
```

A virtual workspace:

| Path | What |
|---|---|
| `crates/rustvisor-debug` | `rvdbg`: rustvisor's front end, backend choice, interactive session |
| `crates/tracebridge` | the TRACE32 backend; rustvisor-sdk's board runner uses its library (`tracebridge::backend`, [docs/backend.md](docs/backend.md)) |
| `crates/t32rcl` | pure Rust port of the parts of Lauterbach's `lauterbach-trace32-rcl` Python library (TCP only) that tracebridge uses |
| `crates/stellarbridge` | the OpenOCD (StellarLINK) backend; OpenOCD runs as a separate program |
| `crates/oocd` | client for OpenOCD's Tcl port |
| `crates/probecore` | what both backends share: check files, clock trees, ELF compare, RTT, fault report |
| `skills/` | the agent skills the binaries install |

Dependencies point one way only (`rvdbg → tracebridge, stellarbridge,
probecore`; `tracebridge → t32rcl, probecore`; `stellarbridge → oocd,
probecore`); `crates/rustvisor-debug/tests/workspace_deps.rs` checks it.
`t32rcl` is tested byte-for-byte against traffic recorded from the Python
library; see `crates/t32rcl/tools/` and the `capture_proxy` and `smoke`
examples for recording against a real PowerView.

## License

MIT. `crates/t32rcl` contains code ported from lauterbach-trace32-rcl
(Copyright (c) 2020 Lauterbach GmbH, MIT); see [NOTICE](NOTICE).
