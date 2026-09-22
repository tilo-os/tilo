#!/usr/bin/env python3
"""An application launches into a session and paints.

The app is started by address through
the compositor's own action surface, and the compositor is then asked
how many frames it has taken from that client. `frame_seq > 0` is the
ground truth — a tile can look like a window while being a placeholder,
and only the frame counter tells those apart.

A PNG of the final state is written for each app for visual inspection.

What a session needs before any of this works
---------------------------------------------
A session that keeps its own documents repoints `XDG_DATA_HOME`. Two
things that are *machine* state and not session state live under it, so
isolating documents silently isolates them too:

  TILO_APPS_DATA_DIR      the installed-app registry. Without it the
                         session enumerates zero applications.
  TILO_RUNTIME_BUNDLE_DIR the runtime bundle every packaged app pins.
                         Without it every launch fails with "no bundle
                         was found".

Both are set below, pointed at the machine's stores, while
documents stay in the session's own directory.

Usage:
  ./app-launch-proof.py [--keep] [app-id ...]

Exits non-zero if any app fails to paint.
"""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import shutil
import socket
import subprocess

from reaper import die_with_parent
import sys
import tempfile
import time

REPO = pathlib.Path(__file__).resolve().parents[2]
SHELL = REPO / "target" / "debug" / "shell"
LAUNCHER = REPO / "target" / "debug" / "tilo-launcher"

# The two the demo opens first.
DEFAULT_APPS = ["so.tilo.file-explorer", "so.tilo.word-processor"]

# The compositor's action surface. Launching an app is an action like
# any other, which is what lets this drive the production path rather
# than a test-only entry point.
APPLICATIONS = "applications"

failures: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return ok


class Session:
    """One real compositor, isolated from the machine except for the
    two stores an app launch cannot do without."""

    def __init__(self, root: str):
        self.dir = root
        os.makedirs(self.dir, exist_ok=True)
        self.socket = os.path.join(self.dir, "control.sock")
        runtime = os.path.join(self.dir, "run")
        os.makedirs(runtime, mode=0o700, exist_ok=True)
        home = os.path.expanduser("~")
        env = {
            **os.environ,
            "TILO_TEST_CONTROL": self.socket,
            "TILO_STATE_PATH": os.path.join(self.dir, "config", "state.json"),
            # The session's own homes: documents and app state land here.
            "XDG_DATA_HOME": os.path.join(self.dir, "data"),
            "XDG_CACHE_HOME": os.path.join(self.dir, "cache"),
            "XDG_STATE_HOME": os.path.join(self.dir, "state"),
            "XDG_CONFIG_HOME": os.path.join(self.dir, "config"),
            "XDG_RUNTIME_DIR": runtime,
            # Machine state the isolation must not take with it.
            "TILO_APPS_DATA_DIR": os.path.join(home, ".local/share/tilo-apps"),
            "TILO_RUNTIME_BUNDLE_DIR": os.path.join(home, ".local/share/tilo-runtime"),
            "TILO_PEER_KEY_PATH": os.path.join(self.dir, "keys", "peer-key"),
            "TILO_HEADLESS_SIZE": "1280x800",
        }
        env.pop("DISPLAY", None)
        env.pop("WAYLAND_DISPLAY", None)
        self.log_path = os.path.join(self.dir, "shell.log")
        self.log = open(self.log_path, "w")
        self.proc = subprocess.Popen(
            [str(SHELL), "--backend", "headless"],
            env=env, stdout=self.log, stderr=subprocess.STDOUT,
            preexec_fn=die_with_parent(),
        )

    def wait_up(self, timeout: float = 90.0) -> bool:
        end = time.time() + timeout
        while time.time() < end:
            if os.path.exists(self.socket):
                time.sleep(1.0)
                return True
            if self.proc.poll() is not None:
                return False
            time.sleep(0.3)
        return False

    def call(self, payload: dict, timeout: float = 60.0) -> dict:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(timeout)
        s.connect(self.socket)
        try:
            s.sendall((json.dumps(payload) + "\n").encode())
            buf = b""
            while not buf.endswith(b"\n"):
                chunk = s.recv(65536)
                if not chunk:
                    break
                buf += chunk
        finally:
            s.close()
        return json.loads(buf.decode())

    def applications(self) -> list[dict]:
        r = self.call({"op": "actions_enumerate", "query": ""})
        acts = r.get("answer", {}).get("actions", [])
        return [a for a in acts if a.get("provider") == APPLICATIONS]

    def frames(self) -> list[dict]:
        r = self.call({"op": "foreign_frames"})
        # The op has been spelled both ways across versions; accept both
        # rather than silently reporting "no windows" against a rename.
        if "foreign_frames" in r:
            return r["foreign_frames"].get("frames", [])
        return r.get("answer", r.get("frames", [])) or []

    def stop(self):
        for p in (self.proc,):
            try:
                p.terminate()
                p.wait(timeout=8)
            except Exception:
                try:
                    p.kill()
                except Exception:
                    pass
        try:
            self.log.close()
        except Exception:
            pass


def painted(session: Session, ignore: set[int], timeout: float = 60.0) -> dict | None:
    """A surface this launch brought into being, that has since painted.

    `ignore` is every surface that already existed, and skipping it is
    the whole point: apps all report the same wayland app-id, so a
    launch that opened nothing is otherwise indistinguishable from one
    that worked — the previous app's window is still on screen, still
    painting, and still answering. Only a surface id that was not there
    before belongs to this launch.
    """
    end = time.time() + timeout
    while time.time() < end:
        for f in session.frames():
            if f.get("surface_id") not in ignore and f.get("frame_seq", 0) > 0:
                return f
        time.sleep(1.0)
    return None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true", help="leave the session dir behind")
    ap.add_argument("apps", nargs="*", default=None)
    args = ap.parse_args()
    apps = args.apps or DEFAULT_APPS

    for binary, what in ((SHELL, "shell"), (LAUNCHER, "tilo-launcher")):
        if not binary.exists():
            print(f"missing {what} at {binary}")
            print("build it:  cargo build -p shell --bin shell && "
                  "cargo build -p launcher --bin tilo-launcher")
            return 2

    # A session's unix sockets live under this root, and AF_UNIX paths
    # are capped near 108 bytes — a long temp root fails the compositor's
    # own socket bind, not anything about apps.
    root = tempfile.mkdtemp(prefix="/tmp/qs-")
    shots = os.path.join(root, "shots")
    os.makedirs(shots, exist_ok=True)
    print(f"session root: {root}")

    session = Session(os.path.join(root, "s"))
    try:
        if not check("compositor starts", session.wait_up()):
            print(open(session.log_path).read()[-3000:])
            return 1

        offered = session.applications()
        check("the session can see installed applications", len(offered) > 0,
              f"{len(offered)} offered")
        ids = {a.get("id") for a in offered}

        for app in apps:
            print(f"\n{app}")
            if not check("offered by the applications provider", app in ids):
                continue
            before = {f.get("surface_id") for f in session.frames()}
            resp = session.call({"op": "actions_invoke",
                                 "provider": APPLICATIONS, "id": app})
            if not check("invoke accepted", "err" not in resp,
                         json.dumps(resp.get("err", ""))):
                continue
            frame = painted(session, before)
            check("this launch opened a window that painted", frame is not None,
                  json.dumps(frame) if frame
                  else f"no new surface painted (had {len(before)} already)")
            png = os.path.join(shots, f"{app}.png")
            session.call({"op": "snapshot", "path": png})
            if os.path.exists(png) and os.path.getsize(png) > 0:
                print(f"  shot  {png}")
            else:
                check("a snapshot was written", False, png)

        print()
        if failures:
            print(f"FAILED ({len(failures)}): " + "; ".join(failures))
            print(f"compositor log: {session.log_path}")
        else:
            print("all apps launched and painted")
        return 1 if failures else 0
    finally:
        session.stop()
        if not args.keep and not failures:
            shutil.rmtree(root, ignore_errors=True)
        else:
            print(f"session kept at {root}")


if __name__ == "__main__":
    sys.exit(main())
