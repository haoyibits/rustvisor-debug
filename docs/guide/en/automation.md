# Scripts, JSON and AI agents

[中文](../cn/automation.md)

Distinguish single commands, judged by process exit code, from persistent pipe sessions, judged by each command's end record. Fix the [configuration and images](configuration.md), record the rvdbg version, and bound waits in both cases.

## Single commands: check the exit code

```sh
rvdbg status --json
rvdbg verify --json
rvdbg check checks/boot.toml --variant debug --json
```

Put `--json` after command arguments, especially for `cmd`, `eval` and `do`, which consume remaining text. Inspection uses an envelope like this; `result` depends on the command:

```json
{"command":"ram","ok":true,"exit_code":0,"result":{"symbol":null,"address":"0x60000000","words":[1],"hex":["0x00000001"]}}
```

This is a format example, not a board observation. `--json` does not guarantee JSON-only stdout for every command or failure stage. Early configuration errors may only appear on stderr; do not merge stderr into JSON input.

| Code | Meaning | Handling |
| --- | --- | --- |
| 0 | Completed, conditions met | Parse the result as needed |
| 1 | Execution failure | Preserve errors; distinguish config, connection and read failures |
| 2 | Usage error | Fix arguments rather than retrying target operations |
| 3 | Executed but unexpected result | Preserve differences or failed conditions; not a connection failure |

Code 3 includes verify differences, failed checks, run-until stopping elsewhere and FDX expected-packet timeouts. Do not blindly retry `flash`, `reset` or protocol messages: retries change the target again.

A read-only status client:

```python
import json
import subprocess

completed = subprocess.run(
    ["rvdbg", "status", "--json"],
    capture_output=True, text=True, timeout=15,
)
if completed.returncode != 0:
    raise SystemExit(completed.stderr or completed.stdout)
report = json.loads(completed.stdout)
if not report["ok"]:
    raise SystemExit(report)
print(report["result"])
```

For commands returning 3, save structured stdout before classifying the exit code. Flashing needs a deadline covering real programming time; do not copy the 15-second example unchanged.

## Multiple commands: an NDJSON pipe session

`rvdbg --json` enables machine sessions when stdin is a pipe rather than a terminal:

```sh
printf '%s\n' 'status' 'quit' | rvdbg --json
```

It emits ready first, then executes commands line by line, appending end after each command's own output. The following omits command results and uses illustrative values:

```json
{"ready":true,"version":"0.1.25","config":"/work/project/rvdbg.toml","backend":"openocd","reason":"..."}
{"end":"status","ok":true,"exit_code":0,"error":null}
{"end":"quit","ok":true,"exit_code":0,"error":null}
```

`ready: true` means the protocol loop started, not that target checks passed. Check `backend`, connection information and actual results. An undetermined backend can be null.

Drive the process as follows:

1. Read ready and confirm version, configuration path and backend.
2. Write one command and flush; collect output through its corresponding end.
3. Inspect `ok`, `exit_code` and `error` before deciding the next operation.
4. Send `quit` or close stdin, then wait for exit and collect stderr.

Inspection, `ram` and single-shot FDX provide structured output. `flash` and `load` may still emit progress text. Preserve these lines as logs and recognize agreed JSON record types rather than assuming identical structure on every line. Continuous `rtt`/bare `fdx` are unsuitable as normal requests in this loop; use [bounded FDX operations](channels.md).

**A session can exit 0 on EOF even after a command failed.** Consume every end record. Failed commands do not stop the input loop. If failure should abort a sequence, wait for end before sending the next command rather than queueing destructive operations upfront.

Apply deadlines to startup, commands and shutdown. Drain stderr continuously to avoid filling a log pipe. After connection loss inspect target state before replaying operations.

## rustvisor-sdk board runner

The current SDK integration drives installed rvdbg (at least 0.1.24, or the runner's `RVDBG` executable override) through `rvdbg --json`, using one session per flash. `ELF`, `EXTRA_ELF` and `PROGRAM_NAME` select images; the test plan belongs to the SDK runner.

Use ready/end and command results as the interface, not colored tables. Record images, configuration, version and reports to distinguish content matches, acceptance conditions and correct application behavior. The [Rust backend integration document](https://github.com/haoyibits/rustvisor-debug/blob/main/docs/backend.md) describes a different path from the SDK's current subprocess integration.

## AI agents

```sh
rvdbg skill
rvdbg skill --dir /path/to/agent/skills
```

The default destination is `$CLAUDE_CONFIG_DIR/skills` or `~/.claude/skills`. Reinstall after upgrading rvdbg. The skill explains command selection, backend restrictions and confirmation requirements for state changes. Agents must not add `--write-otp` automatically.

Lauterbach t32mcp handles general TRACE32 breakpoints, stepping, stacks and interactive debugging; rvdbg handles deployment, specialized inspection, verification, clocks and RAM/FDX workflows. They share PowerView, which runs one PRACTICE script at a time. StellarLINK uses rvdbg/GDB; see [debugging](debugging.md).

If granting agents confirmation-free commands, restrict actual arguments using the [operation effects](README.md). `check` accepts `--halt` and `ram` accepts `--write`; broad prefixes cannot classify all calls as read-only.
