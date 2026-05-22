"""Tiny client for the nirios test_control socket.

Used by the example scripts in this dir. Not a public API — feel free to
copy this into your own script and inline the bits you need; it's
intentionally short.
"""
from __future__ import annotations

import json
import socket
import sys


def call(sock_path: str, payload: dict, timeout: float = 5.0) -> dict:
    """Send one command, read one response.

    Each command goes on a fresh connection — the compositor allows
    persistent connections too, but one-shot is the simplest pattern
    and is what every example here uses. If you want multiple commands
    on one connection, wrap the socket in a context manager and call
    `_send_recv` directly.
    """
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(sock_path)
    try:
        return _send_recv(s, payload)
    finally:
        s.close()


def _send_recv(sock: socket.socket, payload: dict) -> dict:
    sock.sendall(json.dumps(payload).encode() + b"\n")
    buf = b""
    while b"\n" not in buf:
        chunk = sock.recv(65536)
        if not chunk:
            raise RuntimeError("compositor closed the connection mid-reply")
        buf += chunk
    line, _, _ = buf.partition(b"\n")
    return json.loads(line)


def require_ok(resp: dict) -> dict:
    """Assert the response is `{"ok": {...}}` and return the inner dict.

    Raises with the structured error code on `{"err": {...}}` so the
    calling script fails loudly on `unknown_target` etc. rather than
    silently writing an empty PNG.
    """
    if "err" in resp:
        raise RuntimeError(f"compositor returned err: {resp['err']}")
    if "ok" not in resp:
        raise RuntimeError(f"unexpected response shape: {resp}")
    return resp["ok"]


def socket_path_from_argv(default: str = "/tmp/nirios.sock") -> str:
    return sys.argv[1] if len(sys.argv) > 1 else default
