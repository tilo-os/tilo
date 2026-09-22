#!/usr/bin/env python3
"""A window one person opens appears on the other's screen.

`installed-app-collab-proof.py` has both people open the app themselves
and proves their documents converge. That leaves the beat before it
unproven, and it was broken: the tab a peer's window arrived as was a
card reading the app id over "Settings coming soon", because a peer's
`wl_surface` id means nothing here and the tile was degraded to a stub.
It looked fine in that harness only because the person looking at the
card had also opened their own copy, one tile further down.

So here **only alice opens anything**. Whatever appears on bob's screen
appeared because the room said the window existed and bob's machine
opened its own instance of the app against the same room.

What it checks, in order:

  1. bob's tab for alice's window stops being a card and becomes a
     surface, asserted on the tile in bob's tree.
  2. bob's compositor has taken frames from that surface (`frame_seq >
     0`), so it is a painting window and not a placeholder.
  3. exactly one word-processor window on bob's side. A tab that got a
     card *and* a window would be the bug wearing a disguise.
  4. alice types, and the words are on bob's page.

Both screenshots still have to be looked at. Exiting zero means the
assertions held, not that the pixels are right.

    cargo build -p shell -p install -p launcher -p word-processor
    scripts/headless-examples/peer-window-opens-here-proof.py
"""
from __future__ import annotations

import argparse
import base64
import json
import os
import pathlib
import socket
import subprocess
import sys
import tempfile
import time

from reaper import die_with_parent, track

REPO = pathlib.Path(__file__).resolve().parents[2]
APP = "so.tilo.word-processor"
APPLICATIONS = "applications"

failures: list[str] = []


def tool(name: str) -> pathlib.Path | None:
    """The newest build of `name`, not the first profile that has one.

    Taking `demo` whenever it exists is how a run gets driven by a build
    from hours ago while the sources under test sit uncompiled, and a
    stale compositor fails a replication assertion for reasons that have
    nothing to do with replication — it simply does not contain the code.
    The newest is the one the person running this just built.
    """
    found = [REPO / "target" / profile / name for profile in ("demo", "debug")]
    found = [p for p in found if p.exists()]
    if not found:
        return None
    return max(found, key=lambda p: p.stat().st_mtime)


# The files whose behaviour this run asserts on. Named rather than
# globbed: the whole tree is edited by other work all day, and a guard
# that fires on any of it would be ignored within an hour, which is the
# same as not having one. These are the ones where "the binary predates
# the source" means this run cannot tell you anything.
UNDER_TEST = (
    "crates/session/shell/src/shell/peer_windows.rs",
    "crates/session/shell/src/shell/state.rs",
    "crates/session/shell/src/shell/mod.rs",
    "crates/session/shell/src/compositor/tab_binding.rs",
    "crates/session/shell/src/compositor/frame.rs",
    "crates/session/shell/src/shell/launcher.rs",
    "crates/sdk/ui-core/src/widget/tile_host.rs",
)


def stale(binary: pathlib.Path, *sources: str) -> str | None:
    """Whether `binary` predates any of the sources this run is about."""
    behind = []
    for rel in sources:
        f = REPO / rel
        try:
            if f.stat().st_mtime > binary.stat().st_mtime:
                behind.append(rel)
        except OSError:
            pass
    if behind:
        return f"{binary} predates {', '.join(behind)}"
    return None


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'ok  ' if ok else 'FAIL'}  {label}" + (f"  — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return ok


def note(text: str) -> None:
    print(f"        {text}")


def wait_for(pred, timeout=40.0, poll=0.4):
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


class Session:
    """One real compositor, isolated from the machine and from its peer."""

    def __init__(self, name: str, root: str, apps_dir: str):
        self.name = name
        self.dir = os.path.join(root, name)
        os.makedirs(self.dir, exist_ok=True)
        self.socket = os.path.join(self.dir, "control.sock")
        self.apps_dir = apps_dir
        runtime = os.path.join(self.dir, "run")
        os.makedirs(runtime, mode=0o700, exist_ok=True)
        home = os.path.expanduser("~")
        env = {
            **os.environ,
            # The host is de_DE and bwrap does not clear the environment,
            # so without this the app's chrome comes up in German and a
            # screenshot cannot be read by whoever it is for.
            "LC_ALL": "en_US.UTF-8",
            "LANG": "en_US.UTF-8",
            "TILO_TEST_CONTROL": self.socket,
            "TILO_STATE_PATH": os.path.join(self.dir, "config", "state.json"),
            "XDG_DATA_HOME": os.path.join(self.dir, "data"),
            "XDG_CACHE_HOME": os.path.join(self.dir, "cache"),
            "XDG_STATE_HOME": os.path.join(self.dir, "state"),
            "XDG_CONFIG_HOME": os.path.join(self.dir, "config"),
            "XDG_RUNTIME_DIR": runtime,
            "TILO_APPS_DATA_DIR": apps_dir,
            "TILO_RUNTIME_BUNDLE_DIR": os.path.join(home, ".local/share/tilo-runtime"),
            # Without its own device key a session's peer-sync turns
            # itself off, and the only symptom is a missing peer.* action.
            "TILO_PEER_KEY_PATH": os.path.join(self.dir, "keys", "peer-key"),
            "TILO_HEADLESS_SIZE": "1280x1400",
            "RUST_LOG": "tilo.state=debug",
            "RUST_LOG": "info",
        }
        for var, binary in (("TILO_INSTALL_BIN", "tilo-install"),
                            ("TILO_LAUNCHER_BIN", "tilo-launcher")):
            found = tool(binary)
            if found:
                env[var] = str(found)
        env.pop("DISPLAY", None)
        env.pop("WAYLAND_DISPLAY", None)
        self.log_path = os.path.join(self.dir, "shell.log")
        self.log = open(self.log_path, "w")
        self.proc = track(subprocess.Popen(
            [str(tool("shell")), "--backend", "headless", "--enable-peer-sync"],
            env=env, stdout=self.log, stderr=subprocess.STDOUT,
            preexec_fn=die_with_parent(),
        ))

    def wait_up(self, timeout=120.0) -> bool:
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

    def state(self) -> dict:
        return self.call({"op": "state_dump"})["state"]

    def chrome(self) -> dict:
        return self.state()["chrome"]

    def tree(self) -> dict:
        return self.state()["workspace_tree"]

    def invoke(self, ident: str, provider: str = "actions") -> dict:
        return self.call({"op": "actions_invoke", "provider": provider, "id": ident})

    def frames(self) -> list[dict]:
        r = self.call({"op": "foreign_frames"})
        if "foreign_frames" in r:
            return r["foreign_frames"].get("frames", [])
        return r.get("answer", r.get("frames", [])) or []

    def snapshot(self, path: str) -> bool:
        self.call({"op": "snapshot", "path": path})
        for _ in range(40):
            if os.path.exists(path) and os.path.getsize(path) > 0:
                return True
            time.sleep(0.25)
        return False

    def log_text(self) -> str:
        try:
            return open(self.log_path, errors="replace").read()
        except OSError:
            return ""

    def collab_state(self) -> str:
        hits = [ln for ln in self.log_text().splitlines() if "collab connected" in ln]
        return hits[-1].strip() if hits else "no `collab connected` line in the log"

    def stop(self):
        for attempt in (self.proc.terminate, self.proc.kill):
            try:
                attempt()
                self.proc.wait(timeout=8)
                break
            except Exception:
                continue
        try:
            self.log.close()
        except Exception:
            pass


def tiles(session: Session) -> list[dict]:
    """Every tab in every workspace, as `{title, tile}`.

    Read from the compositor's own serialised tree rather than inferred
    from a screenshot: the difference between a card and a window is a
    tile variant, and that is the thing under test.
    """
    out = []
    for ws in session.tree().get("workspaces", []):
        for strip in ws.get("strips", []):
            for group in strip.get("window_groups", []):
                for tab in group.get("tabs", []):
                    out.append({"title": tab.get("title"), "tile": tab.get("tile")})
    return out


def variant(tile) -> str:
    if isinstance(tile, dict):
        return next(iter(tile), "?")
    return str(tile)


def word_processor_tiles(session: Session) -> list[dict]:
    hits = []
    for t in tiles(session):
        blob = json.dumps(t.get("tile"))
        if APP in blob:
            hits.append(t)
    return hits


def open_from_palette(s: Session, timeout: float = 120.0) -> dict | None:
    before = {f.get("surface_id") for f in s.frames()}
    resp = s.invoke(APP, provider=APPLICATIONS)
    if "err" in resp:
        note(f"{s.name}: invoke refused — {json.dumps(resp['err'])}")
        return None
    end = time.time() + timeout
    while time.time() < end:
        for f in s.frames():
            if f.get("surface_id") not in before and f.get("frame_seq", 0) > 0:
                return f
        time.sleep(0.5)
    return None


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    # Lower case only: the control socket's `type` has no shift, so a
    # capital is silently dropped and the run looks like a replication
    # failure when it is a typing one.
    ap.add_argument("--text", default="the window opened on both screens")
    ap.add_argument("--keep", action="store_true")
    args = ap.parse_args()

    live_apps = REPO / "target" / "tilo-apps-live"
    payload = live_apps / "payloads" / APP / "bin" / "word-processor"
    missing = [what for what, path in (
        ("shell", tool("shell")), ("tilo-launcher", tool("tilo-launcher")),
        ("the word-processor payload", payload),
    ) if not path or not pathlib.Path(path).exists()]
    if missing:
        print("missing: " + ", ".join(missing))
        print("build:  cargo build -p shell -p install -p launcher -p word-processor")
        return 2

    # A stale payload is indistinguishable from broken replication: an
    # older build announces its collaboration channel under a different
    # key, so deltas arrive and match nothing.
    built = REPO / "target" / "debug" / "word-processor"
    if built.exists() and payload.stat().st_mtime < built.stat().st_mtime:
        print(f"{payload} is older than {built} — re-copy the payload before trusting "
              f"this run")
        return 2

    # And the same question about the compositor, which is the thing
    # under test here. A run against a binary that predates the change
    # reports the old behaviour as a failure of the new one.
    shell_bin = tool("shell")
    why = stale(pathlib.Path(shell_bin), *UNDER_TEST) if shell_bin else None
    if why:
        print(why)
        print("rebuild before trusting this run — a binary without the change under")
        print("test reports the old behaviour as a failure of the new one")
        return 2
    note(f"compositor: {shell_bin}")

    root = tempfile.mkdtemp(prefix="/tmp/qs-")
    shots = os.path.join(root, "shots")
    os.makedirs(shots, exist_ok=True)
    print(f"session root: {root}")

    sessions: list[Session] = []
    try:
        print("\n[1] two sessions, both with the word processor on their machine")
        apps = {}
        for who in ("alice", "bob"):
            apps[who] = os.path.join(root, f"{who}-apps")
            subprocess.run(["cp", "-a", "--reflink=auto", str(live_apps), apps[who]],
                           check=True)
        alice = Session("alice", root, apps["alice"])
        sessions.append(alice)
        bob = Session("bob", root, apps["bob"])
        sessions.append(bob)
        for s in sessions:
            if not check(f"{s.name} came up", s.wait_up(), f"see {s.log_path}"):
                return 1

        print("\n[2] one room, formed the way a person forms one")
        exported = {a["id"] for a in alice.call(
            {"op": "actions_enumerate", "query": ""}).get("answer", {}).get("actions", [])}
        if not check("peer-sync is on", "peer.invite" in exported):
            note(f"check {alice.log_path} for 'device key load failed'")
            return 1
        alice.invoke("peer.invite")
        token = wait_for(lambda: alice.chrome()["peer_sync"]["invite_token"], timeout=30)
        if not check("alice minted an invite", bool(token)):
            return 1
        alice.call({"op": "key", "key": "Escape"})
        room = json.loads(base64.urlsafe_b64decode(token + "=" * (-len(token) % 4)))["room_id"]
        bob.invoke("peer.connect")
        wait_for(lambda: bob.chrome()["peer_sync"]["connect_input"] is not None, timeout=25)
        bob.call({"op": "type", "text": token})
        bob.call({"op": "key", "key": "Enter"})
        joined = wait_for(
            lambda: (bob.chrome()["peer_sync"]["status"] or "").startswith("Joined"),
            timeout=30)
        if not check("bob joined alice's room", bool(joined)):
            return 1
        bob.call({"op": "key", "key": "Escape"})
        note(f"room = {room}")

        check("bob has opened nothing", not word_processor_tiles(bob),
              json.dumps(word_processor_tiles(bob))[:200])

        print("\n[3] alice opens the word processor. bob opens nothing.")
        a_win = open_from_palette(alice)
        if not check("alice's word processor painted", a_win is not None,
                     json.dumps(a_win) if a_win else f"see {alice.log_path}"):
            return 1

        print("\n[4] what bob's machine does about it")
        # The tab has to arrive first, then stop being a card. Both are
        # waited for separately so a failure says which half broke.
        arrived = wait_for(lambda: word_processor_tiles(bob), timeout=90)
        if not check("the room told bob the window exists", bool(arrived),
                     "no tab naming the app in bob's tree"):
            return 1
        note("bob's tab: " + ", ".join(
            f"{t['title']!r} → {variant(t['tile'])}" for t in arrived))

        became_real = wait_for(
            lambda: [t for t in word_processor_tiles(bob)
                     if variant(t["tile"]) == "Foreign"],
            timeout=180)
        if not check("bob's tab hosts a real window, not a card", bool(became_real),
                     json.dumps(word_processor_tiles(bob))[:400]):
            note(f"see {bob.log_path}")
            return 1

        surface = became_real[0]["tile"]["Foreign"]["surface_id"]
        painting = wait_for(
            lambda: next((f for f in bob.frames()
                          if f.get("surface_id") == surface
                          and f.get("frame_seq", 0) > 0), None),
            timeout=120)
        check("bob's compositor is taking frames from it", painting is not None,
              json.dumps(painting) if painting else json.dumps(bob.frames())[:300])

        held = word_processor_tiles(bob)
        check("exactly one word-processor window on bob's side", len(held) == 1,
              ", ".join(f"{t['title']!r} → {variant(t['tile'])}" for t in held))
        check("no card left behind",
              not any(variant(t["tile"]) == "PeerWindow" for t in held),
              json.dumps(held)[:300])

        # Both apps have to have attached to their broker before either
        # types, or the first keystrokes go out to nobody.
        time.sleep(8.0)
        for s in sessions:
            note(f"{s.name}: {s.collab_state()}")

        print(f"\n[5] alice types {args.text!r}")
        # No click first: the word processor opens with the caret in the
        # document body, and a click at "the middle of the window" lands
        # on the style panel and moves focus off the page.
        alice.call({"op": "type", "text": args.text})

        print("\n[6] what each screen shows")
        time.sleep(14.0)
        a_png = os.path.join(shots, "alice-typed.png")
        b_png = os.path.join(shots, "bob-opened-here.png")
        check("alice's screen was captured", alice.snapshot(a_png), a_png)
        check("bob's screen was captured", bob.snapshot(b_png), b_png)

        print()
        print("  alice (opened, typed):  " + a_png)
        print("  bob   (opened here):    " + b_png)
        print()
        print("  Bob opened nothing. His window is there because the room said")
        print(f"  alice had one, and it should read {args.text!r} —")
        print("  a real toolbar and page, never a card.")
        print()
    finally:
        if not args.keep:
            for s in sessions:
                s.stop()

    if failures:
        print(f"FAILED: {len(failures)} — " + "; ".join(failures))
        return 1
    print("every assertion held. Now look at the two screenshots.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
