# Flash, load symbols and verify

[中文](../cn/flashing.md)

Confirm the [image set and backend](configuration.md) first.

| Command | When to use | Target effect |
| --- | --- | --- |
| `rvdbg flash` | Program contents must change | Write Flash, reset and normally run |
| `rvdbg load` | Load current ELF symbols into TRACE32 | No code write or reset; resume if needed |
| `rvdbg verify` | Compare target contents with local ELF | Read and compare; no automatic halt/reset |

`load` does not deploy newly compiled code. Symbols that do not match the target invalidate breakpoint locations, PC names and stack interpretation.

## Declare all images together

```toml
[project]
elf = "build/el2.elf"
extra_elf = ["build/guest.elf", "build/other-guest.elf"]
```

`flash` handles the primary and every extra image in one operation. TRACE32 `load` loads their symbols; `verify` without an ELF argument checks all of them.

TRACE32 uses `FLASH.ReProgram ALL /Erase`: script-declared sectors may be erased even when no image writes them this time. Do not replace `extra_elf` with separate hypervisor/Guest flashes or casually clear the list for a partial update.

## TRACE32: script to execution

`rvdbg flash` starts PowerView if needed. A flash script must support `PREPAREONLY`: initialize the target and declare Flash, then return for rvdbg to program it. An already-up system is taken down first to avoid running the flash algorithm under application state such as MPU settings.

The main sequence is:

```text
script PREPAREONLY
FLASH.ReProgram ALL /Erase
Data.LOAD.Elf <primary-image>
Data.LOAD.Elf <extra-image> /NoClear    # once per extra image
FLASH.ReProgram OFF
SYStem.Down
SYStem.Up
```

Debug setup and symbol loading follow, then execution resumes if needed. This does not automatically run rvdbg's own segment comparison; use `rvdbg verify` to check contents.

An explicit `script` wins; otherwise match `chip`, falling back to `trace32.target.cpu`. Search the local `rvdbg/flash` library before TRACE32's `demo/*/flash` scripts:

```sh
rvdbg chips SR6P6
rvdbg flash --chip SR6P6
rvdbg flash --script '~~/demo/arm/flash/my_chip.cmm'
```

Automatic selection needs suitable `; @Chip:` metadata and `PREPAREONLY` support. Exact matches beat wildcards; more specific wildcards win. Specify a script if ambiguity remains. rvdbg supplies `CPU=` when the script accepts it and no explicit argument exists; use `[trace32.flash] args` for other arguments.

`flash` and `load` do not interrupt an active PRACTICE script. See [troubleshooting](troubleshooting.md) for `PowerView is busy`.

## StellarLINK: write, compare and run

The normal path executes `reset init`, reads the flash sector geometry, erases each image's touched sectors, writes in chunks, compares loadable segments through `memory_ap`, then executes `reset run`. Segments sharing a sector are combined before writing, with gaps and the sector tail padded with `0xFF`.

Writing and comparison each show a percentage based on completed bytes. Write totals include padding. Terminals display an updating progress bar; redirected output uses plain progress lines. Erase and initialization steps are shown separately without a percentage.

```sh
rvdbg --backend openocd flash
rvdbg --backend openocd flash --no-run
```

`--no-run` leaves the target halted; `--no-verify` skips comparison after programming. These and `--write-otp` are OpenOCD-only. TRACE32 `--chip`/`--script` cannot be used with OpenOCD.

Keep verification enabled normally. Differences return 3: inspect the image, address and readback before deciding what to do.

### OTP protection

Before writing, compare image content intersecting `openocd.otp`. Equal content needs no change; different content is refused unless `--write-otp` is explicit. Attempts to change a bit from 0 to 1 are always refused because OTP cannot be erased.

The SR6P6 default protects the UTEST boot-record range: `start = 0x29F87000`, `size = 0x1000`. Ranges are configured rather than automatically discovered for every chip. `--no-verify` does not disable OTP checks.

On differences, first check the image, boot record and range. The operator should enable `--write-otp` only for an intended permanent change.

## Check what is on the board

```sh
rvdbg verify
rvdbg verify build/el2.elf
rvdbg --backend trace32 verify --t32
```

Comparison uses file contents of ELF `PT_LOAD` segments at their LMA (`p_paddr`), not all initialized runtime RAM. Data copied into RAM during startup is checked at its load location. Use [check files](inspection.md) for BSS and application state.

Without a path, `verify` checks all configured images; with a path it checks only that ELF. Differences return 3; read/parse failures return 1.

TRACE32 `--t32` additionally runs `Data.LOAD.Elf /DIFF /PHYSLOAD /NoRegister /NosYmbol /NoClear` for cross-checking without changing target memory, PC or symbols.

After a content match, use [status and checks](inspection.md) to establish program progress, then connect an [IDE](debugging.md).
