"""High-level Python client for the tilo test_control socket.

Why this exists:
  AI agents and ad-hoc scripts shouldn't have to remember the JSON
  protocol's exact field names or pipe `tilo-control` output through
  `grep`/`jq`. This module wraps the socket + CLI patterns into a
  small typed interface.

Usage:
  >>> from tilo_client import TiloClient
  >>> with TiloClient.spawn_headless() as tilo:
  ...     for target in tilo.list_targets():
  ...         print(target.name, target.rect)
  ...     tilo.click("wheel_disc")
  ...     tilo.snapshot("/tmp/state.png")

If a compositor is already running, skip `spawn_headless` and use
`TiloClient(socket_path)` directly.

Stdlib only — no pip install. Drop into your own automation; the
module is ~150 lines and self-contained.
"""
from __future__ import annotations

import json
import os
import pathlib
import shutil
import socket
import subprocess
import tempfile
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


class TiloError(RuntimeError):
    """Raised when the compositor returns `{"err": {"code": ..., "message": ...}}`."""
    def __init__(self, code: str, message: str):
        super().__init__(f"{code}: {message}")
        self.code = code
        self.message = message

def scratch_session_env(base: dict[str, str] | None = None) -> tuple[dict[str, str], pathlib.Path]:
    """Point HOME and the XDG dirs at a disposable profile.

    Headless scripts used to inherit the login `$HOME`, so the compositor
    indexed the operator's real files. The profile lives under the
    caller's cache (not a tmpfs) and is the caller's to delete.
    """
    env = dict(os.environ if base is None else base)
    login_home = os.path.realpath(env.get("HOME") or "/")
    cache_root = env.get("XDG_CACHE_HOME") or os.path.join(env.get("HOME", "/tmp"), ".cache")
    os.makedirs(cache_root, exist_ok=True)
    profile = pathlib.Path(tempfile.mkdtemp(prefix="tilo-headless.", dir=cache_root))
    for name in ("config", "data", "state", "cache", "home", "identity"):
        (profile / name).mkdir(mode=0o700, exist_ok=True)
    home = profile / "home"
    if os.path.realpath(home) == login_home:
        raise RuntimeError(f"scratch profile home {home} is the login HOME")
    env["HOME"] = str(home)
    env["XDG_CONFIG_HOME"] = str(profile / "config")
    env["XDG_DATA_HOME"] = str(profile / "data")
    env["XDG_STATE_HOME"] = str(profile / "state")
    env["XDG_CACHE_HOME"] = str(profile / "cache")
    env["TILO_IDENTITY_ROOT"] = str(profile / "identity")
    return env, profile


class TiloClient:
    """Thin wrapper over the TILO_TEST_CONTROL unix socket.

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
        socket_path: str | os.PathLike | None = None,
        binary: str | os.PathLike | None = None,
        wait_seconds: float = 15.0,
    ) -> "_SpawnedTilo":
        """Spawn `shell --backend headless`, wait for the socket,
        return a context manager that kills the child on exit.

        Args:
          socket_path: Where to bind the control socket. Defaults to
                       `$XDG_RUNTIME_DIR/tilo-session/tilo-py.sock`; keep
                       any override out of shared directories like /tmp.
          binary: Override the binary path. Defaults to
                  `$TILO_SHELL_BINARY`, else `target/debug/shell`
                  relative to this script's great-grandparent (the
                  repo root).
          wait_seconds: Bounded wait for the socket to appear.
        """
        if socket_path is None:
            runtime = os.environ.get("XDG_RUNTIME_DIR")
            if not runtime or not os.path.isabs(runtime):
                raise RuntimeError(
                    "XDG_RUNTIME_DIR is unset; pass socket_path in a private directory"
                )
            socket_path = os.path.join(runtime, "tilo-session", "tilo-py.sock")
        socket_path = str(socket_path)
        try:
            os.unlink(socket_path)
        except FileNotFoundError:
            pass
        if binary is None:
            binary = os.environ.get("TILO_SHELL_BINARY")
        if binary is None:
            here = pathlib.Path(__file__).resolve().parent
            binary = here.parent.parent / "target" / "debug" / "shell"
        binary = pathlib.Path(binary).resolve()
        if not binary.exists():
            raise FileNotFoundError(
                f"compositor binary not found at {binary} — "
                "run `cargo build -p shell --bin shell` first"
            )

        env, profile = scratch_session_env()
        env["TILO_TEST_CONTROL"] = socket_path
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
                return _SpawnedTilo(proc, cls(socket_path), socket_path, profile)
            time.sleep(0.2)
        proc.kill()
        shutil.rmtree(profile, ignore_errors=True)
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
            raise TiloError(err.get("code", "unknown"), err.get("message", ""))
        if "ok" not in resp:
            raise RuntimeError(f"unexpected response shape: {resp}")
        return resp["ok"]


class _SpawnedTilo:
    """Context manager returned by TiloClient.spawn_headless — kills
    the child + removes the socket on exit."""

    def __init__(
        self,
        proc: subprocess.Popen,
        client: TiloClient,
        socket_path: str,
        profile: pathlib.Path | None = None,
    ):
        self._proc = proc
        self._socket_path = socket_path
        self._profile = profile
        self.client = client

    def __enter__(self) -> TiloClient:
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
        if self._profile is not None:
            shutil.rmtree(self._profile, ignore_errors=True)


__all__ = ["TiloClient", "Target", "TiloError", "scratch_session_env"]
