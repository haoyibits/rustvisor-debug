"""Exercise rvdbg's terminal UI (Fullscreen default + Inline mode) against TCP test doubles, without a board."""
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import runpy
import select
import signal
import socket
import socketserver
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time

ROOT = Path(__file__).resolve().parents[3]
RCL = runpy.run_path(str(ROOT / "t32rcl/tools/fake_t32.py"))
BIN = str(Path(sys.argv[1]).resolve())


class Target:
    def __init__(self):
        self.running = False
        self.hit = False
        self.boot_pending = False
        self.fdx_reads = 0
        self.log = []
        self.ram = bytearray(0x5000)
        self.ram[0x1000:0x100A] = b"SEGGER RTT"
        struct.pack_into("<4I", self.ram, 0x101C, 0x2000, 64, 8, 0)
        struct.pack_into("<4I", self.ram, 0x1034, 0x2100, 64, 0, 0)
        self.ram[0x2000:0x2008] = b"rtt-out\n"
        self.open_fdx(b"fdx-out")

    def open_fdx(self, payload):
        self.ram[0x3000:0x30C0] = bytes(0xC0)
        struct.pack_into("<III", self.ram, 0x3004, 64, 63, 63)
        struct.pack_into("<III", self.ram, 0x3064, 64, 0, 1 + len(payload))
        self.ram[0x3080:0x3082 + len(payload)] = bytes([len(payload)]) + payload + b"\0"

    def reset(self):
        self.running = False
        self.boot_pending = True

    def go(self):
        self.running = True
        if self.boot_pending:
            self.boot_pending = False
            self.open_fdx(b"fresh-boot")

    def read(self, address, length):
        if 0x3000 <= address < 0x30C0:
            self.fdx_reads += 1
        data = self.ram[address:address + length]
        return bytes(data).ljust(length, b"\0")

    def write(self, address, data):
        assert address + len(data) <= len(self.ram)
        self.ram[address:address + len(data)] = data

    def rcl(self, data):
        cmd, opt, mid = data[1:4]
        payload = data[6 if data[0] == 0 else 4:]
        ok = bytes([0, mid])
        if cmd == 0x72 and opt in (4, 5):
            text = payload[4:].split(b"\0", 1)[0].decode()
            self.log.append(text)
            if opt == 4:
                if text.startswith("Go.direct"):
                    self.running = not self.hit
                if text == "Break":
                    self.running = False
                if text.upper() == "SYSTEM.RESET":
                    self.reset()
                if text == "Go":
                    self.go()
                return ok
            if text == "STATE.RUN()":
                ty, response = 1, b"TRUE()" if self.running else b"FALSE()"
            elif text.startswith("ADDRESS.OFFSET(") or text == "Register(PP)":
                ty, response = 4, b"0x4000"
            else:
                ty, response = RCL["FUNCTIONS"].get(text, (4, "0xB"))
                response = response.encode()
            return ok + struct.pack("<II", ty, len(response)) + response
        if cmd == 0x74 and opt in (0x35, 0x36):
            length, address = struct.unpack_from("<H2xQ", payload)
            if opt == 0x35:
                return ok + self.read(address, length)
            head = payload.index(b"XX", 12) + 2
            self.write(address, payload[head:head + length])
            return ok
        return RCL["answer"](data)

    def tcl(self, command):
        self.log.append(command)
        if "curstate" in command:
            return "running" if self.running else "halted"
        if "get_reg" in command:
            register = command.split()[-1]
            return f"{register} 0x4000" if register.lower() == "pc" else f"{register} 0x0"
        if command == "resume":
            if self.hit:
                self.running = False
            else:
                self.go()
        if command == "reset halt":
            self.reset()
        if command.endswith("arp_halt"):
            self.running = False
        fields = command.split()
        if len(fields) >= 5 and fields[1] in ("read_memory", "write_memory"):
            address, width = int(fields[2], 0), int(fields[3]) // 8
            if fields[1] == "read_memory":
                data = self.read(address, int(fields[4]) * width)
                return " ".join(hex(int.from_bytes(data[i:i + width], "little"))
                                for i in range(0, len(data), width))
            values = command.split("{")[-1].rstrip("}").split()
            self.write(address, b"".join(int(v, 0).to_bytes(width, "little") for v in values))
        return ""


class ProbeConnection(socketserver.StreamRequestHandler):
    def handle(self):
        target = self.server.target
        if self.server.backend == "trace32":
            while header := self.rfile.read(8):
                size = struct.unpack_from("<I", header)[0]
                frame = self.rfile.read((size + 7) // 8 * 8)
                self.wfile.write(RCL["frame"](target.rcl(frame[:size])))
        else:
            cmd = bytearray()
            while char := self.rfile.read(1):
                if char == b"\x1a":
                    wrapped = cmd.decode()
                    command = wrapped.split("[catch {", 1)[1].split("} _oocd_out]", 1)[0]
                    self.wfile.write(f"0 {target.tcl(command)}\x1a".encode())
                    cmd.clear()
                else:
                    cmd.extend(char)


class Probe(socketserver.ThreadingTCPServer):
    daemon_threads = True

    def __init__(self, backend):
        super().__init__(("127.0.0.1", 0), ProbeConnection)
        self.target = Target()
        self.backend = backend
        threading.Thread(target=self.serve_forever, daemon=True).start()

    def config(self):
        port = self.server_address[1]
        base = '[project]\nelf = "app.elf"\nprogram = "app"\n'
        if self.backend == "trace32":
            return base + f"[trace32]\nrcl_port = {port}\noperation_timeout = 2\n[trace32.target]\ncpu = \"CORTEXR52\"\n"
        return base + f'[openocd]\ntcl_port = {port}\ncore = "chip.core"\nmemory_ap = "chip.axi"\nconfig = ["fake.cfg"]\n'


class Tui:
    def __init__(self, directory, backend, extra_args=(), color=False, width=112, height=30):
        self.out = bytearray()
        self.width = width
        self.height = height
        self.grid = [[" "] * width for _ in range(height)]
        self.row = 0
        self.col = 0
        self.vt_buf = ""
        self.seen_frames = []
        self.pid, self.pty = pty.fork()
        if self.pid == 0:
            os.chdir(directory)
            self.resize(width, height, fd=0)
            env = {"TERM": "xterm-256color", "HOME": directory}
            if not color:
                env["NO_COLOR"] = "1"
            argv = [BIN, *extra_args, "--backend", backend]
            os.execve(BIN, argv, env)

    def resize(self, width, height, fd=None):
        self.width = width
        self.height = height
        self.grid = [[" "] * width for _ in range(height)]
        self.row = 0
        self.col = 0
        fcntl.ioctl(self.pty if fd is None else fd, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))

    def _feed_vt(self, text):
        s = self.vt_buf + text
        self.vt_buf = ""
        i = 0
        n = len(s)
        while i < n:
            ch = s[i]
            if ch == "\x1b":
                if i + 1 >= n:
                    self.vt_buf = s[i:]
                    break
                nxt = s[i + 1]
                if nxt == "[":
                    j = i + 2
                    while j < n and not (0x40 <= ord(s[j]) <= 0x7E):
                        j += 1
                    if j >= n:
                        self.vt_buf = s[i:]
                        break
                    params = s[i + 2:j]
                    final = s[j]
                    i = j + 1
                    self._csi(params, final)
                    continue
                if nxt == "]":
                    end_bel = s.find("\x07", i + 2)
                    end_st = s.find("\x1b\\", i + 2)
                    if end_bel == -1 and end_st == -1:
                        self.vt_buf = s[i:]
                        break
                    i = (end_bel + 1) if (end_bel != -1 and (end_st == -1 or end_bel < end_st)) else (end_st + 2)
                    continue
                i += 2
                continue
            i += 1
            if ch == "\r":
                self.col = 0
            elif ch == "\n":
                if self.row + 1 < self.height:
                    self.row += 1
                else:
                    self.grid.pop(0)
                    self.grid.append([" "] * self.width)
            elif ord(ch) >= 32:
                if 0 <= self.row < self.height and 0 <= self.col < self.width:
                    self.grid[self.row][self.col] = ch
                self.col = min(self.col + 1, self.width)

    def _csi(self, params, final):
        if params.startswith("?"):
            if params in ("?1049", "?47") and final in ("h", "l"):
                self.grid = [[" "] * self.width for _ in range(self.height)]
                self.row = 0
                self.col = 0
            return
        nums = [int(p) if p.isdigit() else 0 for p in params.split(";")] if params else []
        if final in ("H", "f"):
            r = (nums[0] if len(nums) >= 1 and nums[0] else 1) - 1
            c = (nums[1] if len(nums) >= 2 and nums[1] else 1) - 1
            self.row = max(0, min(r, self.height - 1))
            self.col = max(0, min(c, self.width - 1))
        elif final == "A":
            self.row = max(0, self.row - (nums[0] if nums and nums[0] else 1))
        elif final == "B":
            self.row = min(self.height - 1, self.row + (nums[0] if nums and nums[0] else 1))
        elif final == "C":
            self.col = min(self.width - 1, self.col + (nums[0] if nums and nums[0] else 1))
        elif final == "D":
            self.col = max(0, self.col - (nums[0] if nums and nums[0] else 1))
        elif final == "G":
            c = (nums[0] if nums and nums[0] else 1) - 1
            self.col = max(0, min(c, self.width - 1))
        elif final == "J":
            mode = nums[0] if nums else 0
            if mode == 2:
                self.grid = [[" "] * self.width for _ in range(self.height)]
            elif mode == 0:
                for c in range(self.col, self.width):
                    self.grid[self.row][c] = " "
                for r in range(self.row + 1, self.height):
                    self.grid[r] = [" "] * self.width
        elif final == "K":
            mode = nums[0] if nums else 0
            if mode == 0:
                for c in range(self.col, self.width):
                    self.grid[self.row][c] = " "

    def screen_text(self):
        return "\n".join("".join(row) for row in self.grid)

    def drain(self):
        if not select.select([self.pty], [], [], 0.025)[0]:
            return
        try:
            data = os.read(self.pty, 65536)
        except OSError as error:
            if error.errno == errno.EIO:
                return
            raise
        self.out.extend(data)
        self._feed_vt(data.decode(errors="replace"))
        self.seen_frames.append((len(self.out), self.screen_text()))

    def until(self, condition):
        end = time.monotonic() + 5
        while time.monotonic() < end:
            self.drain()
            if condition():
                return
        raise AssertionError(
            "SCREEN:\n" + self.screen_text() + "\nRAW:\n" + repr(self.out[-1600:].decode(errors="replace"))
        )

    def expect(self, text, since=0):
        def found():
            if since == 0 and text in self.screen_text():
                return True
            return any(pos > since and text in snap for pos, snap in self.seen_frames)
        self.until(found)

    def expect_raw(self, text, since=0):
        self.until(lambda: text.encode() in self.out[since:])

    def send(self, key):
        os.write(self.pty, key)
        return len(self.out)

    def finish(self, sig=None):
        if sig is None:
            idx = self.send(b"\x04")
            self.expect("Press Ctrl+D again", idx)
            self.send(b"\x04")
        else:
            os.kill(self.pid, sig)
        self.expect_raw("\x1b[?2004l")
        self.until(self.exited)
        assert self.code == (128 + sig if sig else 0)
        mode = termios.tcgetattr(self.pty)[3]
        assert mode & termios.ICANON and mode & termios.ECHO

    def exited(self):
        if not self.pid:
            return True
        pid, status = os.waitpid(self.pid, os.WNOHANG)
        if not pid:
            return False
        self.pid = 0
        self.code = os.waitstatus_to_exitcode(status)
        return True

    def close(self):
        if not self.exited():
            os.kill(self.pid, signal.SIGKILL)
            os.waitpid(self.pid, 0)
        os.close(self.pty)


def make_test_elf(symbols):
    """Minimal ELF32 Arm file with defined .symtab entries (name, value, size)."""
    strtab = bytearray(b"\x00")
    symtab = bytearray(16)
    for name, value, size in symbols:
        off = len(strtab)
        strtab.extend(name.encode() + b"\x00")
        symtab.extend(struct.pack("<IIIBBH", off, value, size, 0x11, 0, 1))
    header = bytearray(52)
    header[0:4] = b"\x7fELF"
    header[4] = 1
    header[5] = 1
    struct.pack_into("<H", header, 0x12, 40)
    sym_off = len(header)
    data = bytearray(header) + symtab
    str_off = len(data)
    data.extend(strtab)
    shoff = len(data)
    struct.pack_into("<I", data, 0x20, shoff)
    struct.pack_into("<HHH", data, 0x2E, 40, 3, 0)
    data.extend(bytes(40))
    sh_sym = bytearray(40)
    struct.pack_into("<IIIIII", sh_sym, 4, 2, 0, 0, sym_off, len(symtab), 2)
    struct.pack_into("<I", sh_sym, 0x24, 16)
    sh_str = bytearray(40)
    struct.pack_into("<IIIII", sh_str, 4, 3, 0, 0, str_off, len(strtab))
    data.extend(sh_sym + sh_str)
    return bytes(data)


def exercise(backend):
    with Probe(backend) as probe, tempfile.TemporaryDirectory() as directory:
        Path(directory, "app.elf").write_bytes(make_test_elf([
            ("RUSTVISOR_FDX", 0x3000, 0xC0),
            ("RUSTVISOR_STOP_RECORDS", 0x3400, 0x40),
            ("MAILBOX", 0x3800, 0x40),
        ]))
        Path(directory, "rvdbg.toml").write_text(probe.config())
        # Bare `rvdbg` defaults to Fullscreen (Altscreen) TUI with zero CPR queries.
        tty = Tui(directory, backend)
        try:
            tty.expect_raw("\x1b[?1049h")
            tty.expect("Tab complete")
            assert b"\x1b[6n" not in tty.out
            modes = termios.tcgetattr(tty.pty)[3]
            assert not modes & termios.ICANON and not modes & termios.ECHO

            # Slash menu executes zero-arg commands on a single Enter.
            idx = tty.send(b"/stat")
            tty.expect("/status", idx)
            idx = tty.send(b"\r")
            tty.expect("halted", idx)

            # Unknown slash command reports an error instead of swallowing Enter.
            idx = tty.send(b"/nonexistent\r")
            tty.expect("unknown command '/nonexistent'", idx)

            # Normal Tab completion (without leading `/`) + multiline Tab completion.
            idx = tty.send(b"stat\t\r")
            tty.expect("❯ status", idx)
            tty.expect("4000", idx)

            # Multiline composer Tab completion on line 2.
            idx = tty.send(b"help halt\x1b\rstat\t")
            tty.expect("status", idx)
            tty.send(b"\x03")

            # RAM `=` write with 4-byte ASCII tag + `|GOGO|` readback.
            idx = tty.send(b"ram MAILBOX+0x3c = GOGO\r")
            tty.expect("wrote 1 word(s)", idx)
            idx = tty.send(b"ram MAILBOX+0x3c\r")
            tty.expect("|GOGO|", idx)

            # Multiline paste + resize.
            tty.resize(76, 20)
            idx = tty.send(b"\x1b[200~help halt\nhelp mem\x1b[201~\r")
            tty.expect("Usage: mem", idx)

            # Idle repaint does not query the debugger.
            previous = len(probe.target.log)
            end = time.monotonic() + 0.18
            while time.monotonic() < end:
                tty.drain()
            assert len(probe.target.log) == previous

            # run-until interrupt, hit, and timeout.
            idx = tty.send(b"run-until 0x4000 --timeout 4\r")
            tty.expect("Ctrl+C stop", idx)
            tty.until(lambda: probe.target.running)
            idx = tty.send(b"\x03")
            tty.expect("interrupted", idx)
            tty.expect("/ menu", idx)
            assert probe.target.running
            if backend == "openocd":
                assert "rbp 0x4000" in probe.target.log

            tty.expect("halted", tty.send(b"halt\r"))
            tty.until(lambda: not probe.target.running)
            probe.target.hit = True
            idx = tty.send(b"run-until 0x4000 --timeout 4\r")
            tty.expect("reached", idx)
            tty.expect("/ menu", idx)
            probe.target.hit = False
            idx = tty.send(b"run-until 0x4000 --timeout 0\r")
            tty.expect("not reached", idx)
            tty.expect("/ menu", idx)

            # RTT console passthrough.
            idx = tty.send(b"rtt --cb 0x1000\r")
            tty.expect("rtt-out", idx)
            tty.expect("Ctrl+C return", idx)
            tty.send(b"xyz\r")
            tty.until(lambda: probe.target.read(0x2100, 4) == b"xyz\n")
            stop_rtt = tty.send(b"\x03")
            tty.expect("RTT terminal stopped", stop_rtt)
            tty.until(lambda: "/ menu" in tty.screen_text() and "Ctrl+C stop" not in tty.screen_text())

            # Modeless `@el2` from rvdbg command mode opens the FDX split pane while keeping command focus.
            tty.resize(112, 30)
            idx = tty.send(b"@el2 ping\r")
            tty.expect("FDX", idx)
            tty.expect("Tab complete", idx)
            tty.until(lambda: probe.target.read(0x3008, 4) != struct.pack("<I", 63))
            # Switch into FDX mode, test mis-type guard (`cores` is blocked), then test `!status` & `Ctrl+O`.
            tty.expect("fdx:el2", tty.send(b"\x1b[Z"))
            guard_at = tty.send(b"cores\r")
            tty.expect("not sent", guard_at)
            # Trigger a simulated stop record and verify live [stop] alert.
            probe.target.write(0x3400, struct.pack("<IIII", 0x53540001, 1, 0x0001, 1))
            tty.expect("C0C1 stopped", guard_at)
            # Mock does not consume "ping", so next packet stays queued.
            tty.expect("stale-packet", tty.send(b"stale-packet\r"))
            # Escape into an rvdbg command directly from FDX mode via `!`.
            escape_at = tty.send(b"!status\r")
            tty.expect("0x4000", escape_at)
            # Ctrl+O collapses and expands FDX view.
            collapse_at = tty.send(b"\x0f")
            tty.expect("Ctrl+O expand", collapse_at)
            expand_at = tty.send(b"\x0f")
            tty.expect("Ctrl+O collapse", expand_at)
            # Shift+Tab switches composer back to rvdbg command mode.
            tty.expect("Tab complete", tty.send(b"\x1b[Z"))
            reset_at = tty.send(b"reset\r")
            tty.expect("closed 1 FDX link", reset_at)
            tty.until(lambda: probe.target.read(0x3004, 4) == bytes(4))
            tty.expect("discarded 1 queued packet(s)", reset_at)
            tty.expect("waiting for the target to open the link", reset_at)
            tty.expect("fresh-boot", tty.send(b"go\r"))
            assert probe.target.read(0x3008, 4) == struct.pack("<I", 63)

            # `restart` resets, closes links, and immediately runs `go`.
            resets = sum(1 for x in probe.target.log if x.upper() == "SYSTEM.RESET" or x == "reset halt")
            gos = sum(1 for x in probe.target.log if x in ("Go", "resume"))
            restart_at = tty.send(b"restart\r")
            tty.until(lambda: sum(1 for x in probe.target.log if x.upper() == "SYSTEM.RESET" or x == "reset halt") > resets)
            tty.until(lambda: sum(1 for x in probe.target.log if x in ("Go", "resume")) > gos)
            tty.until(lambda: not probe.target.boot_pending and probe.target.running)
            tty.until(lambda: "Paused" not in tty.screen_text())
            tty.expect("fresh-boot", restart_at)

            tty.send(b"halt\r")
            tty.until(lambda: not probe.target.running)
            tty.until(lambda: "Paused" not in tty.screen_text())
            paused_at = tty.send(b"run-until 0x4000 --timeout 4\r")
            tty.expect("Paused", paused_at)
            tty.until(lambda: probe.target.running)
            reads = probe.target.fdx_reads
            end = time.monotonic() + 0.15
            while time.monotonic() < end:
                tty.drain()
            assert probe.target.fdx_reads == reads
            tty.expect("interrupted", tty.send(b"\x03"))
            tty.until(lambda: probe.target.fdx_reads > reads)

            # Switch back into FDX mode with Shift+Tab and close with :q.
            tty.expect("fdx:el2", tty.send(b"\x1b[Z"))
            tty.expect("Tab complete", tty.send(b":q\r"))

            # Hot-switch Fullscreen -> Inline -> Fullscreen via Alt+F.
            inline_at = tty.send(b"\x1bf")
            tty.expect_raw("\x1b[?1049l", inline_at)
            tty.expect("Alt+F full", inline_at)
            full_at = tty.send(b"\x1bf")
            tty.expect_raw("\x1b[?1049h", full_at)
            tty.expect("Alt+F inline", full_at)

            tty.finish()
        finally:
            tty.close()

        # Verify `--inline` startup mode and One Dark ANSI color output (no stray ␛).
        inline = Tui(directory, backend, extra_args=("--inline",), color=True)
        try:
            inline.expect("Alt+F full")
            idx = inline.send(b"status\r")
            inline.expect("state" if backend == "trace32" else "core", idx)
            inline.expect("running", idx)
            assert b"\x1b[0;36m" in inline.out[idx:] and b"\x1b[0;32m" in inline.out[idx:]
            assert "␛".encode() not in inline.out
            inline.finish()
        finally:
            inline.close()

        # Verify `--json` pipe session compatibility (SDK board-tests contract).
        pipe = subprocess.run([BIN, "--json", "--backend", backend], input="status\nquit\n",
                              cwd=directory, capture_output=True, text=True, timeout=5, check=True)
        frames = [json.loads(line) for line in pipe.stdout.splitlines()]
        assert frames[0]["ready"] and frames[0]["backend"] == backend, frames
        assert frames[1]["command"] == "status" and frames[1]["ok"], frames
        assert frames[2]["end"] == "status" and frames[2]["ok"], frames
        assert frames[3]["end"] == "quit" and frames[3]["ok"], frames


for backend in ("trace32", "openocd"):
    exercise(backend)


# Reserve a port without listening so there is deterministically no PowerView.
with tempfile.TemporaryDirectory() as directory, socket.socket() as port:
    port.bind(("127.0.0.1", 0))
    Path(directory, "rvdbg.toml").write_text(
        f'[project]\nelf = "app.elf"\n[trace32]\nrcl_port = {port.getsockname()[1]}\n'
    )
    tty = Tui(directory, "trace32")
    try:
        tty.expect("Tab complete")
        tty.expect("no PowerView", tty.send(b"status\r"))
        tty.expect("Tab complete")
        tty.finish(signal.SIGTERM)
    finally:
        tty.close()

    # TERM=dumb falls back to the plain stream session without raw mode.
    dumb = subprocess.run(
        [BIN, "--backend", "trace32"],
        cwd=directory,
        input="help\nquit\n",
        capture_output=True,
        text=True,
        env={"TERM": "dumb", "HOME": directory},
        timeout=5,
        check=True,
    )
    assert "inspect" in dumb.stdout, dumb.stdout

    for args in (
        ["--tui"],
        ["--tui", "--json"],
        ["--tui", "status"],
        ["--inline"],
        ["--inline", "status"],
    ):
        run = subprocess.run([BIN, *args], cwd=directory, input="", capture_output=True,
                             text=True, timeout=5)
        assert run.returncode != 0 and ("--tui" in run.stderr or "--inline" in run.stderr) and not run.stdout, run
