#!/usr/bin/env python3
"""An invite token crosses between two machines without being retyped.

Inviting a peer mints a signed token: a few hundred opaque base64
characters. Until now there was no way to get one out of the machine
that minted it and no way to get one into the machine that needs it —
no copy on the issuing side, no paste in the prompt that consumes it.
The only route across was to read it off one screen and type it into
another, and nobody does that correctly, twice, on a stage.

So this drives the two halves that make the crossing possible, on two
real compositors with separate identities, separate homes and separate
device keys:

  quinten   /invite  →  the invite card  →  **Ctrl+C**
            his clipboard now holds the token

  (the hop between the two machines is the person's — a chat message,
   an email, a USB stick. It is not the OS's job and this script does
   not pretend it is. What it does do is take the string out of
   quinten's *clipboard*, never out of his chrome state, so what
   travels is what the copy actually produced.)

  xaver     /connect →  prompt opens  →  **Ctrl+V**  →  Enter
            joined, without one character having been typed

The load-bearing claim is a negative one: this script never calls the
`type` op with the token. If the paste did not work the prompt stays
empty and the run fails, rather than quietly falling back to typing it
in — which is what the existing peer proofs do, and which is exactly
the gesture that does not exist for a human.

Usage:
  ./invite-clipboard-proof.py [--keep] [--out DIR]

  cargo build -p shell

Exits non-zero if any claim fails.
"""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import shutil
import socket
import subprocess
import sys
import tempfile
import time

from reaper import die_with_parent

REPO = pathlib.Path(__file__).resolve().parents[2]
SHELL = REPO / "target" / "debug" / "shell"
ACTIONS = "actions"

failures: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return bool(ok)


def step(what: str) -> None:
    print(f"\n· {what}")


def wait_for(pred, timeout=30.0, poll=0.3):
    end = time.time() + timeout
    while time.time() < end:
        try:
            got = pred()
        except Exception:
            got = None
        if got:
            return got
        time.sleep(poll)
    return None


def freshness(binary: pathlib.Path, sources: list[pathlib.Path]) -> str | None:
    """Is `binary` older than anything it is built from?

    A stale `target/debug` binary produces a verdict about code that is
    not the code under test, and it produces it silently — the run looks
    exactly like a run that worked.
    """
    if not binary.exists():
        return f"{binary} does not exist"
    built = binary.stat().st_mtime
    newer = [s for s in sources if s.exists() and s.stat().st_mtime > built]
    if newer:
        names = ", ".join(str(s.relative_to(REPO)) for s in newer[:3])
        return f"{binary.name} predates {names}"
    return None


class Session:
    """One real compositor, isolated from the machine and from its peer."""

    def __init__(self, name: str, root: str, runtime_root: str):
        self.name = name
        self.dir = os.path.join(root, name)
        os.makedirs(self.dir, exist_ok=True)
        # Short paths. `sun_path` is 108 bytes and both the control
        # socket and the Wayland socket live under directories named
        # here — a run rooted somewhere verbose fails at bind with an
        # error about the path rather than about the length.
        self.socket = os.path.join(runtime_root, f"{name[0]}.sock")
        self.runtime = os.path.join(runtime_root, f"r{name[0]}")
        os.makedirs(self.runtime, mode=0o700, exist_ok=True)
        self.home = os.path.join(self.dir, "home")
        os.makedirs(self.home, exist_ok=True)
        env = {
            **os.environ,
            "QOS_TEST_CONTROL": self.socket,
            "QOS_STATE_PATH": os.path.join(self.dir, "config", "state.json"),
            "HOME": self.home,
            "XDG_DATA_HOME": os.path.join(self.dir, "data"),
            "XDG_CACHE_HOME": os.path.join(self.dir, "cache"),
            "XDG_STATE_HOME": os.path.join(self.dir, "state"),
            "XDG_CONFIG_HOME": os.path.join(self.dir, "config"),
            "XDG_RUNTIME_DIR": self.runtime,
            # Without its own device key a session's peer-sync turns
            # itself off, and the only visible symptom is that
            # `peer.invite` is never exported.
            "QOS_PEER_KEY_PATH": os.path.join(self.dir, "keys", "peer-key"),
            "QOS_USER_ID": name,
            "NIRIOS_HEADLESS_SIZE": "1600x1000",
            # The host is de_DE and bwrap does not clear the
            # environment, so without this the chrome renders in German.
            "LC_ALL": "en_US.UTF-8",
            "LANG": "en_US.UTF-8",
        }
        env.pop("DISPLAY", None)
        env.pop("WAYLAND_DISPLAY", None)
        self.env = env
        self.log_path = os.path.join(self.dir, "shell.log")
        self.log = open(self.log_path, "w")
        self.proc = subprocess.Popen(
            [str(SHELL), "--mode", "native", "--backend", "headless",
             "--enable-peer-sync"],
            env=env, stdout=self.log, stderr=subprocess.STDOUT,
            preexec_fn=die_with_parent(),
        )

    def wait_up(self, timeout=60.0) -> bool:
        end = time.time() + timeout
        while time.time() < end:
            if os.path.exists(self.socket):
                time.sleep(1.0)
                return True
            if self.proc.poll() is not None:
                return False
            time.sleep(0.3)
        return False

    def call(self, payload: dict, timeout=60) -> dict:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(timeout)
        s.connect(self.socket)
        s.sendall((json.dumps(payload) + "\n").encode())
        buf = b""
        while not buf.endswith(b"\n"):
            chunk = s.recv(65536)
            if not chunk:
                break
            buf += chunk
        s.close()
        return json.loads(buf.decode())

    # -- convenience -------------------------------------------------
    def state(self) -> dict:
        return self.call({"op": "state_dump"})["state"]

    def chrome(self) -> dict:
        return self.state()["chrome"]

    def peer_sync(self) -> dict:
        return self.chrome().get("peer_sync", {}) or {}

    def key(self, k: str):
        self.call({"op": "key", "key": k})

    def chord(self, k: str):
        """Send `k` with Control held, then drop the modifier.

        The mask is state on the connection, so leaving it set would
        make every later keystroke a chord — including the Enter that
        submits the prompt.
        """
        self.call({"op": "modifiers", "ctrl": True, "shift": False,
                   "alt": False, "logo": False})
        self.call({"op": "key", "key": k})
        self.call({"op": "modifiers", "ctrl": False, "shift": False,
                   "alt": False, "logo": False})

    def clipboard_read(self) -> str | None:
        return self.call({"op": "clipboard_paste"})["clipboard"]["text"]

    def clipboard_write(self, text: str):
        self.call({"op": "clipboard_copy", "text": text})

    def invoke(self, ident: str, args: dict | None = None) -> dict:
        payload = {"op": "actions_invoke", "provider": ACTIONS, "id": ident}
        if args is not None:
            payload["args"] = args
        return self.call(payload)

    def snapshot(self, path: str) -> dict:
        return self.call({"op": "snapshot", "path": path}, timeout=180)

    def stop(self):
        try:
            self.proc.terminate()
            self.proc.wait(timeout=10)
        except Exception:
            try:
                self.proc.kill()
            except Exception:
                pass
        try:
            self.log.close()
        except Exception:
            pass


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true",
                    help="leave the session roots on disk for inspection")
    ap.add_argument("--out", default=None, help="where the PNGs go")
    args = ap.parse_args()

    shots = args.out or os.path.join(tempfile.gettempdir(), "invite-clipboard")
    os.makedirs(shots, exist_ok=True)

    stale = freshness(SHELL, [
        REPO / "shell" / "src" / "shell" / "overlay_host.rs",
        REPO / "shell" / "src" / "compositor" / "selection.rs",
        REPO / "shell" / "src" / "network" / "invite.rs",
    ])
    if stale:
        print(f"REFUSING: {stale}\n  build first:  cargo build -p shell")
        return 2

    root = tempfile.mkdtemp(prefix="qinv-")
    # Separate short root for the sockets — see Session.__init__.
    runtime_root = tempfile.mkdtemp(prefix="/tmp/qs-")
    quinten = Session("quinten", root, runtime_root)
    xaver = Session("xaver", root, runtime_root)
    try:
        step("two compositors, separate identities and device keys")
        if not check("quinten's session came up", quinten.wait_up()):
            print(open(quinten.log_path).read()[-3000:])
            return 1
        if not check("xaver's session came up", xaver.wait_up()):
            print(open(xaver.log_path).read()[-3000:])
            return 1

        step("quinten mints an invite")
        quinten.invoke("peer.invite")
        token = wait_for(lambda: quinten.peer_sync().get("invite_token"), timeout=30)
        if not check("a token was minted", bool(token),
                     f"{len(token or '')} chars"):
            return 1
        quinten.snapshot(os.path.join(shots, "01-quinten-invite-card.png"))

        step("quinten presses Ctrl+C")
        quinten.chord("c")
        time.sleep(0.5)
        copied = wait_for(lambda: quinten.clipboard_read(), timeout=10)
        check("the token is on quinten's system clipboard",
              copied == token,
              "clipboard is empty" if not copied
              else ("matches the minted token" if copied == token
                    else f"clipboard holds something else ({len(copied)} chars)"))
        status = quinten.peer_sync().get("status") or ""
        check("the card says the copy happened",
              "copied" in status.lower(), repr(status))
        quinten.snapshot(os.path.join(shots, "02-quinten-copied.png"))

        if not copied:
            return 1

        step("the token travels — by hand, off quinten's clipboard")
        # Deliberately `copied`, not `token`: what crosses is what the
        # copy produced, so a copy that silently truncated would fail
        # the join rather than pass on the value it never wrote.
        xaver.clipboard_write(copied)
        check("it is on xaver's clipboard now",
              xaver.clipboard_read() == copied)

        step("xaver opens the connect prompt and presses Ctrl+V")
        xaver.invoke("peer.connect")
        if not check("the prompt opened",
                     wait_for(lambda: xaver.peer_sync().get("connect_input")
                              is not None, timeout=15) is not None):
            return 1
        xaver.chord("v")
        time.sleep(0.5)
        pasted = xaver.peer_sync().get("connect_input")
        check("the prompt filled itself from the clipboard",
              pasted == token,
              "prompt is still empty — paste did nothing" if not pasted
              else ("holds the whole token" if pasted == token
                    else f"holds {len(pasted)} chars, token is {len(token)}"))
        xaver.snapshot(os.path.join(shots, "03-xaver-pasted.png"))

        step("xaver joins")
        xaver.key("Enter")
        joined = wait_for(
            lambda: (xaver.peer_sync().get("status") or "").startswith("Joined"),
            timeout=30)
        check("xaver joined quinten's room", bool(joined),
              repr(xaver.peer_sync().get("status")))
        xaver.snapshot(os.path.join(shots, "04-xaver-joined.png"))

        step("and the negative claim")
        check("the token was never typed — only copied and pasted", True,
              "this script issues no `type` op at all; the prompt's "
              "contents came from the clipboard or the run failed above")

        print(f"\nshots in {shots}")
        for name in sorted(os.listdir(shots)):
            print(f"  {os.path.join(shots, name)}")
        print()
        if failures:
            print(f"FAILED: {len(failures)}")
            for f in failures:
                print(f"  - {f}")
            return 1
        print("all claims hold")
        return 0
    finally:
        xaver.stop()
        quinten.stop()
        if args.keep:
            print(f"session roots kept at {root}")
        else:
            shutil.rmtree(root, ignore_errors=True)
            shutil.rmtree(runtime_root, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
