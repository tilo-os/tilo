#!/usr/bin/env python3
"""Review the real Bash editor with a synthetic, namespace-isolated fixture.

The compiled module is deliberately not a production Terminal backend.
No microphone, personal home, clipboard or external network is exposed.
"""

import argparse
import hashlib
import os
import socket
import subprocess
import pty
import time
import select
import json
import signal
import tempfile
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument(
    "--review-root",
    type=Path,
    required=True,
    help="Private directory containing editor-proof.so compiled from bash_editor_review.c",
)
parser.add_argument(
    "--receipt",
    type=Path,
    required=True,
    help="New evidence file; existing files are never overwritten",
)
args = parser.parse_args()
if not __debug__:
    parser.error("Qualification requires assertions; Python optimization is forbidden")
root = args.review_root.resolve(strict=True)
module = root / "editor-proof.so"
if not module.is_file() or module.stat().st_size > 1024 * 1024:
    parser.error("Missing or oversized native review module")
if args.receipt.exists():
    parser.error("Receipt already exists")
workspace = tempfile.TemporaryDirectory(prefix="editor-journey-", dir=root)
work = Path(workspace.name)
master, slave = pty.openpty()
client, server = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
client.settimeout(2)
rc = work / "synthetic-rc"
rc.write_text(
    "PS1='PROOF> '; PS2='CONT> '; HISTFILE=/dev/null\nset -o emacs\nbind 'set enable-active-region on'\nenable -f /review/editor-proof.so tilo_review || exit 91\ntilo_review "
    + str(server.fileno())
    + " || exit 92\nread -e -p 'STARTUP_PRIVATE> ' startupsecret\n"
)
command = [
    "bwrap",
    "--unshare-all",
    "--die-with-parent",
    "--ro-bind",
    "/usr",
    "/usr",
    "--symlink",
    "usr/lib",
    "/lib",
    "--symlink",
    "usr/lib64",
    "/lib64",
    "--symlink",
    "usr/bin",
    "/bin",
    "--proc",
    "/proc",
    "--dev",
    "/dev",
    "--tmpfs",
    "/tmp",
    "--dir",
    "/review",
    "--ro-bind",
    str(module),
    "/review/editor-proof.so",
    "--ro-bind",
    str(rc),
    "/review/synthetic-rc",
    "--chdir",
    "/tmp",
    "--clearenv",
    "--setenv",
    "PATH",
    "/usr/bin:/bin",
    "--setenv",
    "HOME",
    "/tmp",
    "--setenv",
    "LANG",
    "C.UTF-8",
    "--setenv",
    "TERM",
    "xterm",
    "/usr/bin/python3",
    "-c",
    'import os,fcntl,termios; os.setsid(); fcntl.ioctl(0,termios.TIOCSCTTY,0); os.execv("/usr/bin/bash", ["bash", "--noprofile", "--rcfile", "/review/synthetic-rc", "-i"])',
]
process = subprocess.Popen(
    command,
    stdin=slave,
    stdout=slave,
    stderr=slave,
    pass_fds=(server.fileno(),),
    start_new_session=True,
)
os.close(slave)
server.close()
os.set_blocking(master, False)
output = bytearray()
checks = []


def drain():
    while select.select([master], [], [], 0)[0]:
        try:
            part = os.read(master, 8192)
        except OSError:
            break
        if not part:
            break
        output.extend(part)
        if len(output) > 65536:
            raise RuntimeError("PTY output bound")


def key(text):
    os.write(master, text if isinstance(text, bytes) else text.encode())
    time.sleep(0.25)
    drain()


def call(message):
    client.send(message.encode())
    reply = client.recv(8192).decode()
    drain()
    return reply


def state():
    reply = call("GET")
    if not reply.startswith("STATE "):
        raise AssertionError(reply)
    header, buffer = reply.split("\n", 1)
    _, epoch, start, end, length = header.split()
    assert len(buffer.encode()) == int(length)
    return int(epoch), int(start), int(end), buffer


def replace(snapshot, text):
    epoch, start, end, _ = snapshot
    return call(f"REPLACE {epoch} {start} {end}\n{text}")


def passed(name):
    checks.append(name)
    print("PASS " + name, flush=True)


try:
    time.sleep(0.5)
    drain()
    if process.poll() is not None:
        raise RuntimeError("shell startup failed: " + output.decode(errors="replace"))
    assert call("GET") == "UNAVAILABLE\n"
    key("synthetic-startup-secret")
    assert call("GET") == "UNAVAILABLE\n"
    key(b"\n")
    passed("startup read -e protected input unavailable")
    assert (
        b"no job control" not in output
        and b"cannot set terminal process group" not in output
    )
    assert state()[3] == ""
    passed("actual isolated Bash editor with controlling PTY available")
    key("café ☕")
    before = state()
    assert before[3] == "café ☕"
    reply = replace(before, " bientôt")
    assert reply.startswith("APPLIED "), reply
    applied = int(reply.split()[1])
    assert state()[3] == "café ☕ bientôt"
    assert call(f"UNDO {applied}").startswith("UNDONE ")
    assert state()[3] == "café ☕"
    passed("Unicode insertion and real Readline Undo")
    # Move left one Unicode glyph, activate mark, move to start: actual native selection.
    key(b"\x02\x00\x01\x18\x18")
    selected = state()
    assert selected[1:3] == (0, len("café ".encode())), selected[:3]
    assert replace(selected, "salut ").startswith("APPLIED ")
    changed = state()
    assert changed[3] == "salut ☕", changed[3]
    assert call(f"UNDO {changed[0]}").startswith("UNDONE ")
    restored = state()
    assert restored[3] == "café ☕" and restored[1:3] == selected[1:3]
    passed("native Unicode selection replacement and selection restoration")
    # Return pixels and caret to the same state; old lease must still be stale.
    key(b"\x05")
    old = state()
    key(b"x\x7f")
    assert state()[3] == old[3]
    assert replace(old, "BAD") == "STALE\n"
    passed("net-zero typing invalidates original lease")
    current = state()
    assert replace(current, "\n") == "INVALID\n"
    assert state()[3] == current[3]
    assert replace(current, "\x1b") == "INVALID\n"
    passed("newline and escape cannot be inserted")
    reply = replace(current, " ok")
    assert reply.startswith("APPLIED ")
    assert replace(current, " ok") == "STALE\n"
    assert state()[3] == "café ☕ ok"
    passed("duplicate old lease does not reapply")
    # Cancel current line, enter read -e. The producer must expose no buffer text.
    key(b"\x15")
    key("read -e -p 'PRIVATE> ' secret\n")
    assert call("GET") == "UNAVAILABLE\n"
    key("synthetic-secret")
    assert call("GET") == "UNAVAILABLE\n"
    key(b"\n")
    assert state()[3] == ""
    passed("read -e protected input unavailable and next prompt renewed")
    # Silent non-Readline input cannot service requests. Cancel and drain any queued request.
    old = state()
    key("read -s -p 'SILENT> ' secret\n")
    client.settimeout(0.3)
    client.send(b"GET")
    try:
        client.recv(8192)
        raise AssertionError("silent read exposed editor")
    except socket.timeout:
        pass
    client.settimeout(2)
    key(b"\n")
    assert client.recv(8192).startswith(b"STATE ")
    assert replace(old, "BAD") == "STALE\n"
    passed("silent read yields no editor response and fences prior lease")
    # Subprocess checks only whether the passed capability FD survived exec.
    fd = int(rc.read_text().split("tilo_review ")[-1].split()[0])
    key(
        f'/usr/bin/python3 -c \'import os; print("FD_CLOSED" if not os.path.exists("/proc/self/fd/{fd}") else "FD_LEAK")\'\n'
    )
    assert b"FD_CLOSED\r\n" in output and b"FD_LEAK\r\n" not in output
    passed("editor capability closes on arbitrary child exec")
    key(b"\x15")
    key("exit\n")
    process.wait(timeout=3)
    receipt = {
        "status": "passed",
        "checks": checks,
        "isolated_network": True,
        "user_home_bound": False,
        "microphone_bound": False,
        "production_terminal_integration": False,
        "module_sha256": hashlib.sha256(module.read_bytes()).hexdigest(),
        "runner_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "installed_bash_sha256": hashlib.sha256(
            Path("/usr/bin/bash").read_bytes()
        ).hexdigest(),
        "bash_version": subprocess.check_output(
            ["/usr/bin/bash", "--version"], text=True, timeout=5
        ).splitlines()[0],
    }
    with args.receipt.open("x") as evidence:
        json.dump(receipt, evidence, indent=2)
        evidence.write("\n")
    print(json.dumps(receipt), flush=True)
except BaseException:
    print("PRIVATE_SYNTHETIC_PTY_DIAGNOSTIC " + repr(output[-4096:]), flush=True)
    raise
finally:
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=3)
    client.close()
    os.close(master)
    workspace.cleanup()
