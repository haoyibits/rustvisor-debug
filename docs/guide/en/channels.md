# RTT logs and FDX communication

[中文](../cn/channels.md)

RTT and FDX exchange data through target RAM while firmware runs. TRACE32 uses runtime access, usually requiring `dual_port = "ON"`; StellarLINK uses the configured MEM-AP. Firmware must implement and initialize its buffers; host commands do not create a firmware protocol.

| Need | Use |
| --- | --- |
| Continuous character logs or terminal input | RTT channel 0 |
| Send a packet and wait for an agreed reply | FDX |
| Inspect ordinary RAM directly | [mem / ram](inspection.md) |

## RTT: locate the control block

```sh
rvdbg rtt
rvdbg rtt --output-only
rvdbg rtt --replay
```

The default symbol is `_SEGGER_RTT`. `--output-only` disables keyboard forwarding. `--replay` resets the up-channel read offset to zero to display buffered contents; it is not a full historical log replay. Ctrl-C exits and restores terminal settings.

Configuration and temporary overrides:

```toml
[rtt]
symbol = "_SEGGER_RTT"
control_block_address = ""
poll_interval = 0.02
```

```sh
rvdbg rtt --symbol _SEGGER_RTT --poll 0.02
rvdbg rtt --cb 0x60000000 --output-only
```

Replace the example address with the actual control-block address. `--cb` bypasses symbol lookup, not missing firmware initialization, unreadable memory or cache coherency issues. TRACE32 also needs the correct `project.program`. See [troubleshooting](troubleshooting.md) if it keeps waiting.

## FDX: each link belongs to an image

rustvisor uses FDX BufferE links, normally named `RUSTVISOR_FDX`. Two adjacent channels in a static region carry traffic to the target and to the host. Both backends can access them through RAM.

```sh
rvdbg fdx
rvdbg fdx --guest 0
rvdbg fdx --all
```

The first command uses the primary ELF, named `el2`. `--guest 0` (or `--guest guest0`) means the first `project.extra_elf` entry, not VMID 0 or hardware core 0. `--all` watches every image's link at once. In interactive mode each input line is one packet, and received packets print one per line. On a terminal the input line has history and Tab completion (the words of the target's reply to `help`), received packets start with `< ` and are printed above the line being typed; through a pipe the lines are the bare packets. The terminal follows a link reopened after target reset.

In the interactive `rvdbg` TUI, `fdx` opens a persistent FDX monitor (a right-hand split pane on wide fullscreen terminals, or a collapsible preview bar toggled with `Ctrl+O`). Press `Shift+Tab` (or `F6`) to switch the composer between debugger commands and FDX packets, or prefix a line with `!` in FDX mode to run a debugger command without leaving FDX. The monitor and debugger commands share the same probe session. Polling pauses during commands such as `run-until` and resumes when the command returns. Commands such as `reset` and `flash` discard packets the pane has yet to write and mark the boundary. A normal `reset` also marks the watched links it has verified open as closed, and the monitor resumes exchanging packets when firmware reopens them.

With `--all` each packet carries its link's name (`[guest0] < ready`) and the prompt names the link input goes to (`el2> `). `@guest1 run timer` sends one packet to another link, `:to guest1` switches.

Lines that are not packets:

| Input | Effect |
| --- | --- |
| `:q`, `exit`, `quit`, Ctrl-C | Back to rvdbg |
| `:last` | What this session received on the link |
| `:help` | These commands |
| `::text` | Sends `:text` |
| `--…` | Not sent: it looks like an rvdbg option |

The terminal also says what it sees: a packet the target has not read after 1 s (it may no longer serve that link), a packet read but not answered after 2 s, and, when a link is entered again in the same session, the packets received on it before, e.g. `< ready (35 s ago); the target may be waiting for a command`. A link that stays closed for 3 s gets a diagnosis read without halting: the other links' state and the hypervisor's stop records (see [`rvdbg cores`](inspection.md)).

`--symbol` selects another symbol; `--address` bypasses symbol lookup. See [configuration](configuration.md) for image selection. A supplied address must identify a valid FDX link.

## Scripted packets and timeouts

These messages are examples for firmware implementing that protocol:

```sh
rvdbg fdx --send "boot single" --expect "ok boot" --timeout 10 --json
```

All `--send` packets are sent in order, followed by all `--expect` waits in order. Repeat either option for multiple packets. A packet sent before the link opens waits in the ring, so `rvdbg fdx --guest 0 --send "run timer" --expect ready --expect "ok timer"` also works while the Guest is still starting. `--timeout` applies separately to link opening, send space and each expected packet; it is not a total command deadline. Missing an expected packet returns 3. A link that does not open within the timeout fails with the same diagnosis as above in the error.

FDX transports messages; firmware defines their effects, which may include booting, measurement or state changes. `--json` only changes output formatting. See [automation](automation.md) for result handling.

## Buffer ownership

Only one host program may drive an FDX link at a time. Do not share it concurrently among a board runner, `rvdbg fdx` and PowerView `FDX.*` windows. Avoid competing RTT readers updating the same channel read offset too.

`rvdbg reset` marks every image's link closed after the reset, while the chip is held, so that `fdx` waits for this run's links instead of reading the last run's (`reset --keep-links` leaves them). `rvdbg fdx --close` (one link) and `rvdbg fdx --close --all` do the same without a reset: they modify RAM, for a board held in reset whose RAM retains an old open link, not as a general host-disconnect button. Do not use them while firmware or a runner owns the link.
