"""High-level Python client for the nirios test_control socket.

Why this exists:
  AI agents and ad-hoc scripts shouldn't have to remember the JSON
  protocol's exact field names or pipe `qos-control` output through
  `grep`/`jq`. This module wraps the socket + CLI patterns into a
  small typed interface.

Usage:
  >>> from qos_client import NiriosClient
  >>> with NiriosClient.spawn_headless() as nirios:
  ...     for target in nirios.list_targets():
  ...         print(target.name, target.rect)
  ...     nirios.click("wheel_disc")
  ...     nirios.snapshot("/tmp/state.png")

If a compositor is already running, skip `spawn_headless` and use
`NiriosClient(socket_path)` directly.

Stdlib only — no pip install. Drop into your own automation; the
module is ~150 lines and self-contained.
"""
from __future__ import annotations

import json
import os
import pathlib
import socket
import subprocess
import time
from dataclasses import dataclass
from typing import Any


@dataclass
class Target:
    """One row from `list_targets`."""
    name: str
    rect: tuple[float, float, float, float]  # (x, y, w, h)

    @property
    def center(self) -> tuple[float, float]:
        x, y, w, h = self.rect
        return (x + w / 2, y + h / 2)


class NiriosError(RuntimeError):
    """Raised when the compositor returns `{"err": {"code": ..., "message": ...}}`."""
    def __init__(self, code: str, message: str):
        super().__init__(f"{code}: {message}")
        self.code = code
        self.message = message


class NiriosClient:
    """Thin wrapper over the QOS_TEST_CONTROL unix socket.

    Each method opens a fresh connection, sends one command, reads one
    response, closes. That's the simplest correct pattern; if you need
    higher throughput, see the persistent-connection mode in
    `_send_recv` below.
    """

    def __init__(self, socket_path: str | os.PathLike, timeout: float = 5.0):
        self.socket_path = str(socket_path)
        self.timeout = timeout

    # --- Lifecycle ---

    @classmethod
    def spawn_headless(
        cls,
        socket_path: str | os.PathLike = "/tmp/nirios-py.sock",
        binary: str | os.PathLike | None = None,
        wait_seconds: float = 15.0,
    ) -> "_SpawnedNirios":
        """Spawn `shell --backend headless`, wait for the socket,
        return a context manager that kills the child on exit.

        Args:
          socket_path: Where to bind the control socket.
          binary: Override the binary path. Defaults to
                  `target/debug/shell` relative to this script's
                  great-grandparent (the repo root).
          wait_seconds: Bounded wait for the socket to appear.
        """
        socket_path = str(socket_path)
        try:
            os.unlink(socket_path)
        except FileNotFoundError:
            pass
        if binary is None:
            here = pathlib.Path(__file__).resolve().parent
            binary = here.parent.parent / "target" / "debug" / "shell"
        binary = pathlib.Path(binary).resolve()
        if not binary.exists():
            raise FileNotFoundError(
                f"compositor binary not found at {binary} — "
                "run `cargo build -p shell --bin shell` first"
            )

        env = {**os.environ, "QOS_TEST_CONTROL": socket_path}
        env.pop("DISPLAY", None)
        env.pop("WAYLAND_DISPLAY", None)
        proc = subprocess.Popen(
            [str(binary), "--backend", "headless"],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

        deadline = time.time() + wait_seconds
        while time.time() < deadline:
            if os.path.exists(socket_path):
                time.sleep(0.3)  # let the first paint complete
                return _SpawnedNirios(proc, cls(socket_path), socket_path)
            time.sleep(0.2)
        proc.kill()
        raise TimeoutError(f"compositor did not bind {socket_path} within {wait_seconds}s")

    # --- Commands ---

    def list_targets(self) -> list[Target]:
        resp = self._call({"op": "list_targets"})
        return [Target(t["name"], tuple(t["rect"])) for t in resp["targets"]]

    def click(self, name: str) -> None:
        self._require_ok(self._call({"op": "click_named", "name": name}))

    def click_at(self, x: float, y: float) -> None:
        self._require_ok(self._call({"op": "click_at", "x": x, "y": y}))

    def drag(self, x_from: float, y_from: float, x_to: float, y_to: float, steps: int = 8) -> None:
        self._require_ok(self._call({
            "op": "drag", "x_from": x_from, "y_from": y_from,
            "x_to": x_to, "y_to": y_to, "steps": steps,
        }))

    def key(self, key: str) -> None:
        self._require_ok(self._call({"op": "key", "key": key}))

    def type_text(self, text: str) -> None:
        self._require_ok(self._call({"op": "type", "text": text}))

    def modifiers(self, ctrl: bool = False, shift: bool = False, alt: bool = False, logo: bool = False) -> None:
        self._require_ok(self._call({
            "op": "modifiers", "ctrl": ctrl, "shift": shift, "alt": alt, "logo": logo,
        }))

    def snapshot(self, path: str | os.PathLike) -> str:
        """Write a PNG of the current state. Returns the absolute path."""
        path = str(pathlib.Path(path).absolute())
        self._require_ok(self._call({"op": "snapshot", "path": path}))
        return path

    def state_dump(self) -> dict[str, Any]:
        """Return the live `{"workspace_tree": ..., "chrome": ...}` JSON.

        The compositor wire format wraps the payload in a `"state"`
        envelope (Response::State { workspace_tree, chrome }); this
        helper unwraps it so callers get just `workspace_tree` + `chrome`
        keys at the top level.
        """
        resp = self._call({"op": "state_dump"})
        if "state" in resp:
            return resp["state"]
        return resp

    def raw(self, payload: dict[str, Any]) -> dict[str, Any]:
        """Escape hatch — send arbitrary JSON, return the parsed response."""
        return self._call(payload)

    # --- Helpers ---

    def _call(self, payload: dict[str, Any]) -> dict[str, Any]:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(self.timeout)
        s.connect(self.socket_path)
        try:
            return self._send_recv(s, payload)
        finally:
            s.close()

    @staticmethod
    def _send_recv(sock: socket.socket, payload: dict[str, Any]) -> dict[str, Any]:
        sock.sendall(json.dumps(payload).encode() + b"\n")
        buf = b""
        while b"\n" not in buf:
            chunk = sock.recv(65536)
            if not chunk:
                raise ConnectionError("compositor closed the socket mid-reply")
            buf += chunk
        return json.loads(buf.split(b"\n")[0])

    @staticmethod
    def _require_ok(resp: dict[str, Any]) -> dict[str, Any]:
        if "err" in resp:
            err = resp["err"]
            raise NiriosError(err.get("code", "unknown"), err.get("message", ""))
        if "ok" not in resp:
            raise RuntimeError(f"unexpected response shape: {resp}")
        return resp["ok"]


class _SpawnedNirios:
    """Context manager returned by NiriosClient.spawn_headless — kills
    the child + removes the socket on exit."""

    def __init__(self, proc: subprocess.Popen, client: NiriosClient, socket_path: str):
        self._proc = proc
        self._socket_path = socket_path
        self.client = client

    def __enter__(self) -> NiriosClient:
        return self.client

    def __exit__(self, *exc) -> None:
        self._proc.terminate()
        try:
            self._proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self._proc.kill()
        try:
            os.unlink(self._socket_path)
        except FileNotFoundError:
            pass


__all__ = ["NiriosClient", "Target", "NiriosError"]
