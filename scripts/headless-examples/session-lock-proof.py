#!/usr/bin/env python3
"""The session's lock, end to end, in a headless session of its own.

One desk locks, refuses a password that is not the person's, and comes back
when the person's own is typed — every step read off the session itself, never
from the thing that asked for it:

  lock    the desk's own lock action locks the session, and the lock screen the
          session starts for it puts its own frame on the displays
  wrong   a password that is not the person's is checked and refused: the
          screen stays locked and keeps covering the desk
  unlock  the person's own password lifts it: the desk is back, and the screen
          that covered it is gone

The password check is a stand-in: a container has neither the installed
helper's PAM stack nor a login to check, so the session is given one with the
development-only `TILO_UNLOCK_HELPER` seam, which answers as the installed
helper does (0 for the person's password, 1 for another). It writes every
password it was handed to a file, so the proof reads what the session actually
asked it, not what the screen said it asked.

Usage:
  ./session-lock-proof.py
"""
import json
import os
import pathlib
import shutil
import socket
import subprocess
import sys
import tempfile
import time

REPO = pathlib.Path(__file__).resolve().parents[2]
# The one place that picks a session's private runtime directory.
SESSION = REPO / "dist" / "dev" / "nested-session.sh"
# The nightly step points this at the binary Cargo built for the run.
SHELL = pathlib.Path(os.environ.get("TILO_SHELL_BINARY", REPO / "target" / "debug" / "tilo-session"))
PASSWORD = "correct horse battery staple"
WRONG = "not the person's password"

failures: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return ok


def runtime_directory(state: pathlib.Path) -> pathlib.Path:
    """The private runtime directory a session on `state` uses.

    The launcher picks it (a login's XDG_RUNTIME_DIR, the user's /run/user
    seat, or a short directory of its own) and prints it, so no second copy of
    that rule lives here."""
    printed = subprocess.run(
        [str(SESSION), "--print-runtime", "--state", str(state)],
        capture_output=True, text=True, check=True,
    )
    runtime = printed.stdout.strip()
    if not runtime:
        raise SystemExit("the session launcher named no runtime directory")
    return pathlib.Path(runtime)


class Desk:
    """A headless session on a scratch home of its own, driven at its socket."""

    def __init__(self) -> None:
        self.dir = pathlib.Path(tempfile.mkdtemp(prefix="tilo-lock-proof-"))
        self.home = self.dir / "home"
        self.identity = self.dir / "identity"
        for path in (self.home, self.identity):
            path.mkdir(parents=True, exist_ok=True)
        self.identity.chmod(0o700)
        # The desk's state is this scratch directory, as a launcher's state is
        # a session's: the runtime directory it answers in comes from there.
        self.runtime = runtime_directory(self.dir)
        self.runtime.mkdir(parents=True, mode=0o700, exist_ok=True)
        self.socket = self.runtime / "test-control.sock"
        self.attempts = self.dir / "attempts"
        self.helper = self.dir / "unlock-helper"
        self.helper.write_text(
            "#!/bin/sh\n"
            "# The installed helper's answers, with every password kept for the proof.\n"
            'read -r given || true\n'
            f'printf "%s\\n" "$given" >> "{self.attempts}"\n'
            f'[ "$given" = "{PASSWORD}" ] && exit 0\n'
            "exit 1\n"
        )
        self.helper.chmod(0o755)
        self.env = {
            **os.environ,
            "HOME": str(self.home),
            "TILO_TEST_CONTROL": str(self.socket),
            "TILO_UNLOCK_HELPER": str(self.helper),
            # The identity this desk opens with is this scratch directory's,
            # never the machine's: the shared tree is refused a throwaway.
            "TILO_IDENTITY_ROOT": str(self.identity),
            "XDG_RUNTIME_DIR": str(self.runtime),
            "XDG_DATA_HOME": str(self.dir / "data"),
            "XDG_CACHE_HOME": str(self.dir / "cache"),
            "XDG_STATE_HOME": str(self.dir / "state"),
            "XDG_CONFIG_HOME": str(self.dir / "config"),
        }
        self.env.pop("DISPLAY", None)
        self.env.pop("WAYLAND_DISPLAY", None)
        self.session = None

    def start(self) -> None:
        """Start the session and wait until it answers on its socket."""
        self.socket.unlink(missing_ok=True)
        self.session = subprocess.Popen(
            [str(SHELL), "--backend", "headless"],
            env=self.env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
        )
        end = time.time() + 60
        while time.time() < end:
            if self.socket.exists():
                return
            if self.session.poll() is not None:
                raise SystemExit(f"the session exited at startup: {self.session.stderr.read()}")
            time.sleep(0.2)
        raise SystemExit("the session never answered on its socket")

    def stop(self) -> None:
        if self.session is None:
            return
        self.session.terminate()
        try:
            self.session.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.session.kill()
            self.session.wait()
        self.session = None

    def call(self, request: dict, timeout: float = 120.0) -> dict:
        """One request, one answer: the socket speaks a line of JSON each way."""
        with socket.socket(socket.AF_UNIX) as client:
            client.settimeout(timeout)
            client.connect(str(self.socket))
            client.sendall(json.dumps(request).encode() + b"\n")
            answer = b""
            while not answer.endswith(b"\n"):
                chunk = client.recv(1 << 20)
                if not chunk:
                    break
                answer += chunk
        return json.loads(answer)

    def chrome(self) -> dict:
        return (self.call({"op": "state_dump"}).get("state") or {}).get("chrome") or {}

    def locked(self) -> bool:
        return self.chrome().get("session_locked") is True

    def screen_drawn(self) -> bool:
        """Whether the lock screen's own frame is what the displays show."""
        return self.chrome().get("lock_screen_drawn") is True

    def asked(self) -> list:
        """Every password the session handed its helper, in order."""
        if not self.attempts.exists():
            return []
        return self.attempts.read_text().splitlines()

    def lock_action(self) -> dict | None:
        """The session's own lock action, as the desk publishes it."""
        rows = (self.call({"op": "actions_enumerate", "query": "lock"}).get("answer")
                or {}).get("actions") or []
        return next((row for row in rows
                     if row.get("resource") == "session:screen"
                     and row.get("operation") == "lock"), None)


def until(check, timeout: float = 30.0) -> bool:
    end = time.time() + timeout
    while time.time() < end:
        if check():
            return True
        time.sleep(0.2)
    return False


def main() -> int:
    if not SHELL.exists():
        print(f"FAIL: no session binary at {SHELL}", file=sys.stderr)
        return 1

    desk = Desk()
    try:
        # A development desk opens on first-use setup; the throwaway identity a
        # development session opens its desk with instead is minted here, so
        # this one starts on the desk whose lock the proof locks.
        print("\n[0] a desk of its own")
        desk.start()
        minted = desk.call({"op": "provision_test_identity"})
        check("a development desk mints its throwaway identity",
              bool((minted.get("answer") or {}).get("created")), json.dumps(minted)[:120])
        desk.stop()
        desk.start()
        check("the desk answers, with no lock on it", not desk.locked())
        check("and no lock screen covers it", not desk.screen_drawn())

        print("\n[1] the desk locks itself, and its lock screen stands over it")
        lock = desk.lock_action()
        check("the desk publishes its own lock action", lock is not None)
        desk.call({"op": "actions_invoke",
                   "provider": (lock or {}).get("provider", ""),
                   "id": (lock or {}).get("id", "")})
        check("the session is locked", until(desk.locked))
        # What a person sees: not the desk's last frame, but the lock screen's
        # own, on every display. Read off the compositor, which only says so
        # once the lock client attached and committed a buffer.
        check("the lock screen's own frame is on the displays", until(desk.screen_drawn))

        print("\n[2] another password is refused, and the screen stays shut")
        desk.call({"op": "type", "text": WRONG})
        desk.call({"op": "key", "key": "Enter"})
        check("the session asks the helper", until(lambda: len(desk.asked()) >= 1),
              f"{len(desk.asked())} attempt(s)")
        check("the helper was handed what was typed", desk.asked()[:1] == [WRONG],
              ", ".join(desk.asked())[:80])
        check("the session stays locked", desk.locked())
        check("and the lock screen still covers the desk", desk.screen_drawn())

        print("\n[3] the person's own password brings the desk back")
        desk.call({"op": "type", "text": PASSWORD})
        desk.call({"op": "key", "key": "Enter"})
        check("the session unlocks", until(lambda: not desk.locked()))
        check("with the password that was typed", desk.asked()[1:2] == [PASSWORD],
              ", ".join(desk.asked())[:80])
        check("the lock screen is gone", until(lambda: not desk.screen_drawn()))
        state = desk.call({"op": "state_dump"}).get("state") or {}
        workspaces = (state.get("workspace_tree") or {}).get("workspaces") or []
        check("the desk behind it is readable again", bool(workspaces),
              ", ".join(str(workspace.get("name", "")) for workspace in workspaces))
    finally:
        desk.stop()
        shutil.rmtree(desk.runtime, ignore_errors=True)
        shutil.rmtree(desk.dir, ignore_errors=True)

    print()
    if failures:
        print(f"FAILED: {len(failures)} check(s): {', '.join(failures)}")
        return 1
    print("The lock holds a person out and lets them back in. Every claim read off the session.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
