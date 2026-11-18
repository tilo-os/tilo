#!/usr/bin/env python3
"""Drive a guest's serial console over a qemu unix socket, and log what it said.

    serial-console.py SOCKET --log FILE STEP [STEP ...]
    serial-console.py SOCKET --log FILE --run LINE [--login USER:PASS]
                       [--timeout SECONDS] [--deadline SECONDS]

    STEP := expect:REGEX      wait for REGEX to appear (default timeout 120 s)
          | send:TEXT         type TEXT and press return
          | sleep:SECONDS
          | timeout:SECONDS   change the timeout for later `expect` steps

`--run` is the one-line form a harness needs on a machine with no SSH: it signs
in at the console if it is asked to (`--login USER:PASS`; a session already open
is used as it is), runs LINE in that shell, prints the command's output on
stdout and exits with the command's status. `--timeout` bounds the wait for the
prompt, `--deadline` the wait for the command. Diagnostics go to stderr.

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
# systemd marks the terminal sessions it opens (a `sudo` call among them) with
# OSC 3008 sequences, and everything the command prints arrives between a start
# and an end mark. Left in, a caller reading a value gets the marks with it.
_OSC = re.compile(rb"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)")
_CR = re.compile(rb"\r+\n?")


def visible(buf: bytes) -> bytes:
    """What the line would look like on a terminal, for matching against."""
    return _CR.sub(b"\n", _CSI.sub(b"", _OSC.sub(b"", buf)))


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

    def read_until(self, pattern):
        """Wait for PATTERN; return what the console said before it and the match.

        Everything read is dropped, so a caller that runs one command and reads
        its answer needs no bookkeeping between calls: each connects to the same
        terminal, whose scrollback this process never sees.
        """
        rx = re.compile(pattern.encode(), re.S)
        deadline = time.monotonic() + self.timeout
        while True:
            text = visible(self.buf)
            m = rx.search(text)
            if m:
                before = text[: m.start()].decode("utf-8", "replace")
                self.buf = self.buf[m.end() :]
                return before, m
            if time.monotonic() > deadline:
                tail = visible(self.buf)[-4000:].decode("utf-8", "replace")
                print(
                    f"[FAIL] never saw /{pattern}/ in {self.timeout:.0f}s.\n"
                    f"       Last of what the console said:\n{tail}",
                    file=sys.stderr,
                )
                raise SystemExit(1)
            self._pump()

    def login(self, userpass):
        """Sign in at the console's own prompt, or find a session already open."""
        # A connection sees only what the console says from now on, and a
        # terminal that is already at a prompt says nothing until it is typed
        # at. A bare return asks for that prompt: a signed-in shell prints it
        # again, and a login prompt prints itself again.
        self.send("")
        # At line start only: boot messages and prompts carry '# ' and '$ '
        # mid-line, and taking one for a shell sends the command into
        # whatever is drawing it — at a disk-unlock prompt that then waits for
        # the command's whole deadline instead of letting the caller answer.
        _, prompt = self.read_until(r"(?m)^login: |(?m)^[#$] ")
        if not prompt.group(0).startswith(b"login"):
            return
        user, _, password = userpass.partition(":")
        self.send(user)
        self.read_until(r"[Pp]assword:")
        self.send(password)
        self.read_until(r"[#$] ")

    def run(self, command):
        """Run one shell line in the signed-in session; its output and status.

        A command that takes the session down with it — a reboot the line
        requests — never prints its status; a fresh `login:` on the line means
        the session ended, and that is reported at once rather than after the
        deadline.
        """
        begin = "__TILO_RUN_BEGIN__"
        line = (
            f"printf '\\n{begin}\\n'; {{ {command} ; }} 2>&1; "
            "printf '\\n__TILO_RUN_END__%s\\n' \"$?\""
        )
        self.send(line)
        text, match = self.read_until(r"__TILO_RUN_END__([0-9]+)|login: ")
        # The console echoes what was typed, markers and all; only what follows
        # the last BEGIN is the command's own output.
        if begin in text:
            text = text.rsplit(begin, 1)[1]
        sys.stdout.write(text.lstrip("\r\n"))
        sys.stdout.flush()
        if match.group(1) is None:
            print("[FAIL] the session ended before the command reported", file=sys.stderr)
            raise SystemExit(1)
        return int(match.group(1))


def option(argv, name, default=None):
    if name not in argv:
        return default
    return argv[argv.index(name) + 1]


def main(argv):
    if len(argv) < 4 or "--log" not in argv:
        raise SystemExit(__doc__)
    con = Console(argv[1], option(argv, "--log"))
    command = option(argv, "--run")
    if command is not None:
        # One shell line on a machine with no SSH: sign in if the console asks,
        # then run it and return its status. The timeout bounds the wait for the
        # prompt, never the command itself, which the caller sizes by its own
        # deadline (an update staged, a reboot done).
        con.timeout = float(option(argv, "--timeout", "30"))
        login = option(argv, "--login")
        if login:
            con.login(login)
        con.timeout = float(option(argv, "--deadline", "600"))
        return con.run(command)
    for step in argv[argv.index("--log") + 2 :]:
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
