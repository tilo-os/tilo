#!/usr/bin/env python3
"""One desktop, one story: open, find, place, share, edit, undo.

The product's capabilities are each proved somewhere. Nothing walks the
whole way through in one run, and the whole way through is the thing that
says "this is a desktop" rather than "these are parts".

So this drives two real compositors — separate state directories,
separate identities, separate control sockets, no display — through the
sequence a pair of people actually perform:

  1. open an app from the palette, and see it paint,
  2. activate a document, and see it come up in the app that owns its
     file type,
  3. find it again: `Ctrl+K` inside the focused app, `Super+K` across
     the machine — two chords that mean deliberately different things,
  4. check where the windows went, against the rule the shell publishes
     rather than one restated here,
  5. invite a peer, who joins and lands in the same room,
  6. grant a capability the peer receives, watch the peer be refused
     when it tries to administer, and revoke it again,
  7. both edit, and read the history back with both contributors on it,
  8. roll the document back to an earlier version.

Every claim prints expected and observed. Claims are read back from the
side that did *not* make the change wherever the change is supposed to
have crossed a process boundary.

    scripts/desktop-story-demo.py
    scripts/desktop-story-demo.py --out target/desktop-story --keep

Requires:  cargo build -p shell --bin shell -p word-processor -p launcher
No display server.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import signal
import socket
import stat
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# The document the story is about, and the package that owns its type.
# `.md` is in `word-processor`'s `[providers].ext`, so the routing has a
# real manifest to resolve against rather than a table in the shell.
DOC_NAME = "quarterly-report.md"
DOC_STEM = "quarterly-report"
WP_ID = "dev.qos.word-processor"
WP_NAME = "Word Processor"
# Installed alongside, claiming no extensions at all. Without a second
# package "the app that owns .md" and "the only app installed" are the
# same observation, and the first is the one being claimed.
DECOY_ID = "dev.qos.calculator"
DECOY_NAME = "Calculator"

# A word that appears in the document body but in no filename, so a hit
# on it is a hit on content.
BODY_NEEDLE = "photovoltaic"

# How long to wait for a change to cross between sessions. Replication
# is pulled on a fixed cadence; this is that cadence plus a slow frame.
SYNC_TIMEOUT_S = 25.0
# How long to wait for a launched app to map a toplevel and paint.
APP_TIMEOUT_S = 40.0


# --------------------------------------------------------------------
# control socket
# --------------------------------------------------------------------


def call(sock_path: str, payload: dict, timeout: float = 30.0) -> dict:
    """Send one command, read one reply."""
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(sock_path)
    try:
        s.sendall(json.dumps(payload).encode() + b"\n")
        buf = b""
        while b"\n" not in buf:
            chunk = s.recv(1 << 20)
            if not chunk:
                raise RuntimeError("compositor closed the connection mid-reply")
            buf += chunk
        return json.loads(buf.split(b"\n", 1)[0])
    finally:
        s.close()


def ok(resp: dict) -> dict:
    if "err" in resp:
        raise RuntimeError(f"compositor refused: {resp['err']}")
    return resp


# --------------------------------------------------------------------
# reporting
# --------------------------------------------------------------------

FAILURES: list[str] = []
CLAIMS = 0


def step(n: str, text: str) -> None:
    print(f"\n=== {n}  {text}", flush=True)


def claim(description: str, actual, expected) -> bool:
    global CLAIMS
    CLAIMS += 1
    good = actual == expected
    mark = "PASS" if good else "FAIL"
    print(f"  [{mark}] {description}\n         expected {expected!r}\n"
          f"         observed {actual!r}", flush=True)
    if not good:
        FAILURES.append(description)
    return good


def note(text: str) -> None:
    print(f"  ...  {text}", flush=True)


def wait_for(fn, timeout: float = SYNC_TIMEOUT_S, interval: float = 0.5):
    """Poll `fn` until it returns a truthy value, or give up."""
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = fn()
        if last:
            return last
        time.sleep(interval)
    return last


# --------------------------------------------------------------------
# the installed-app fixture
# --------------------------------------------------------------------


def install_package(apps_dir: str, package_id: str, name: str, exts: list[str],
                    binary: str | None) -> None:
    """Write a registry entry + manifest the real `installed_apps()` reads.

    `binary`, when given, is hard-linked (falling back to a copy) into the
    payload as the entrypoint — so the launch starts the actual program,
    not a stand-in that reports success.
    """
    payload = os.path.join(apps_dir, "payloads", package_id)
    os.makedirs(os.path.join(payload, "bin"), exist_ok=True)
    entry_bin = "bin/app"
    if binary:
        dst = os.path.join(payload, "bin", "app")
        if os.path.exists(dst):
            os.unlink(dst)
        try:
            os.link(binary, dst)
        except OSError:
            shutil.copy2(binary, dst)
        os.chmod(dst, os.stat(dst).st_mode | stat.S_IXUSR | stat.S_IXGRP)
    ext_list = ", ".join(f'"{e}"' for e in exts)
    manifest_path = os.path.join(payload, "manifest.toml")
    with open(manifest_path, "w") as f:
        f.write(
            f'[package]\nid = "{package_id}"\nname = "{name}"\n'
            f'version = "1.0.0"\n'
            f'[entrypoint]\nbin = "{entry_bin}"\ntype = "gui"\n'
            f'[providers]\nmime = []\next = [{ext_list}]\n'
        )
    registry = os.path.join(apps_dir, "registry.toml")
    with open(registry, "a") as f:
        f.write(f"[[entry]]\nid = '{package_id}'\n"
                f'payload_root = "{payload}"\nmanifest_path = "{manifest_path}"\n')


DOC_BODY = f"""# Quarterly Report

Revenue held flat against a hard comparison quarter.

The {BODY_NEEDLE} line carried the margin: unit costs fell faster
than prices, and the backlog converted at a rate we have not seen
since the first year.

Headcount is unchanged.
"""


def write_home(home: str) -> str:
    """The user's files. Returns the document's path."""
    docs = os.path.join(home, "Documents")
    os.makedirs(docs, exist_ok=True)
    path = os.path.join(docs, DOC_NAME)
    with open(path, "w") as f:
        f.write(DOC_BODY)
    return path


# --------------------------------------------------------------------


class Session:
    """One running compositor, addressed by its control socket."""

    def __init__(self, name: str, user: str, run_dir: str, sock: str,
                 binary: str, size: str, app_binary: str, launcher_bin: str):
        self.name = name
        self.user = user
        self.dir = run_dir
        self.sock = sock
        self.binary = binary
        self.size = size
        self.app_binary = app_binary
        self.launcher_bin = launcher_bin
        self.proc: subprocess.Popen | None = None
        self.log_path = os.path.join(run_dir, "compositor.log")
        self.home = os.path.join(run_dir, "home")
        self.apps = os.path.join(run_dir, "apps")
        self.doc_path = ""
        self._log = None
        self.runtime = ""

    # -- lifecycle ---------------------------------------------------

    def prepare(self, runtime: str) -> None:
        self.runtime = runtime
        os.makedirs(os.path.join(self.dir, "config"), exist_ok=True)
        os.makedirs(os.path.join(self.dir, "keys"), exist_ok=True)
        os.makedirs(self.apps, exist_ok=True)
        os.makedirs(runtime, exist_ok=True)
        self.doc_path = write_home(self.home)
        install_package(self.apps, WP_ID, WP_NAME,
                        ["odt", "docx", "md", "markdown", "rtf"],
                        self.app_binary)
        install_package(self.apps, DECOY_ID, DECOY_NAME, [], None)

    def start(self) -> None:
        env = dict(os.environ)
        # No display, and nothing inherited that would make the two
        # sessions share state — including `HOME`, which is what the
        # file index walks.
        env.pop("DISPLAY", None)
        env.pop("WAYLAND_DISPLAY", None)
        env.update(
            HOME=self.home,
            XDG_CONFIG_HOME=os.path.join(self.dir, "config"),
            XDG_DATA_HOME=os.path.join(self.dir, "data"),
            XDG_CACHE_HOME=os.path.join(self.dir, "cache"),
            XDG_RUNTIME_DIR=self.runtime,
            QOS_USER_ID=self.user,
            QOS_PEER_KEY_PATH=os.path.join(self.dir, "keys", "peer-key"),
            QOS_FS_ACL_PATH=os.path.join(self.dir, "config", "fs-capabilities.json"),
            QOS_APPS_DATA_DIR=self.apps,
            QOS_LAUNCHER_BIN=self.launcher_bin,
            # No session services are running under a scratch runtime
            # dir; a package that declares them optional would warn per
            # launch otherwise.
            QOS_LAUNCH_NO_SERVICES="1",
            QOS_TEST_CONTROL=self.sock,
            NIRIOS_HEADLESS_SIZE=self.size,
            RUST_LOG=env.get("RUST_LOG", "info"),
        )
        self._log = open(self.log_path, "w")
        self.proc = subprocess.Popen(
            [self.binary, "--backend", "headless", "--enable-peer-sync"],
            cwd=REPO, env=env, stdout=self._log, stderr=subprocess.STDOUT,
        )

    def wait_ready(self, timeout: float = 120.0) -> None:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.proc and self.proc.poll() is not None:
                raise RuntimeError(
                    f"{self.name} exited before binding its control socket; "
                    f"see {self.log_path}")
            if os.path.exists(self.sock):
                try:
                    self.state()
                    return
                except (OSError, RuntimeError):
                    pass
            time.sleep(0.25)
        raise RuntimeError(f"{self.name} never answered on {self.sock}")

    def stop(self) -> None:
        if self.proc and self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        if self._log:
            self._log.close()

    # -- reading -----------------------------------------------------

    def state(self) -> dict:
        return ok(call(self.sock, {"op": "state_dump"}))["state"]

    def chrome(self) -> dict:
        return self.state()["chrome"]

    def tree(self) -> dict:
        return self.state()["workspace_tree"]

    def active_workspace(self) -> dict:
        tree = self.tree()
        active = tree["active"]
        for ws in tree["workspaces"]:
            if ws["id"] == active:
                return ws
        raise RuntimeError(f"{self.name}: no active workspace in the tree")

    def active_strip(self) -> dict:
        ws = self.active_workspace()
        tree = self.tree()
        strip_id = tree.get("active_strip", {}).get(str(ws["id"]))
        for s in ws["strips"]:
            if s["id"] == strip_id:
                return s
        return ws["strips"][0] if ws["strips"] else {"id": None, "window_groups": []}

    def groups(self) -> list[dict]:
        return self.active_strip().get("window_groups") or []

    def active_group_id(self):
        strip = self.active_strip()
        return self.tree().get("active_group", {}).get(str(strip["id"]))

    def foreign_frames(self) -> list[dict]:
        return ok(call(self.sock, {"op": "foreign_frames"}))["frames"]

    def targets(self) -> list[dict]:
        return ok(call(self.sock, {"op": "list_targets"}))["targets"]

    def foreign_tabs(self) -> list[dict]:
        """Every adopted Wayland toplevel in the active workspace, in
        strip order, with the group it landed in."""
        out = []
        for gi, g in enumerate(self.groups()):
            for ti, t in enumerate(g["tabs"]):
                tile = t.get("tile") or {}
                if "Foreign" in tile:
                    out.append({
                        "group_index": gi, "group_id": g["id"],
                        "tab_index": ti, "tab_id": t["id"],
                        "title": t["title"],
                        "app_id": tile["Foreign"]["app_id"],
                        "surface_id": tile["Foreign"]["surface_id"],
                    })
        return out

    # -- driving -----------------------------------------------------

    def key(self, name: str) -> None:
        ok(call(self.sock, {"op": "key", "key": name}))

    def type_text(self, text: str) -> None:
        for i in range(0, len(text), 200):
            ok(call(self.sock, {"op": "type", "text": text[i:i + 200]}))

    def modifiers(self, ctrl=False, shift=False, alt=False, logo=False) -> None:
        ok(call(self.sock, {"op": "modifiers", "ctrl": ctrl, "shift": shift,
                            "alt": alt, "logo": logo}))

    def snapshot(self, path: str) -> str:
        os.makedirs(os.path.dirname(path), exist_ok=True)
        ok(call(self.sock, {"op": "snapshot", "path": path}))
        return path

    def click(self, name: str) -> None:
        ok(call(self.sock, {"op": "click_named", "name": name}))
        self.settle(0.4)

    def click_at(self, x: float, y: float) -> None:
        ok(call(self.sock, {"op": "click_at", "x": x, "y": y}))
        self.settle(0.4)

    def settle(self, seconds: float = 0.6) -> None:
        """Let the compositor paint. Chrome rects are published by the
        draw pass, so a click by name issued in the same breath as the
        state change that reveals it would miss."""
        time.sleep(seconds)

    # -- the palette -------------------------------------------------

    def palette(self) -> dict:
        return self.chrome()["palette"]

    def open_palette(self, machine_wide: bool = True) -> None:
        """`Super+K` searches the machine; `Ctrl+K` searches the app in
        front of you. The chord is the question."""
        self.modifiers(logo=machine_wide, ctrl=not machine_wide)
        self.key("k")
        self.modifiers()
        self.settle(0.4)

    def close_palette(self) -> None:
        for _ in range(3):
            if not self.palette()["visible"]:
                return
            self.key("Escape")
            self.settle(0.3)

    def palette_rows(self) -> list[dict]:
        return self.palette()["rows"]

    def selected_row(self) -> dict | None:
        p = self.palette()
        for r in p["rows"]:
            if r["id"] == p["selected_id"]:
                return r
        return None

    def query_palette(self, query: str, machine_wide: bool = True,
                      until=None, timeout: float = 20.0) -> list[dict]:
        """Open the palette and type `query`, waiting for the background
        index if `until` says the row wanted is not there yet.

        The wait re-types the last character rather than reaching into
        the index, so what settles is the production per-keystroke
        refresh.
        """
        self.open_palette(machine_wide)
        self.type_text(query)
        self.settle(0.7)
        if until is None:
            return self.palette_rows()
        deadline = time.time() + timeout
        while time.time() < deadline:
            rows = self.palette_rows()
            if until(rows):
                return rows
            self.type_text(query[-1])
            self.key("Backspace")
            self.settle(0.5)
        return self.palette_rows()

    def activate_selected(self) -> None:
        self.key("Enter")
        self.settle(0.8)

    def run_palette_command(self, query: str, expect_id: str | None = None,
                            machine_wide: bool = True) -> str | None:
        self.open_palette(machine_wide)
        self.type_text(query)
        self.settle(0.7)
        selected = self.palette()["selected_id"]
        if expect_id is not None and selected != expect_id:
            rows = [r["id"] for r in self.palette()["rows"]][:6]
            raise RuntimeError(
                f"{self.name}: palette query {query!r} selected {selected!r}, "
                f"expected {expect_id!r} (rows: {rows})")
        self.key("Enter")
        self.settle()
        return selected

    # -- capabilities -------------------------------------------------

    def grant(self, subject: str, scope: str, verb: str,
              cap_id: str | None = None) -> dict:
        payload = {"op": "grant", "subject": subject, "scope": scope, "verb": verb}
        if cap_id:
            payload["id"] = cap_id
        return call(self.sock, payload)

    def revoke(self, cap_id: str) -> dict:
        return call(self.sock, {"op": "revoke", "id": cap_id})

    def check_action(self, actor: str, app: str, action: str,
                     verb: str = "write") -> dict:
        return call(self.sock, {"op": "check_action", "actor": actor, "app": app,
                                "action": action, "verb": verb})

    def capability_ids(self) -> set[str]:
        return {c["id"] for c in (self.active_workspace().get("capabilities") or [])}


def peer_id_of(s: Session) -> str:
    """The libp2p peer id the session logged at startup."""
    try:
        with open(s.log_path) as f:
            for line in f:
                plain = re.sub(r"\x1b\[[0-9;]*m", "", line)
                if "local_peer_id=" in plain:
                    return plain.split("local_peer_id=")[1].split()[0].strip()
    except OSError:
        pass
    return "<unknown>"


def wayland_socket_of(s: Session) -> str:
    try:
        with open(s.log_path) as f:
            for line in f:
                plain = re.sub(r"\x1b\[[0-9;]*m", "", line)
                if "compositor listening on" in plain:
                    return plain.split("listening on", 1)[1].strip().strip('"')
    except OSError:
        pass
    return "<unknown>"


# --------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", default=os.path.join(REPO, "target", "desktop-story"),
                    help="run directory for state, logs and screenshots")
    ap.add_argument("--shell", default=os.path.join(REPO, "target", "debug", "shell"))
    ap.add_argument("--app", default=os.path.join(REPO, "target", "debug", "word-processor"))
    ap.add_argument("--launcher",
                    default=os.path.join(REPO, "target", "debug", "qos-launcher"))
    ap.add_argument("--size", default="1280x800", help="headless viewport")
    ap.add_argument("--keep", action="store_true",
                    help="keep the previous run directory instead of wiping it")
    args = ap.parse_args()

    missing = [p for p in (args.shell, args.app, args.launcher) if not os.path.exists(p)]
    if missing:
        print("missing binaries:\n  " + "\n  ".join(missing) +
              "\nbuild them:  cargo build -p shell --bin shell -p word-processor -p launcher",
              file=sys.stderr)
        return 2

    if not args.keep:
        shutil.rmtree(args.out, ignore_errors=True)
    shots = os.path.join(args.out, "shots")
    os.makedirs(shots, exist_ok=True)

    # Unix socket paths are capped near 108 bytes, so the sockets and the
    # per-session runtime dirs live under the runtime root.
    runtime_root = os.environ.get("XDG_RUNTIME_DIR") or "/tmp"
    tag = os.getpid()

    sessions = []
    for name, user in (("host", "alice"), ("peer", "bob")):
        sessions.append(Session(
            name, user, os.path.join(args.out, name),
            os.path.join(runtime_root, f"qos-story-{name}-{tag}.sock"),
            args.shell, args.size, args.app, args.launcher))
    host, peer = sessions

    print(f"run directory: {args.out}")
    print(f"screenshots:   {shots}")
    rc = 0
    try:
        for s in sessions:
            s.prepare(os.path.join(runtime_root, f"qos-story-{s.name}-{tag}"))
            s.start()
        for s in sessions:
            s.wait_ready()
            print(f"{s.name} ready (pid {s.proc.pid}, user {s.user}, "
                  f"wayland {wayland_socket_of(s)})")
        run(host, peer, shots)
    except Exception as exc:  # noqa: BLE001 — the report is the product
        FAILURES.append(f"{type(exc).__name__}: {exc}")
        import traceback
        traceback.print_exc()
    finally:
        for s in sessions:
            s.stop()
            try:
                os.unlink(s.sock)
            except FileNotFoundError:
                pass

    print("\n" + "=" * 68)
    print(f"{CLAIMS} claim(s) checked.")
    if FAILURES:
        rc = 1
        print(f"{len(FAILURES)} did not hold:")
        for f in FAILURES:
            print(f"  - {f}")
    else:
        print("every claim held.")
    print(f"screenshots: {shots}")
    for s in sessions:
        print(f"log:         {s.log_path}")
    return rc


def run(host: Session, peer: Session, shots: str) -> None:
    """Filled in step by step below."""
    raise NotImplementedError


if __name__ == "__main__":
    sys.exit(main())
