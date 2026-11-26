"""Exercise rvdbg's actual terminal UI against TCP test doubles, without a board."""
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
            # The buffer starts after the (variable length) address fields.
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
    def __init__(self, directory, backend):
        self.out = bytearray()
        self.pid, self.pty = pty.fork()
        if self.pid == 0:
            os.chdir(directory)
            self.resize(100, 28, fd=0)
            os.execve(BIN, [BIN, "--tui", "--backend", backend], {"TERM": "xterm-256color", "NO_COLOR": "1", "HOME": directory})

    def resize(self, width, height, fd=None):
        fcntl.ioctl(self.pty if fd is None else fd, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))

    def drain(self):
        if not select.select([self.pty], [], [], 0.025)[0]:
            return
        try:
            data = os.read(self.pty, 65536)
        except OSError as error:
            if error.errno == errno.EIO:
                return
            raise
        # Answer Cursor Position Reports requested by the inline viewport.
        old_size = len(self.out)
        self.out.extend(data)
        for _ in re.finditer(rb"\x1b\[6n", self.out[max(0, old_size - 3):]):
            os.write(self.pty, b"\x1b[1;1R")

    def until(self, condition):
        end = time.monotonic() + 5
        while time.monotonic() < end:
            self.drain()
            if condition():
                return
        raise AssertionError(repr(self.out[-2400:].decode(errors="replace")))

    def expect(self, text, since=0):
        self.until(lambda: text.encode() in self.out[since:])

    def send(self, key):
        os.write(self.pty, key)
        return len(self.out)

    def finish(self, sig=None):
        if sig is None:
            self.send(b"\x04")
        else:
            os.kill(self.pid, sig)
        self.expect("\x1b[?2004l")
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


def exercise(backend):
    with Probe(backend) as probe, tempfile.TemporaryDirectory() as directory:
        Path(directory, "rvdbg.toml").write_text(probe.config())
        tty = Tui(directory, backend)
        try:
            tty.expect("commands")
            modes = termios.tcgetattr(tty.pty)[3]
            assert not modes & termios.ICANON and not modes & termios.ECHO
            idx = tty.send(b"/stat")
            tty.expect("status", idx)
            idx = tty.send(b"\t\r")
            tty.expect("commands", idx)
            tty.resize(72, 16)
            tty.expect("commands", tty.send(b"\x1b[200~help halt\nhelp mem\x1b[201~\r"))
            tty.until(lambda: b"Usage: mem" in tty.out)
            # A repaint has no reason to query the debugger.
            previous = len(probe.target.log)
            end = time.monotonic() + 0.18
            while time.monotonic() < end:
                tty.drain()
            assert len(probe.target.log) == previous
            idx = tty.send(b"run-until 0x4000 --timeout 4\r")
            tty.expect("stop", idx)
            tty.until(lambda: probe.target.running)
            idx = tty.send(b"\x03")
            tty.expect("interrupted", idx)
            tty.expect("commands", idx)
            assert probe.target.running
            if backend == "openocd":
                assert "rbp 0x4000" in probe.target.log
            tty.expect("commands", tty.send(b"halt\r"))
            assert not probe.target.running
            probe.target.hit = True
            idx = tty.send(b"run-until 0x4000 --timeout 4\r")
            tty.expect("reached", idx)
            tty.expect("commands", idx)
            probe.target.hit = False
            idx = tty.send(b"run-until 0x4000 --timeout 0\r")
            tty.expect("not reached", idx)
            tty.expect("commands", idx)
            idx = tty.send(b"rtt --cb 0x1000\r")
            tty.expect("rtt-out", idx)
            tty.expect("return", idx)
            tty.send(b"xyz\r")
            tty.until(lambda: probe.target.read(0x2100, 4) == b"xyz\n")
            tty.expect("commands", tty.send(b"\x03"))
            idx = tty.send(b"fdx --address 0x3000\r")
            tty.expect("el2>", idx)
            tty.expect("< fdx-out", idx)
            tty.send(b"hello\r")
            tty.until(lambda: probe.target.read(0x3008, 4) == struct.pack("<I", 57))
            # The mock is not consuming "hello", so this packet stays queued.
            tty.expect("el2> stale-packet", tty.send(b"stale-packet\r"))
            tty.send(b"\x1b[17~")  # F6 to the main prompt.
            reset_at = tty.send(b"reset\r")
            tty.expect("closed 1 FDX link", reset_at)
            tty.until(lambda: probe.target.read(0x3004, 4) == bytes(4))
            tty.expect("discarded 1 queued packet(s)", idx)
            tty.expect("waiting for the target to open the link", reset_at)
            tty.expect("< fresh-boot", tty.send(b"go\r"))
            assert probe.target.read(0x3008, 4) == struct.pack("<I", 63)

            tty.expect("halted", tty.send(b"halt\r"))
            tty.until(lambda: not probe.target.running)
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
            tty.send(b"\x1b[17~")
            tty.expect("commands", tty.send(b":q\r"))
            tty.expect("commands", tty.send(b"status\r"))
            tty.finish()
        finally:
            tty.close()

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
        tty.expect("commands")
        tty.expect("no PowerView", tty.send(b"status\r"))
        tty.expect("commands")
        tty.finish(signal.SIGTERM)
    finally:
        tty.close()

    for args in (["--tui"], ["--tui", "--json"], ["--tui", "status"]):
        run = subprocess.run([BIN, *args], cwd=directory, input="", capture_output=True,
                             text=True, timeout=5)
        assert run.returncode != 0 and "--tui" in run.stderr and not run.stdout, run
