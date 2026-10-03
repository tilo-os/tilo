#!/usr/bin/python3
"""Prove the installed, systemd-activated ush service returns a live shell."""

import array
import json
import os
import select
import socket
import sys
import time
from typing import NoReturn


def fail(message: str) -> NoReturn:
    raise SystemExit(f"ush-systemd-terminal-probe: {message}")


runtime = os.environ.get("XDG_RUNTIME_DIR")
if not runtime or not os.path.isabs(runtime):
    fail("XDG_RUNTIME_DIR must be absolute")
socket_path = os.path.join(runtime, "tilo", "services", "ush.sock")

client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
client.settimeout(10)
client.connect(socket_path)
request = {
    "op": "open-session",
    "target": {"kind": "environment"},
    "cols": 100,
    "rows": 30,
    "term": "xterm-256color",
}
client.sendall(json.dumps(request, separators=(",", ":")).encode() + b"\n")

fds = array.array("i")
payload, ancillary, _flags, _address = client.recvmsg(4096, socket.CMSG_SPACE(fds.itemsize))
for level, kind, data in ancillary:
    if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
        fds.frombytes(data[: len(data) - (len(data) % fds.itemsize)])
if len(fds) != 1:
    fail(f"expected one PTY descriptor, received {len(fds)}; reply={payload!r}")
reply = json.loads(payload.decode().strip())
if reply.get("result") != "session":
    fail(f"service refused the session: {reply!r}")

pty = fds[0]
os.set_blocking(pty, False)


def read_until(needle: bytes, timeout: float) -> bytes:
    seen = bytearray()
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        ready, _, _ = select.select([pty], [], [], min(0.2, deadline - time.monotonic()))
        if not ready:
            continue
        try:
            chunk = os.read(pty, 8192)
        except BlockingIOError:
            continue
        except OSError as error:
            # Linux PTY masters may briefly report EIO while Bubblewrap moves
            # between slave descriptors. The app must survive the same handoff.
            if error.errno == 5:
                time.sleep(0.01)
                continue
            raise
        if not chunk:
            break
        seen.extend(chunk)
        if needle in seen:
            return bytes(seen)
    fail(f"did not receive {needle!r}; PTY output={bytes(seen)!r}")


read_until(b"]#", 240)
os.write(pty, b"printf 'TILO_USH_SYSTEMD_OK-%s\\n' \"$(whoami)\"\n")
output = read_until(b"TILO_USH_SYSTEMD_OK-root", 30)
os.write(pty, b"exit\n")
os.close(pty)
print("ush systemd terminal probe: PASS")
