# Inspect the target and run checks

[中文](../cn/inspection.md)

Decide whether you need execution state, fault context, program contents or an application condition. Inspection does not automatically halt to satisfy a read; `check --halt` is an explicit exception.

TRACE32 needs a running PowerView. StellarLINK reuses an OpenOCD Tcl service or starts one without reset. Use [troubleshooting](troubleshooting.md) on connection failure before resetting a useful fault scene.

## From execution state to a fault report

```sh
rvdbg status
rvdbg halt
rvdbg status
rvdbg fault
```

The first command observes state; `halt` explicitly changes it. When halted, `status` reports PC, symbol offset and decoded CPSR. `fault` reports AArch32 Hyp exception context: vector, HSR, HDFAR/HIFAR, ELR_hyp and SPSR_hyp. An arbitrary halt does not establish that a valid Hyp fault occurred.

StellarLINK core/coprocessor reads require a halted core; Hyp registers additionally require EL2. Failure while halted in Guest EL1 does not imply absent register values. Break at a suitable hypervisor location. TRACE32 can substitute `HC15:` for some Hyp reads from EL1; StellarLINK cannot.

After recording the result, decide whether to resume, reset or redeploy using [debugging commands](debugging.md).

## One value or a RAM buffer?

| Command | Intended object | Units and restrictions |
| --- | --- | --- |
| `mem <address-or-symbol> [count]` | A few memory words, TRACE32 address classes, supported coprocessor addresses | Defaults to one 32-bit word |
| `ram <address-or-symbol> [count]` | Known valid ordinary RAM | Bulk aligned 32-bit words; never MMIO/coprocessors |
| `ram <address-or-symbol> --write <values…>` | Write ordinary RAM | Changes target memory; confirm ownership/address |
| `reg <names…>` | TRACE32 PER registers/fields | Case-sensitive; full path may be necessary |

```sh
rvdbg mem my_buffer 4
rvdbg ram my_buffer 64 --json
rvdbg --backend trace32 reg HSR HSCTLR.C
```

`ram` accesses TRACE32 `AD:` or StellarLINK MEM-AP while cores run. Alignment/range checks cannot establish that an address is ordinary RAM; consult the project's layout. A multiword buffer concurrently updated by firmware is not necessarily a consistent snapshot. Use a firmware protocol or explicit halt as appropriate.

For ordinary RAM you control:

```sh
rvdbg ram my_buffer --write 0x12345678 0
```

Read MMIO with `mem` or project checks, after checking register semantics. A read-clear register can change hardware state even without an explicit write command.

TRACE32 `reg` resolves PER names/fields and reads through PowerView. Ambiguous names with different addresses produce candidate paths; use a full path. `watch` opens PER.Watch on supporting PowerView versions; displaying a window is not an acceptance record.

## Turn observations into check files

Keep checks in the firmware project, for example `checks/boot.toml`. This example assumes a real `idle_loop` symbol:

```toml
description = "Stopped at the idle loop"

[[check]]
name = "PC is in the idle loop"
read = { core = "PC" }
expect = { in_symbol = "idle_loop" }
```

```sh
rvdbg check checks/boot.toml --dry-run
rvdbg check checks/boot.toml --halt
```

`--dry-run` resolves names/symbols without target reads; parsing success does not establish acceptance. `--halt` halts when core/coprocessor reads need it and leaves the core halted. Resume explicitly with `rvdbg go`.

| Read form | Backend |
| --- | --- |
| `read = { addr = "AD:0x..." }` | Both, subject to supported address classes |
| `read = { core = "PC" }` | Both; halted core required |
| `read = { clock = "CORE" }` | Both; clock description required |
| `read = { reg = "SCTRL" }` | TRACE32 PER |
| `read = { expr = "..." }` | TRACE32 PRACTICE |

Numeric predicates include `eq`, `ne`, `nonzero`, `range`, `in_symbol`, `one_of`, with `mask` where applicable. `eq`/`ne`/`one_of` accept `"sym:name+offset"`. `variants = ["debug"]` selects checks for a variant:

```sh
rvdbg check checks/boot.toml --variant debug --json
```

Exit codes are 0 for all conditions met, 3 for executed checks that fail, and 1 for execution errors. The public [check example](https://github.com/haoyibits/rustvisor-debug/blob/main/docs/check-example.toml) includes fictional registers/values and older tracebridge command examples. With rvdbg, use `rvdbg check` and real project conditions.

`verify` checks ELF load contents; `check` evaluates current observations. See [flashing and verification](flashing.md).

## Clock trees and board inputs

```sh
rvdbg clock
rvdbg clock XOSC=40MHz
rvdbg --backend trace32 clock --html
```

Choose a description with `--tree` or match the configured chip against the local `rvdbg/clock` library. `[clock]` provides facts unavailable from registers, such as crystal frequency; command arguments can override inputs temporarily.

Output shows source, frequency and calculation. `off` means disabled; `?` means missing inputs or unknown sources. Warnings such as `not locked` come from description rules. Read failures mark affected clocks as errors and return 1. Accuracy depends on both sampled registers and correct formulas/board inputs.

TRACE32-only `--html` creates `.tracebridge/clock.html` with module and active-source views. Use the [fictional clock example](https://github.com/haoyibits/rustvisor-debug/blob/main/docs/clock-example.toml) for syntax, real chip manuals for formulas, and respect material licensing.

## Export recorded trace (TRACE32)

rvdbg does not configure ETM or board trace hardware. Use a project script, run to the desired point, then export while halted:

```sh
rvdbg halt
rvdbg do scripts/trace_setup.cmm
rvdbg run-until batch_done --timeout 30
rvdbg trace --last 3000 --json > trace.json
```

The script and symbol must exist in your project. `fault --trace` attaches recent records. rvdbg exports existing PowerView records, resolves symbols again and leaves the raw export in `.tracebridge/trace.ad`. Cycle columns need cycle-accurate trace configuration; address records alone cannot establish cycle counts.
