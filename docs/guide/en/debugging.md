# Debugging and interactive sessions

[中文](../cn/debugging.md)

Start with a deployed program. Confirm [local and target contents match](flashing.md) and the ELF contains debug information; an ELF suffix alone does not imply source line tables.

## Choose the target state change

| Command | Behavior |
| --- | --- |
| `rvdbg status` | Inspect state without automatically halting |
| `rvdbg attach` | Connect without reset, preserving running/halted state |
| `rvdbg halt` | Halt the selected core |
| `rvdbg go` | Resume the selected core |
| `rvdbg reset` | Reset the chip and halt at the reset vector |
| `rvdbg run-until <symbol-or-address>` | Run a halted core to a temporary hardware breakpoint |
| `rvdbg down` | TRACE32 `SYStem.Down` |

TRACE32 inspection/control needs PowerView already running. `rvdbg open` starts or reuses it. `rvdbg load` loads symbols but also resumes a halted target.

For example:

```sh
rvdbg halt
rvdbg run-until rustvisor_boot_main --timeout 30
rvdbg status
```

The symbol must exist and remain reachable. `run-until` does not first halt a running core. Stopping elsewhere returns 3. Timeout does not mean the target is halted: on StellarLINK it keeps running while the tool attempts to remove the breakpoint. Check status next. OpenOCD uses A32 hardware breakpoints; do not assume this works for another instruction set.

## VS Code

```sh
rvdbg vscode
```

Configurations are merged into `.vscode/launch.json` and `tasks.json`, preserving existing entries and backing up modified files as `*.bak.<timestamp>`. Select the matching configuration in Run and Debug:

| Configuration | Connection | Preparation and effects |
| --- | --- | --- |
| `TRACE32: Attach` | Hidden task starts rvdbg's DAP proxy | PowerView running, symbols loaded |
| `StellarLINK: Attach` | Cortex-Debug connects to `rvdbg debug` | Flash first; SR6P6's board script resets the core when GDB connects |

The name “Attach” does not guarantee preservation of the current fault scene. Record [inspection results](inspection.md) before opening an IDE if necessary.

The TRACE32 proxy handles Restart by resetting through Remote API and resuming. It returns empty Locals lists to avoid crashes in some `t32debugadapter` versions on FreeRTOS interrupt frames; registers, stack, watch expressions, breakpoints and stepping remain available.

Generated files contain absolute rvdbg/config paths. Regenerate after moving the project or executable.

## RustRover / JetBrains (TRACE32)

Install LSP4IJ, then run:

```sh
rvdbg rustrover
```

This creates `.run/TRACE32 Attach.run.xml`. Select `TRACE32: Attach` and Debug; LSP4IJ starts and connects to the DAP proxy. For StellarLINK use VS Code/Cortex-Debug or GDB.

## Direct GDB (StellarLINK)

Keep the service running in one terminal:

```sh
rvdbg --backend openocd debug
```

In another, load the actual primary image and connect on the default port:

```sh
arm-none-eabi-gdb build/el2.elf -ex 'target extended-remote :3333'
```

The service normally stops when GDB detaches; `rvdbg debug --keep` runs until Ctrl-C. Reusing a session's OpenOCD leaves its lifetime under that session's control. Load extra image symbols yourself in a manual GDB session; generated VS Code configuration does this for `extra_elf`.

`gdb_target` selects the debug core; inspection `core` can differ, as explained in [configuration](configuration.md). External halt/resume commands affect the same core GDB is debugging; these are not isolated sessions.

## Interactive commands

Run bare `rvdbg`. The following is illustrative; prompts depend on the backend and target state:

```text
$ rvdbg
rvdbg [...]> status
rvdbg [...]> halt
rvdbg [...]> fault
rvdbg [...]> mem my_buffer 4
rvdbg [...]> go
rvdbg [...]> quit
```

Use `help [command]`, Tab completion, `backend trace32|openocd`, `reconnect`, and `quit`/`exit`. Use `reconnect` after changing probes. History lives in `.rvdbg/history`. Ctrl-C interrupts waiting RTT/FDX/run-until commands without ending the session; command failures also leave it open.

A StellarLINK session keeps one OpenOCD shared by commands and GDB. TRACE32 inspection, `ram` and `fdx` share a lazily established connection. Failure drops it; the next command reconnects, without replaying the failed command. `flash`, `load` and `open` manage connections separately.

## Custom TRACE32 operations

```sh
rvdbg eval 'Register(PC)'
rvdbg cmd 'Break.Set my_function /Program /Onchip'
rvdbg do scripts/trace_setup.cmm
```

`eval` evaluates expressions, `cmd` executes PRACTICE commands and `do` runs a script and waits. Scripts stopped by errors are cleaned up and reported; dialogs and `STOP` remain for the operator. Effects depend on content and may include register writes, reset and flashing.

See [inspection](inspection.md) for trace setup/export and [troubleshooting](troubleshooting.md) for missing sources, ports and busy scripts.
