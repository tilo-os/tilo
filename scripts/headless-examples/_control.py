"""Tiny client for the tilo test_control socket.

Used by the example scripts in this dir. Not a public API — feel free to
copy this into your own script and inline the bits you need; it's
intentionally short.
"""
from __future__ import annotations

import json
import os
import socket
import sys


def call(sock_path: str, payload: dict, timeout: float = 5.0) -> dict:
    """Send one command, read one response.

    Each command goes on a fresh connection — the compositor allows
    persistent connections too, but one-shot is the simplest pattern
    and is what every example here uses. If you want multiple commands
    on one connection, wrap the socket in a context manager and call
    `_send_recv` directly.

    To wait for a result rather than poll for it, add `wait_ms` to an
    `actions_invoke`, `actions_status` or `actions_enumerate` payload: the
    compositor holds the reply until it has settled (see
    docs/agent-action-api.md). The socket timeout is extended to outlast
    that wait.
    """
    wait_s = payload.get("wait_ms", 0) / 1000.0
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        s.settimeout(timeout + wait_s)
        s.connect(sock_path)
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


def runtime_socket_path(name: str) -> str:
    """`$XDG_RUNTIME_DIR/tilo-session/<name>`: a socket path only this user
    can reach. The compositor creates the directory 0700 when it binds.

    Never a shared directory: a well-known name under /tmp can be bound
    first by another account, which then receives every command sent.
    """
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    if not runtime or not os.path.isabs(runtime):
        raise SystemExit(
            "no control socket: pass its path or set TILO_TEST_CONTROL "
            "(XDG_RUNTIME_DIR is unset, so there is no private default)"
        )
    return os.path.join(runtime, "tilo-session", name)


def default_socket_path() -> str:
    """`$TILO_TEST_CONTROL`, else the default `tilo-control` also uses."""
    return os.environ.get("TILO_TEST_CONTROL") or runtime_socket_path("test-control.sock")


def socket_path_from_argv() -> str:
    return sys.argv[1] if len(sys.argv) > 1 else default_socket_path()
