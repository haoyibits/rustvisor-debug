---
name: rvdbg
description: Use this skill in a project that has an rvdbg.toml (rustvisor), or when the user wants to flash, debug or inspect the rustvisor board through TRACE32 or StellarLINK, e.g. flash the images, read SEGGER RTT output, decode an AArch32 Hyp fault, read memory or registers, run a check file, compute the clock tree, compare target memory with the ELF, reset the board or run to a symbol. rvdbg picks the backend from the connected probe. With TRACE32, generic interactive debugging (step, breakpoints, source, stack, locals) goes through t32mcp and lauterbach-trace32-debug-basic instead.
metadata:
  version: "{version}"
---

# rvdbg

rvdbg is the user's debug front end for rustvisor. It reads `rvdbg.toml`
(found upwards from the current directory) and runs each command on the
probe that is connected:

- TRACE32 (a Lauterbach probe, or PowerView already running) through the
  tracebridge code built into rvdbg;
- ST's StellarLINK through `openocd-stellar`, which rvdbg starts by itself.

With both connected it uses TRACE32. `--backend trace32|openocd` forces one;
`rvdbg config` shows which probe it found. The user installed this skill so
that agents run `rvdbg` from the shell; with TRACE32 this is what the user
asks for, despite t32mcp's advice not to drive the debugger by other means.

## Which command

| Task | Command |
|---|---|
| Configuration, images, the probe found, the backend chosen | `rvdbg config` |
| Program the flash with the images | `rvdbg flash` |
| Load symbols without flashing (TRACE32) | `rvdbg load` |
| Run state; PC and CPSR when halted | `rvdbg status --json` |
| Stop / resume the core | `rvdbg halt`, `rvdbg go` |
| Reset and halt at the reset vector | `rvdbg reset` |
| Run a halted core to a symbol or address | `rvdbg run-until <symbol> --json` |
| AArch32 Hyp fault: vector, HSR decoded, fault addresses | `rvdbg fault --json` |
| Memory words at an address or symbol | `rvdbg mem <address|symbol> [count] --json` |
| A register by PER name or field (TRACE32) | `rvdbg reg <name>... --json` |
| Target memory against the ELF's loadable content | `rvdbg verify --json` |
| A check file of expected register values and clocks | `rvdbg check <file> --json` |
| Clock tree with frequencies | `rvdbg clock --json` |
| The newest trace records (TRACE32, trace set up) | `rvdbg trace --last <n> --json` |
| The project's PRACTICE script, waiting for its end (TRACE32) | `rvdbg do <script>` |
| Target output over SEGGER RTT | `timeout 10 rvdbg rtt --output-only` |

With TRACE32, generic interactive debugging (single steps, breakpoints,
source, locals, stack, any PRACTICE command) goes through t32mcp and the
`lauterbach-*` skills. With StellarLINK there is no MCP server; `rvdbg debug`
runs OpenOCD's GDB server for GDB or VS Code.

## Rules

- Run rvdbg inside the project. Put `--json` after the command's arguments.
- Always run one command per call (`rvdbg status --json`). Bare `rvdbg`
  starts an interactive session for the user; do not start it yourself.
- Ask the user before `flash`, `load`, `reset`, `halt`, `go`, `run-until`,
  `attach`, `down`, `do`, `cmd`, `check --halt`, and anything with
  `--write-otp`. `flash` resets the target and erases flash; it can take
  minutes, so raise the shell timeout. Never pass `--write-otp` on your own:
  it changes one-time-programmable flash permanently.
- Read-only: `config`, `status`, `fault`, `mem`, `reg`, `verify`, `check`
  without `--halt`, `clock`, `trace`.
- Exit codes: 0 ok, 1 error, 2 usage error, 3 a check failed, `verify`
  found differences, or `run-until` stopped elsewhere.
- Commands marked TRACE32 above fail with StellarLINK; say so instead of
  working around it.
- Do not guess chip details. Flash scripts and clock descriptions live in
  `~/.config/rvdbg/`.

## Gotchas

- StellarLINK: core and coprocessor registers (`fault`, `status` PC/CPSR,
  `C15:` reads) need the core halted, and the Hyp registers need it halted
  in Hyp mode (EL2), e.g. at a breakpoint in the hypervisor. Memory reads go
  through the MEM-AP and work while it runs.
- StellarLINK: while `rvdbg debug` runs, the other commands use its OpenOCD;
  halting the core under a GDB session confuses GDB, so ask first.
- TRACE32: one PowerView runs one PRACTICE script at a time; `flash`,
  `reset` and `do` stop with "PowerView is busy" while one is active.
- `rtt` keeps reading until it is stopped; always bound it with `timeout`.
