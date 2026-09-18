#!/usr/bin/env python3
"""Drive a guest's serial console over a qemu unix socket, and log what it said.

    serial-console.py SOCKET --log FILE STEP [STEP ...]

    STEP := expect:REGEX      wait for REGEX to appear (default timeout 120 s)
          | send:TEXT         type TEXT and press return
          | sleep:SECONDS
          | timeout:SECONDS   change the timeout for later `expect` steps

`vm.sh boot --serial-socket` starts qemu with `-serial unix:...,server=on`,
which is bidirectional — unlike `-serial file:`, which can only be read. That
difference is the whole reason this exists: repair mode is an interactive
console on /dev/console, and a console nothing can type into cannot be shown to
do anything.

## A step that cannot observe is a failure

`expect` exits non-zero on timeout and prints everything received, so a proof
that "the repair console offered a rollback" cannot pass because the harness
was looking at the wrong stream or the guest never got that far. There is
deliberately no "expect, but carry on if absent" step.
"""

import re
import socket
import sys
import time

DEFAULT_TIMEOUT = 120.0

# systemd colours its console output, so the bytes on the wire for a status line
# are `Reached target \x1b[0;1;39mtilo-repair.target\x1b[0m - …`. A pattern written
# the way the line reads on screen never matches that, and the failure looks like
# the guest never got there. The log keeps the raw bytes; matching happens against
# a stripped copy.
_CSI = re.compile(rb"\x1b\[[0-9;?]*[ -/]*[@-~]")
_CR = re.compile(rb"\r+\n?")


def visible(buf: bytes) -> bytes:
    """What the line would look like on a terminal, for matching against."""
    return _CR.sub(b"\n", _CSI.sub(b"", buf))


class Console:
    def __init__(self, path, log):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(0.5)
        self.sock.connect(path)
        self.log = open(log, "ab", buffering=0)
        self.buf = b""
        self.timeout = DEFAULT_TIMEOUT

    def _pump(self):
        try:
            chunk = self.sock.recv(65536)
        except socket.timeout:
            return False
        except OSError as e:
            raise SystemExit(f"serial socket closed: {e}")
        if not chunk:
            raise SystemExit("serial socket reached EOF: the guest is gone")
        self.log.write(chunk)
        self.buf += chunk
        return True

    def expect(self, pattern):
        rx = re.compile(pattern.encode(), re.I)
        deadline = time.monotonic() + self.timeout
        # Match against everything received since the last match, so a pattern
        # that already arrived while we were doing something else still counts.
        while True:
            m = rx.search(visible(self.buf))
            if m:
                # Consume up to the match so a later `expect` for the same
                # string waits for a NEW occurrence rather than re-matching.
                # Stripping can only shorten, so the visible offset is at or
                # before the raw one — dropping that many raw bytes never
                # discards anything the match had not already covered.
                self.buf = self.buf[m.end():]
                print(f"[ok]   matched /{pattern}/", file=sys.stderr)
                return
            if time.monotonic() > deadline:
                tail = visible(self.buf)[-4000:].decode("utf-8", "replace")
                print(
                    f"[FAIL] never saw /{pattern}/ in {self.timeout:.0f}s.\n"
                    f"       Last of what the console said:\n{tail}",
                    file=sys.stderr,
                )
                raise SystemExit(1)
            self._pump()

    def send(self, text):
        print(f"[send] {text!r}", file=sys.stderr)
        self.sock.sendall(text.encode() + b"\r")
        # Give the guest a moment to echo, so the next expect sees the response
        # rather than the echo of what we just typed.
        time.sleep(0.4)
        self._pump()

    def idle(self, seconds):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            self._pump()


def main(argv):
    if len(argv) < 4 or argv[2] != "--log":
        raise SystemExit(__doc__)
    con = Console(argv[1], argv[3])
    for step in argv[4:]:
        kind, _, arg = step.partition(":")
        if kind == "expect":
            con.expect(arg)
        elif kind == "send":
            con.send(arg)
        elif kind == "sleep":
            con.idle(float(arg))
        elif kind == "timeout":
            con.timeout = float(arg)
        else:
            raise SystemExit(f"unknown step: {step}")
    print("[done] all steps completed", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
