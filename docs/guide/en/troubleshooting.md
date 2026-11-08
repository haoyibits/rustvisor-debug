# Troubleshoot by symptom

[中文](../cn/troubleshooting.md)

Preserve errors and target context before deciding whether the problem is configuration, service, probe or target. `rvdbg config` describes the intended operation; `rvdbg status` observes execution. Resetting first can destroy the fault scene.

## No probe, or the wrong backend

For `no probe found` or unexpected selection, inspect discovered USB devices, PowerView and fixed backend settings with `rvdbg config`. Both probes default to TRACE32; choose `rvdbg --backend openocd ...` for StellarLINK. In an interactive session, run `reconnect` after changing probes.

Explicit selection bypasses discovery, not physical connection requirements. A successful target-state read is the next confirmation. See [configuration](configuration.md).

## TRACE32 port responds but connection fails

Symptoms include `no PowerView on RCL port ...`, `not a usable TRACE32 RCL endpoint`, or startup never becoming ready.

- If PowerView is absent, run `rvdbg open` before inspection.
- If `config.t32` defines `RCL=`, confirm `NETTCP` and a `PORT=` matching `trace32.rcl_port`.
- If the port belongs to another service, identify the listener rather than treating it as Remote API.
- Check `.tracebridge/powerview.log`, installation paths and startup configuration.

Keep local-access restrictions enabled. See [quick start](quickstart.md) and [configuration](configuration.md).

## OpenOCD fails to start or reuses another session

Inspect executable, board scripts and ports with `rvdbg config`, then read `.rvdbg/openocd.log`. Confirm `openocd-stellar`, available scripts, target power and probe wiring.

rvdbg reuses existing Tcl services. Avoid accidental shared ports across projects and do not terminate an unidentified debugging service just to clear a conflict. Assign separate ports and re-establish the intended [connection](configuration.md).

## Command or option is TRACE32-only

On `... is available with TRACE32 only`, check the chosen backend. PER names, PRACTICE, trace and clock HTML have no direct StellarLINK equivalents. Use verified addresses or check files where appropriate; a PER name is not a physical address. See the [capability table](README.md) and [inspection](inspection.md).

## Core or Hyp registers are unreadable

For `... can only be read while the core is halted`, distinguish memory from core state. Explicitly use `rvdbg halt` or `check --halt` when needed. StellarLINK Hyp reads additionally require EL2; break at a suitable hypervisor location. An EL1 read failure does not imply zero Hyp registers. `check --halt` leaves the core halted; see [inspection](inspection.md).

## PowerView is busy

`flash`, `load`, `reset` and `do` refuse to overwrite an active PRACTICE script. Another tool may own it, or it may be stopped at a dialog, `STOP` or an error. Identify it in PowerView and wait; if abandonment is intended, the operator should end it before retrying.

Increasing timeouts does not resolve script-stack contention. See [debugging](debugging.md).

## Flash timeout, differences or OTP refusal

| Symptom | Next step |
| --- | --- |
| Programming is still progressing | Check `trace32.operation_timeout` / `openocd.timeout`; inspect TRACE32 AREA output |
| `verify` returns 3 | Save the first differing address; check local images, load addresses and the complete image set |
| Another image disappeared | Check `extra_elf`, especially TRACE32 full-range erase behavior |
| OTP differs | Check boot records, protection ranges and whether any change was intended |

Neither `--no-verify` nor `--write-otp` is a general retry option. See [flashing](flashing.md).

## IDE shows only assembly, or wrong breakpoints

Confirm ELF/target agreement, then check DWARF line information. Rust release builds can enable `debug = true`; rebuild and verify/redeploy normally rather than assuming the resulting ELF still matches the board.

TRACE32 must load correct symbols; GDB must load primary and extra ELF symbols. Regenerate IDE files after moving the project. Empty TRACE32 Locals is deliberate proxy behavior; see [debugging](debugging.md).

## RTT is silent or FDX receives no reply

Confirm firmware initialization, symbols/addresses and the TRACE32 program name for RTT. `--cb` only bypasses lookup.

For FDX, check the primary/`--guest N` image, implemented message protocol, exclusive ownership, expected reply and timeout. Do not repeatedly send state-changing packets to an unknown link. See [channels](channels.md).

## A script succeeds despite a failed check

In `rvdbg --json` pipe mode, inspect every end record. Process exit 0 can mean only that the input loop ended normally, even after a command returned 3. Use single-shot commands or implement [per-command result handling](automation.md).
