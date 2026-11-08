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
```

The first command uses the primary ELF. `--guest 0` means the first `project.extra_elf` entry, not VMID 0 or hardware core 0. In interactive mode each input line is one packet, and received packets print one per line. The terminal follows a link reopened after target reset.

`--symbol` selects another symbol; `--address` bypasses symbol lookup. See [configuration](configuration.md) for image selection. A supplied address must identify a valid FDX link.

## Scripted packets and timeouts

These messages are examples for firmware implementing that protocol:

```sh
rvdbg fdx --send "boot single" --expect "ok boot" --timeout 10 --json
```

All `--send` packets are sent in order, followed by all `--expect` waits in order. Repeat either option for multiple packets. `--timeout` applies separately to link opening, send space and each expected packet; it is not a total command deadline. Missing an expected packet returns 3.

FDX transports messages; firmware defines their effects, which may include booting, measurement or state changes. `--json` only changes output formatting. See [automation](automation.md) for result handling.

## Buffer ownership

Only one host program may drive an FDX link at a time. Do not share it concurrently among a board runner, `rvdbg fdx` and PowerView `FDX.*` windows. Avoid competing RTT readers updating the same channel read offset too.

`rvdbg fdx --close` modifies RAM to mark a link closed. It is for the specific case where a board is held in reset and RAM retains an old open link, not a general host-disconnect button. Do not use it while firmware or a runner owns the link.
