#!/usr/bin/env python3
"""An app *installed* through the catalogue, collaborating.

`word-processor-convergence-proof.py` proves two word processors
converge when both were already on the machine and both were opened
from the palette. That is not the ordinary path. The ordinary path is
the one a person takes the first time they use anything: they find an
app they do not have, they install it, and they open it.

That path was broken, and quietly. The local test registry published a
manifest it made up instead of the one the payload was built with, so
the installed package declared no `[sandbox.services]`. The launcher
then opened no collab socket and handed the app no
`QOS_SVC_COLLAB_FD`, and the toolkit fell back to its disabled
backend. No warning: every service a package declares is optional, and
a service that was never declared is not even an absence. Presence
rides the compositor rather than the app's socket, so a peer's caret
still crossed and painted — on a page that stayed empty.

So this proof deliberately differs from the convergence one in exactly
one respect:

  alice's word processor is fetched, verified and installed by the
  catalogue during the run, and opened by the install path.

Bob's comes from the machine's own store, opened from the palette —
the path already known to work, so what the run tests is alice's.

What it produces is two screenshots. Look at them. Bob's page has to
carry the words alice typed; a caret over a blank page is the failure
this proves against.

    scripts/headless-examples/installed-app-collab-proof.py

    cargo build -p shell -p install -p launcher -p word-processor
    cargo build -p app-catalogue --bins --examples

Exits non-zero if a step fails; a successful exit still means someone
has to look at the pixels.
"""
from __future__ import annotations

import argparse
import base64
import json
import os
import pathlib
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time

from reaper import die_with_parent, track

REPO = pathlib.Path(__file__).resolve().parents[2]
APP = "dev.qos.word-processor"
DISPLAY_NAME = "Word Processor"
APPLICATIONS = "applications"
QUERY = "word processor"

failures: list[str] = []


def tool(name: str) -> pathlib.Path | None:
    for profile in ("demo", "debug"):
        p = REPO / "target" / profile / name
        if p.exists():
            return p
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
    """One real compositor, isolated from the machine and from its peer.

    `apps_dir` is the one thing the two sessions deliberately do not
    share: alice gets an empty one so the app is genuinely absent and
    has to be installed, bob gets the machine's so his side is the
    known-good path.
    """

    def __init__(self, name: str, root: str, apps_dir: str,
                 store_config: str | None = None):
        self.name = name
        self.dir = os.path.join(root, name)
        os.makedirs(self.dir, exist_ok=True)
        self.socket = os.path.join(self.dir, "control.sock")
        self.apps_dir = apps_dir
        runtime = os.path.join(self.dir, "run")
        os.makedirs(runtime, mode=0o700, exist_ok=True)
        self.runtime = runtime
        home = os.path.expanduser("~")
        env = {
            **os.environ,
            "QOS_TEST_CONTROL": self.socket,
            "QOS_STATE_PATH": os.path.join(self.dir, "config", "state.json"),
            "XDG_DATA_HOME": os.path.join(self.dir, "data"),
            "XDG_CACHE_HOME": os.path.join(self.dir, "cache"),
            "XDG_STATE_HOME": os.path.join(self.dir, "state"),
            "XDG_CONFIG_HOME": os.path.join(self.dir, "config"),
            "XDG_RUNTIME_DIR": runtime,
            "QOS_APPS_DATA_DIR": apps_dir,
            # Machine state an install does not fetch and a launch
            # cannot do without.
            "QOS_RUNTIME_BUNDLE_DIR": os.path.join(home, ".local/share/qos-runtime"),
            # Without its own device key a session's peer-sync turns
            # itself off, and the only symptom is a missing peer.* action.
            "QOS_PEER_KEY_PATH": os.path.join(self.dir, "keys", "peer-key"),
            # Tall enough that two tiles — this session's app and the
            # peer's, which sharing a workspace places here too — both
            # still show a page rather than only their toolbars.
            "NIRIOS_HEADLESS_SIZE": "1280x1400",
            # The app prints `collab connected=` under this, which is
            # the line that told us which path was disabled.
            "QUICK_COLLAB_DEBUG": "1",
            "RUST_LOG": "info",
        }
        if store_config is not None:
            env["QOS_STORE_CONFIG_DIR"] = store_config
            env["QOS_STORE_CACHE_DIR"] = os.path.join(self.dir, "store-cache")
            os.makedirs(env["QOS_STORE_CACHE_DIR"], exist_ok=True)
        for var, binary in (("QOS_CATALOGUE_BIN", "qos-catalogue"),
                            ("QOS_INSTALL_BIN", "qos-install"),
                            ("QOS_LAUNCHER_BIN", "qos-launcher")):
            found = tool(binary)
            if found:
                env[var] = str(found)
        # A window on the operator's screen is not a headless run.
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

    def chrome(self) -> dict:
        return self.call({"op": "state_dump"})["state"]["chrome"]

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
        """The app's own verdict on its broker connection, or why there
        isn't one yet. Read rather than asserted: this line is the
        difference between 'the document did not cross' and 'the app
        was never connected to anything'."""
        hits = [ln for ln in self.log_text().splitlines() if "collab connected" in ln]
        return hits[-1].strip() if hits else "no `collab connected` line in the log"

    def stop(self):
        try:
            self.proc.terminate()
            self.proc.wait(timeout=8)
        except Exception:
            try:
                self.proc.kill()
            except Exception:
                pass
        try:
            self.log.close()
        except Exception:
            pass


class Registry:
    """`serve-test-registry` on loopback, serving one real signed `.qpkg`.

    It packs the manifest it finds beside the payload, so the package it
    serves is the package the payload belongs to — services and all.
    """

    def __init__(self, example_bin, payload, config_dir, log_path):
        self.example_bin, self.payload = example_bin, payload
        self.config_dir, self.log_path = config_dir, log_path
        self.proc = self.log = self.base_url = None

    def __enter__(self):
        os.makedirs(self.config_dir, exist_ok=True)
        self.log = open(self.log_path, "w+b")
        self.proc = track(subprocess.Popen(
            [str(self.example_bin), "--config-dir", str(self.config_dir),
             "--payload", str(self.payload), "--package-id", APP,
             "--name", DISPLAY_NAME],
            stdout=self.log, stderr=subprocess.STDOUT,
            preexec_fn=die_with_parent(),
        ))
        deadline = time.time() + 300.0
        while time.time() < deadline:
            if self.proc.poll() is not None:
                raise SystemExit(f"the registry exited early; see {self.log_path}")
            text = open(self.log_path, errors="replace").read()
            m = re.search(r"registry (http://\S+) serving", text)
            if m:
                self.base_url = m.group(1)
                if "publishing the package manifest at" not in text:
                    raise SystemExit(
                        "the registry synthesised a manifest instead of publishing the "
                        f"package's own — an app installed from it cannot collaborate. "
                        f"See {self.log_path}")
                return self
            time.sleep(0.25)
        raise SystemExit(f"the registry never announced a port; see {self.log_path}")

    def __exit__(self, *exc):
        if self.proc is not None:
            try:
                self.proc.terminate()
                self.proc.wait(timeout=10)
            except Exception:
                try:
                    self.proc.kill()
                except Exception:
                    pass
        if self.log is not None:
            self.log.close()
        return False


def find_row(pal: dict) -> dict | None:
    for row in pal.get("rows", []):
        if APP in json.dumps(row):
            return row
    return None


def open_palette(s: Session) -> bool:
    """Get the palette on screen, and say how.

    Forming the room leaves the chrome somewhere other than at rest, and
    a modifier mask set through the control socket is not the same thing
    as a modifier key held down — which of the two the hotkey reads is
    not something a harness should have to know. So try the ways a
    person could press it and report which one landed, rather than
    asserting on one and calling a chrome state a palette failure.
    """
    attempts = (
        ("Super+K (mask)", lambda: (
            s.call({"op": "modifiers", "ctrl": False, "shift": False,
                    "alt": False, "logo": True}),
            s.call({"op": "key", "key": "k"}),
            s.call({"op": "modifiers", "ctrl": False, "shift": False,
                    "alt": False, "logo": False}))),
        ("Super+K (held)", lambda: (
            s.call({"op": "key_down", "key": "Super"}),
            s.call({"op": "key", "key": "k"}),
            s.call({"op": "key_up", "key": "Super"}))),
        ("Ctrl+K (held)", lambda: (
            s.call({"op": "key_down", "key": "Control"}),
            s.call({"op": "key", "key": "k"}),
            s.call({"op": "key_up", "key": "Control"}))),
    )
    for label, press in attempts:
        for _ in range(2):
            s.call({"op": "key", "key": "Escape"})
            time.sleep(0.4)
            press()
            time.sleep(0.8)
            if s.chrome().get("palette", {}).get("visible") is True:
                note(f"palette opened with {label}")
                return True
    ch = s.chrome()
    note("no keystroke opened the palette")
    note(f"chrome overlays: {json.dumps(ch.get('overlays'))[:300]}")
    note(f"chrome palette:  {json.dumps(ch.get('palette'))[:300]}")
    return False


def install_and_open(s: Session, timeout: float = 300.0) -> dict | None:
    """Find the app in the palette, install it, and wait for its frame.

    The row is found by typing, the way a person finds it. The catalogue
    answers from a cache that is empty on a cold start, so the query is
    nudged until the row appears rather than asserted on the first try.
    """
    before = {f.get("surface_id") for f in s.frames()}
    if not open_palette(s):
        return None
    time.sleep(1.5)
    s.call({"op": "type", "text": QUERY})
    time.sleep(0.8)

    deadline = time.time() + 60
    row = pal = None
    while time.time() < deadline:
        pal = s.chrome().get("palette", {})
        row = find_row(pal)
        if row is not None:
            break
        s.call({"op": "key", "key": "Backspace"})
        time.sleep(0.25)
        s.call({"op": "type", "text": QUERY[-1]})
        time.sleep(0.6)
    if row is None:
        note(f"no palette row for {APP}; last rows: {json.dumps((pal or {}).get('rows', []))[:300]}")
        return None
    if pal.get("selected_id") != row.get("id"):
        note(f"Enter would activate {pal.get('selected_id')!r}, not {row.get('id')!r}")
        return None

    registry_toml = pathlib.Path(s.apps_dir) / "registry.toml"
    s.call({"op": "key", "key": "Enter"})
    installed = wait_for(
        lambda: registry_toml.exists() and APP in registry_toml.read_text(errors="replace"),
        timeout=timeout, poll=0.2)
    if not installed:
        note(f"the catalogue never installed {APP}; see {s.log_path}")
        return None
    note("the catalogue fetched, verified and installed it")
    end = time.time() + timeout
    while time.time() < end:
        for f in s.frames():
            if f.get("surface_id") not in before and f.get("frame_seq", 0) > 0:
                return f
        time.sleep(0.5)
    return None


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
    # Lower case, no punctuation that needs a modifier: the control
    # socket's `type` has no shift, so a capital is silently dropped and
    # a run can look like a replication failure when it is a typing one.
    ap.add_argument("--text", default="installed and collaborating")
    ap.add_argument("--keep", action="store_true")
    args = ap.parse_args()

    shell = tool("shell")
    example = REPO / "target" / "debug" / "examples" / "serve-test-registry"
    payload = (REPO / "target" / "qos-apps-live" / "payloads" / APP / "bin"
               / "word-processor")
    missing = [what for what, path in (
        ("shell", shell), ("qos-launcher", tool("qos-launcher")),
        ("qos-install", tool("qos-install")), ("qos-catalogue", tool("qos-catalogue")),
        ("serve-test-registry", example), ("the word-processor payload", payload),
    ) if not path or not pathlib.Path(path).exists()]
    if missing:
        print("missing: " + ", ".join(missing))
        print("build:  cargo build -p shell -p install -p launcher -p word-processor && "
              "cargo build -p app-catalogue --bins --examples")
        return 2

    # Bob's copy has to be the same build alice installs, and that is not
    # a detail. An older payload announces its collaboration channel
    # under a different key, so the delta arrives at bob's broker and
    # matches nothing — which looks exactly like a replication failure
    # and is a stale binary. `target/qos-apps-live` is the store the
    # payload alice's registry serves comes from, so it is the one that
    # cannot drift from it.
    live_apps = REPO / "target" / "qos-apps-live"
    bob_payload = live_apps / "payloads" / APP / "bin" / "word-processor"
    if not bob_payload.exists():
        print(f"bob needs a current {APP} in {live_apps}; pack and install one there")
        return 2
    built = REPO / "target" / "debug" / "word-processor"
    if built.exists() and bob_payload.stat().st_mtime < built.stat().st_mtime:
        print(f"{bob_payload} is older than {built} — re-copy the payload before "
              f"trusting this run; a stale app fails to converge for reasons that "
              f"have nothing to do with collaboration")
        return 2

    root = tempfile.mkdtemp(prefix="/tmp/qs-")
    shots = os.path.join(root, "shots")
    os.makedirs(shots, exist_ok=True)
    print(f"session root: {root}")

    store_config = os.path.join(root, "store-config")
    sessions: list[Session] = []
    try:
        with Registry(example, payload, store_config,
                      os.path.join(root, "registry.log")) as reg:
            print(f"\n[1] a signed registry on {reg.base_url}, serving the real package")

            print("\n[2] two real sessions — alice has no apps at all")
            alice_apps = os.path.join(root, "alice-apps")
            os.makedirs(alice_apps, exist_ok=True)
            alice = Session("alice", root, alice_apps, store_config)
            sessions.append(alice)
            # A copy, so a run cannot write into the store the payload
            # is served from. Reflinked where the filesystem allows it,
            # which is why copying 400-odd MB here costs nothing.
            bob_apps = os.path.join(root, "bob-apps")
            subprocess.run(["cp", "-a", "--reflink=auto", str(live_apps), bob_apps],
                           check=True)
            bob = Session("bob", root, bob_apps)
            sessions.append(bob)
            for s in sessions:
                if not check(f"{s.name} came up", s.wait_up(), f"see {s.log_path}"):
                    return 1
            check("alice does not have the word processor",
                  not os.path.exists(os.path.join(alice_apps, "payloads", APP)),
                  alice_apps)

            print("\n[3] one room, formed the way a person forms one")
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

            print("\n[4] bob opens his own copy from the palette")
            b_win = open_from_palette(bob)
            if not check("bob's word processor painted", b_win is not None,
                         json.dumps(b_win) if b_win else f"see {bob.log_path}"):
                return 1

            # Bob first, alice second, and that order matters: sharing a
            # workspace places the peer's window as a tile on this side
            # too, and the newest window is the focused one. Installing
            # last leaves alice's own document holding the keyboard.
            print("\n[5] alice installs the word processor and it opens")
            a_win = install_and_open(alice)
            if not check("alice's installed word processor painted", a_win is not None,
                         json.dumps(a_win) if a_win else f"see {alice.log_path}"):
                return 1

            # Both apps have to have attached to their broker and
            # announced an address before either types, or the first
            # keystrokes go out to nobody.
            time.sleep(8.0)
            for s in sessions:
                note(f"{s.name}: {s.collab_state()}")
            check("alice's installed app connected to collaboration",
                  "collab connected=true" in alice.collab_state(),
                  alice.collab_state())

            print(f"\n[6] alice types {args.text!r}")
            # Deliberately no click first. The word processor opens with
            # the caret already in the document body, and a click aimed
            # at "the middle of the window" lands on the style panel and
            # moves focus off the page — after which every keystroke is
            # accepted by the chrome and the document stays empty. That
            # is what an earlier harness was doing while reporting that
            # the keystrokes had been accepted.
            alice.call({"op": "type", "text": args.text})

            print("\n[7] what each screen shows")
            time.sleep(14.0)
            a_png = os.path.join(shots, "alice-installed-and-typed.png")
            b_png = os.path.join(shots, "bob-received.png")
            check("alice's screen was captured", alice.snapshot(a_png), a_png)
            check("bob's screen was captured", bob.snapshot(b_png), b_png)

            print()
            print("  alice (installed, typed):  " + a_png)
            print("  bob   (remote):            " + b_png)
            print()
            print(f"  Both pages should read {args.text!r}. Alice's app was")
            print("  fetched, verified and installed during this run; bob's was")
            print("  already on his machine. A caret over a blank page on bob's")
            print("  side is the failure this proves against.")
            print()

            if failures:
                print(f"\nFAILED ({len(failures)}): " + "; ".join(failures))
                return 1
            print("\nlooked-at-by-a-human is still required: open both PNGs.")
            return 0
    finally:
        if args.keep:
            print(f"\nsessions left running under {root}")
        else:
            for s in sessions:
                s.stop()
            print(f"\nsessions stopped; files kept at {root}")


if __name__ == "__main__":
    sys.exit(main())
