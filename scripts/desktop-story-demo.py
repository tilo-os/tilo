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

# A word in the Notes tile's own seeded body — in no filename, and in
# no other document. What the shell-hosted app's own search must find.
NOTES_NEEDLE = "replicate"

# The runtime bundle version `apps/word-processor/qos-app.toml` pins.
RUNTIME_VERSION = "2026.07"

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


def register(apps_dir: str, package_id: str, payload: str, manifest_path: str) -> None:
    """Add an entry to the registry `launcher::installed_apps()` reads."""
    with open(os.path.join(apps_dir, "registry.toml"), "a") as f:
        f.write(f"[[entry]]\nid = '{package_id}'\n"
                f'payload_root = "{payload}"\nmanifest_path = "{manifest_path}"\n')


def install_real_app(apps_dir: str, manifest_src: str, package_id: str,
                     entry_rel: str, binary: str) -> None:
    """Install a package built from this workspace, as shipped.

    The manifest is the app's own `qos-app.toml` — its real sandbox
    declarations, its real `[providers].ext` list — and the entrypoint is
    the real binary. Nothing here is a stand-in: `qos-launcher` reads
    this manifest, builds the sandbox it describes and execs this
    program.
    """
    payload = os.path.join(apps_dir, "payloads", package_id)
    dst = os.path.join(payload, entry_rel)
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    if os.path.exists(dst):
        os.unlink(dst)
    try:
        os.link(binary, dst)
    except OSError:
        shutil.copy2(binary, dst)
    os.chmod(dst, os.stat(dst).st_mode | stat.S_IXUSR | stat.S_IXGRP)
    manifest_path = os.path.join(payload, "manifest.toml")
    shutil.copyfile(manifest_src, manifest_path)
    register(apps_dir, package_id, payload, manifest_path)


def install_decoy(apps_dir: str, package_id: str, name: str) -> None:
    """A second installed package that claims no file types.

    It is never launched. It exists so that "the app that owns `.md`" is
    a different observation from "the only app installed" — without it,
    a routing that ignored the extension entirely would look identical
    to one that resolved it.
    """
    payload = os.path.join(apps_dir, "payloads", package_id)
    os.makedirs(os.path.join(payload, "bin"), exist_ok=True)
    manifest_path = os.path.join(payload, "manifest.toml")
    with open(manifest_path, "w") as f:
        f.write(f'[package]\nid = "{package_id}"\nname = "{name}"\n'
                f'version = "1.0.0"\n'
                f'[entrypoint]\nbin = "bin/app"\ntype = "gui"\n'
                f'[providers]\nmime = []\next = []\n')
    register(apps_dir, package_id, payload, manifest_path)


def bake_runtime(work: str, tools: dict) -> str:
    """Build and sign one runtime bundle, and return its directory.

    The launcher refuses an unsigned bundle, and it is right to: the
    bundle is the shared library set every sandboxed app runs against.
    So the fixture signs one with a freshly generated key, exactly as
    `os/scripts/bake-runtime-bundle.sh` does for an image — no launcher
    flag is relaxed anywhere in this run.
    """
    key = os.path.join(work, "runtime-key.json")
    data_home = os.path.join(work, "runtime-data")
    installed = os.path.join(data_home, "qos-runtime", RUNTIME_VERSION)
    if os.path.isdir(installed):
        return installed
    os.makedirs(work, exist_ok=True)
    subprocess.run([tools["bundle"], "key", "generate", "--out", key],
                   check=True, capture_output=True)
    env = dict(os.environ, XDG_DATA_HOME=data_home)
    subprocess.run(
        [tools["install"], "runtime", "install", "--version", RUNTIME_VERSION,
         "--from-host", "--identity", key, "--force"],
        check=True, capture_output=True, env=env)
    return installed


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

    def prepare(self, runtime: str, runtime_bundle: str) -> None:
        self.runtime = runtime
        self.data_home = os.path.join(self.dir, "data")
        os.makedirs(os.path.join(self.dir, "config"), exist_ok=True)
        os.makedirs(os.path.join(self.dir, "keys"), exist_ok=True)
        os.makedirs(self.apps, exist_ok=True)
        os.makedirs(runtime, exist_ok=True)
        self.doc_path = write_home(self.home)
        install_real_app(self.apps, os.path.join(REPO, "apps", "word-processor",
                                                 "qos-app.toml"),
                         WP_ID, "bin/word-processor", self.app_binary)
        install_decoy(self.apps, DECOY_ID, DECOY_NAME)
        # Each session gets its own copy of the signed runtime bundle,
        # in the location the launcher looks in for this user.
        dst = os.path.join(self.data_home, "qos-runtime", RUNTIME_VERSION)
        if not os.path.isdir(dst):
            shutil.copytree(runtime_bundle, dst)

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

    def active_tab_id(self):
        gid = self.active_group_id()
        for g in self.groups():
            if g["id"] == gid:
                return g["active_tab"]
        return None

    def foreign_frames(self) -> list[dict]:
        return ok(call(self.sock, {"op": "foreign_frames"}))["foreign_frames"]["frames"]

    def frame_seq(self, surface_id: int) -> int:
        for f in self.foreign_frames():
            if f["surface_id"] == surface_id:
                return f["frame_seq"]
        return -1

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

    def focus_via_search(self, query: str, group: str) -> bool:
        """Focus an app's tab by activating one of its own search hits.

        Opening a result in a shell-hosted app focuses the tab holding it,
        which is the only route to "look at that app" a script has: tab
        pills are drawn by the group row and carry no addressable name.
        Returns whether a row of the wanted section was the one activated.
        """
        self.open_palette(machine_wide=True)
        self.type_text(query)
        self.settle(1.0)
        row = self.selected_row() or {}
        hit = row.get("group") == group
        if hit:
            self.activate_selected()
        else:
            note(f"{self.name}: {query!r} selected {row.get('group')!r} "
                 f"{row.get('label')!r}, not a {group} row")
            self.close_palette()
        return hit

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

    def open_permissions_tab(self) -> bool:
        """Workspace settings, on its Permissions tab. `False` means the
        modal itself never opened."""
        if not self.chrome()["overlays"]["ws_settings"]:
            self.run_palette_command("workspace settings", expect_id="result:0")
        if not self.chrome()["overlays"]["ws_settings"]:
            return False
        self.settle()
        names = {t["name"] for t in self.targets()}
        if "ws_settings_tab[permissions]" not in names:
            return False
        self.click("ws_settings_tab[permissions]")
        return self.chrome()["ws_settings_tab"] == "Permissions"

    def close_ws_settings(self) -> None:
        for _ in range(4):
            if not self.chrome()["overlays"]["ws_settings"]:
                return
            self.key("Escape")
            self.settle(0.3)

    def revoke_rects(self) -> list[str]:
        return sorted(t["name"] for t in self.targets()
                      if t["name"].startswith("permissions_revoke["))


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
    ap.add_argument("--installer",
                    default=os.path.join(REPO, "target", "debug", "qos-install"))
    ap.add_argument("--bundler",
                    default=os.path.join(REPO, "target", "debug", "qos-runtime-bundle"))
    ap.add_argument("--size", default="1280x800", help="headless viewport")
    ap.add_argument("--keep", action="store_true",
                    help="keep the previous run directory instead of wiping it")
    args = ap.parse_args()

    tools = {"install": args.installer, "bundle": args.bundler}
    missing = [p for p in (args.shell, args.app, args.launcher, args.installer,
                           args.bundler) if not os.path.exists(p)]
    if missing:
        print("missing binaries:\n  " + "\n  ".join(missing) +
              "\nbuild them:  cargo build -p shell --bin shell -p word-processor "
              "-p launcher -p install -p runtime-bundle", file=sys.stderr)
        return 2

    if not args.keep:
        shutil.rmtree(args.out, ignore_errors=True)
    shots = os.path.join(args.out, "shots")
    os.makedirs(shots, exist_ok=True)
    # Baked once and shared between the sessions by copy; keeping it
    # outside the wiped tree makes a re-run cheap.
    bundle = bake_runtime(os.path.join(REPO, "target", "desktop-story-runtime"), tools)
    print(f"runtime bundle: {bundle}")

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
            s.prepare(os.path.join(runtime_root, f"qos-story-{s.name}-{tag}"), bundle)
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


def wait_for_toplevel(s: Session, app_id: str, since: set[int],
                      timeout: float = APP_TIMEOUT_S) -> dict | None:
    """A toplevel for `app_id` in a tab that was not already there.

    Keyed on the tab, not on the wayland surface: a `wl_surface`'s
    protocol id is per-client, so two clients both hold low ids and
    "a surface id I have not seen" is not the same question as "a window
    that was not there".
    """
    return wait_for(
        lambda: next((t for t in s.foreign_tabs()
                      if t["app_id"] == app_id and t["tab_id"] not in since),
                     None),
        timeout=timeout, interval=0.5)


def wait_for_paint(s: Session, surface_id: int, timeout: float = APP_TIMEOUT_S) -> int:
    """Frames the compositor has taken from that client.

    `frame_seq > 0` is the compositor's own bookkeeping, not a guess from
    pixels: a tile that still shows the first-frame placeholder is
    near-identical to a real first frame of an app that starts empty.
    """
    return wait_for(lambda: s.frame_seq(surface_id) or None, timeout=timeout) or 0


def run(host: Session, peer: Session, shots: str) -> None:
    step("1.", "open an app from the palette, and watch it paint")
    before = {t["tab_id"] for t in host.foreign_tabs()}
    rows = host.query_palette(
        WP_NAME, until=lambda r: any(WP_NAME in x["label"] for x in r))
    note(f"palette rows: {[r['label'] for r in rows][:5]}")
    selected = host.selected_row()
    claim("the palette's top row is the app",
          (selected or {}).get("label", "").endswith(WP_NAME), True)
    claim("the palette is searching the whole machine",
          host.chrome()["search_scope"], "All")
    host.snapshot(os.path.join(shots, "01-palette-app.png"))
    host.activate_selected()

    win = wait_for_toplevel(host, WP_ID, before)
    claim("the app mapped a window the compositor adopted", bool(win), True)
    if not win:
        note("no toplevel; the rest of the story has nothing to act on")
        note(f"see {host.log_path}")
        return
    note(f"window: {json.dumps(win)}")
    claim("the window is attributed to the package that was launched",
          win["app_id"], WP_ID)
    claim("its tab is labelled with the app, not a generic app-id",
          win["title"], WP_NAME)
    painted = wait_for_paint(host, win["surface_id"])
    note(f"frames taken from the client: {painted}")
    claim("the app painted its own pixels", painted > 0, True)
    host.snapshot(os.path.join(shots, "02-app-open.png"))

    step("2.", "activate a document, and it comes up in the app that owns it")
    groups_before = [g["id"] for g in host.groups()]
    selected_group_before = host.active_group_id()
    before = {t["tab_id"] for t in host.foreign_tabs()}
    rows = host.query_palette(
        DOC_STEM, until=lambda r: any(DOC_STEM in x["label"] for x in r))
    note(f"palette rows: {[(r['group'], r['label']) for r in rows][:5]}")
    hit = host.selected_row() or {}
    claim("the palette's top row is the document",
          DOC_STEM in hit.get("label", ""), True)
    claim("and it is offered as a file, not as an app",
          hit.get("group"), "Files")
    host.snapshot(os.path.join(shots, "03-palette-document.png"))
    host.activate_selected()

    doc_win = wait_for_toplevel(host, WP_ID, before)
    claim("activating it started a window", bool(doc_win), True)
    record = "file:" + host.doc_path
    opened = wait_for(
        lambda: next((d for d in host.chrome().get("open_documents") or []
                      if d["record"] == record), None), timeout=APP_TIMEOUT_S)
    note(f"open documents: {json.dumps(host.chrome().get('open_documents'))}")
    claim("the desktop records the document as open", bool(opened), True)
    claim("in the package that declares its file type, not the other one",
          (opened or {}).get("package_id"), WP_ID)
    if doc_win:
        claim("bound to the window that arrived",
              (opened or {}).get("surface_id"), doc_win["surface_id"])
        painted = wait_for_paint(host, doc_win["surface_id"])
        note(f"frames taken from the document's client: {painted}")
        claim("the document's window painted", painted > 0, True)

    # The app is handed a path, not access to it. Both decisions have to
    # be made, and this is the second one: without the grant the app
    # opens its argument and gets ENOENT inside its own sandbox.
    grants = [c for c in (host.active_workspace().get("capabilities") or [])
              if c.get("effect") == "Grant"
              and json.dumps(c.get("subject")).count(WP_ID)]
    note(f"grants addressed to the app: {json.dumps(grants)}")
    claim("the workspace grants the app read on exactly that file",
          [c["scope"] for c in grants], [{"FsPath": host.doc_path}])
    host.snapshot(os.path.join(shots, "04-document-open.png"))

    # Asking for the same document again is asking to look at it, not to
    # start a second copy of the app on it.
    windows_before = len([t for t in host.foreign_tabs() if t["app_id"] == WP_ID])
    host.query_palette(DOC_STEM, until=lambda r: any(DOC_STEM in x["label"] for x in r))
    host.activate_selected()
    host.settle(2.0)
    claim("re-activating the document focuses its window instead of opening a second",
          len([t for t in host.foreign_tabs() if t["app_id"] == WP_ID]), windows_before)
    if doc_win:
        claim("and the window it focused is the document's",
              host.active_tab_id(), doc_win["tab_id"])

    step("3.", "find it again — two chords that ask different questions")
    # `Ctrl+K` is "search what I am looking at". The focused window is
    # the document's, so a hit has to come from inside the document.
    host.open_palette(machine_wide=False)
    claim("Ctrl+K narrows the search to the focused app",
          host.chrome()["search_scope"], "ActiveApp")
    host.type_text(BODY_NEEDLE)
    host.settle(1.0)
    in_app = host.palette_rows()
    note(f"in-app rows: {[(r['group'], r['label'], r['description']) for r in in_app][:4]}")
    # A hit *inside* the document comes back with the text around the
    # match as its subtitle. A row whose subtitle is the file's path is
    # the machine-wide file search answering instead — same document,
    # different question, and not what this chord asked.
    claim("searching in the app finds the word inside the open document",
          any(BODY_NEEDLE.lower() in (r.get("description") or "").lower()
              for r in in_app), True)
    host.snapshot(os.path.join(shots, "06-ctrl-k-in-app.png"))
    host.close_palette()

    # The same chord over an app the shell hosts itself, to place the
    # boundary: if this passes and the one above fails, in-app search
    # works and what is missing is a provider for windows the compositor
    # only adopts.
    # Focus it the way a user would: search for something only that app
    # holds, and activate the row — a hit in a shell-hosted app focuses
    # its tab.
    focused = host.focus_via_search(NOTES_NEEDLE, "Notes")
    claim("a search hit in a shell-hosted app focuses that app's tab", focused, True)
    host.open_palette(machine_wide=False)
    claim("Ctrl+K over a shell-hosted app narrows to it as well",
          host.chrome()["search_scope"], "ActiveApp")
    host.type_text(NOTES_NEEDLE)
    host.settle(1.0)
    hosted = host.palette_rows()
    note(f"hosted-app rows: {[(r['group'], r['label'], r['description']) for r in hosted][:4]}")
    claim("and it finds the word inside that app's own document",
          any(NOTES_NEEDLE.lower() in (r.get("description") or "").lower()
              for r in hosted), True)
    host.close_palette()

    # The same word under `Super+K`, which searches the machine: the file
    # is found as a file, wherever it is, whether or not it is open.
    rows = host.query_palette(
        BODY_NEEDLE, until=lambda r: any(DOC_STEM in x["label"] for x in r))
    note(f"machine-wide rows: {[(r['group'], r['label']) for r in rows][:4]}")
    claim("Super+K searches everywhere", host.chrome()["search_scope"], "All")
    claim("and finds the document on the machine by a word inside it",
          any(DOC_STEM in r["label"] and r["group"] == "Files" for r in rows), True)
    host.snapshot(os.path.join(shots, "07-super-k-machine.png"))
    host.close_palette()

    # And the two really are different: the machine-wide search offers
    # things the in-app one does not.
    machine_labels = {r["label"] for r in rows}
    claim("the two scopes are not the same list",
          machine_labels != {r["label"] for r in in_app}, True)

    step("4.", "where the windows went")
    # The rule is the shell's, not this script's: a new window goes
    # immediately right of the selection, and joins it as a tab when the
    # selection is one tab of several. Both cases are exercised, and both
    # are read back off the tree.
    note(f"strip before the app opened: groups {groups_before}, "
         f"selected {selected_group_before}")
    if win:
        sel_group = next((g for g in host.groups() if g["id"] == win["group_id"]), None)
        claim("the app joined the group that was selected, which was tabbed",
              win["group_id"], selected_group_before)
        claim("as a tab immediately right of the one that was selected",
              win["tab_index"], 1)
        claim("and it did not split the strip into another column",
              [g["id"] for g in host.groups()], groups_before)
        note(f"that group's tabs now: {[t['title'] for t in (sel_group or {}).get('tabs', [])]}")
    host.snapshot(os.path.join(shots, "05-placement-tabbed.png"))

    step("5.", "invite a peer, who joins and lands in the same room")
    host_ws = host.active_workspace()
    note(f"host room: {host_ws['room']} ({host_ws['name']})")
    claim("the host owns the room it is sitting in", host_ws["owner"], host.user)
    claim("and may administer it", host.chrome()["may_administer_active"], True)
    peer_before = peer.active_workspace()
    claim("the peer starts somewhere else",
          peer_before["room"] != host_ws["room"], True)

    host.run_palette_command("invite peer", expect_id="result:0")
    token = wait_for(lambda: host.chrome()["peer_sync"]["invite_token"], timeout=20)
    claim("the palette minted an invite", bool(token), True)
    if not token:
        note("no invite token; steps 5 to 8 have nothing to act on")
        return
    note(f"token is {len(token)} chars; host status "
         f"{host.chrome()['peer_sync']['status']!r}")
    host.snapshot(os.path.join(shots, "08-invite-token.png"))
    host.key("Escape")
    host.settle()

    peer.run_palette_command("connect to peer", expect_id="result:0")
    opened = wait_for(
        lambda: peer.chrome()["peer_sync"]["connect_input"] is not None, timeout=15)
    claim("the peer's connect prompt opened", bool(opened), True)
    peer.type_text(token)
    claim("the token arrived intact",
          peer.chrome()["peer_sync"]["connect_input"], token)
    peer.snapshot(os.path.join(shots, "09-peer-connect.png"))
    peer.key("Enter")
    joined = wait_for(
        lambda: (peer.chrome()["peer_sync"]["status"] or "").startswith("Joined"),
        timeout=20)
    note(f"peer status: {peer.chrome()['peer_sync']['status']!r}")
    claim("the peer joined", bool(joined), True)
    peer.key("Escape")
    peer.settle()
    peer_ws = peer.active_workspace()
    claim("the peer is in the host's room", peer_ws["room"], host_ws["room"])
    claim("and its copy names the host as the room's owner",
          peer_ws["owner"], host.user)
    claim("a guest may not administer the room it joined",
          peer.chrome()["may_administer_active"], False)
    claim("the peer records the inviter as the room's governance authority",
          peer.tree().get("room_authority", {}).get(peer_ws["room"]),
          {"Peer": f"p2p:{peer_id_of(host)}"})
    peer.snapshot(os.path.join(shots, "10-peer-joined.png"))

    step("6.", "the host grants, the peer receives; the peer may not administer")
    before_grant = peer.check_action(f"app:{WP_ID}", WP_ID, "edit")
    claim("the peer's own gate denies the app before any grant",
          before_grant.get("answer", {}).get("permitted"), False)

    cap_id = "story-edit"
    fs_cap_id = "story-path"
    granted = host.grant(f"app:{WP_ID}", "workspace", "write", cap_id)
    claim("the workspace-scoped grant was recorded",
          granted.get("answer", {}).get("granted"), cap_id)
    granted_fs = host.grant(f"app:{WP_ID}", f"fs:{host.doc_path}", "write", fs_cap_id)
    claim("the path-scoped grant was recorded",
          granted_fs.get("answer", {}).get("granted"), fs_cap_id)

    arrived = wait_for(lambda: peer.capability_ids() >= {cap_id, fs_cap_id})
    claim("both grants reached the peer's own copy", bool(arrived), True)
    after_grant = peer.check_action(f"app:{WP_ID}", WP_ID, "edit")
    claim("and the peer's own gate now permits the app",
          after_grant.get("answer", {}).get("permitted"), True)
    claim("the peer's permissions tab is reachable", peer.open_permissions_tab(), True)
    rows = peer.revoke_rects()
    note(f"peer permissions rows: {rows}")
    claim("the peer's screen lists the capability that arrived over the wire",
          f"permissions_revoke[{fs_cap_id}]" in rows, True)
    peer.snapshot(os.path.join(shots, "11-peer-permissions.png"))

    peer_grant = peer.grant("app:org.qos.Rogue", "workspace", "admin")
    claim("a guest's grant is refused", "err" in peer_grant, True)
    claim("for the right reason",
          peer_grant.get("err", {}).get("code"), "grant_refused")
    peer_revoke = peer.revoke(cap_id)
    claim("a guest's revoke is refused", "err" in peer_revoke, True)
    claim("and the capability it tried to revoke is still there",
          cap_id in peer.capability_ids(), True)
    claim("nothing the guest asked for reached the host",
          any("Rogue" in json.dumps(c.get("subject"))
              for c in host.active_workspace().get("capabilities") or []), False)
    peer.close_ws_settings()

    revoked = host.revoke(cap_id)
    claim("the host's revoke succeeds", revoked.get("answer", {}).get("revoked"), True)
    gone = wait_for(lambda: cap_id not in peer.capability_ids())
    claim("and the peer sees it gone", bool(gone), True)
    after_revoke = peer.check_action(f"app:{WP_ID}", WP_ID, "edit")
    claim("the peer's gate denies the app again",
          after_revoke.get("answer", {}).get("permitted"), False)
    host.snapshot(os.path.join(shots, "12-after-revoke.png"))

    step("7.", "both edit, and the history carries both of them")
    host_mark = "Quarterly review"
    rename_active_workspace(host, host_mark)
    claim("the host's edit landed on the host",
          host.active_workspace()["name"], host_mark)
    crossed = wait_for(lambda: peer.active_workspace()["name"] == host_mark)
    claim("and reached the peer's own copy",
          peer.active_workspace()["name"], host_mark)
    if not crossed:
        note("the host's edit never crossed; nothing downstream would mean anything")

    peer_mark = "Quarterly review — with notes"
    rename_active_workspace(peer, peer_mark)
    claim("the peer's edit landed on the peer",
          peer.active_workspace()["name"], peer_mark)
    back = wait_for(lambda: host.active_workspace()["name"] == peer_mark)
    claim("and came back to the host — the document converged",
          host.active_workspace()["name"], peer_mark)
    if not back:
        note("the peer's edit never crossed back")

    # The room's own change graph, read off each replica. Two distinct
    # automerge actors is what "two people wrote this" looks like in the
    # document itself, independent of what either pane chose to display.
    host.run_palette_command("show room activity", expect_id="result:0")
    host.settle(1.2)
    claim("the room-activity sheet opened",
          host.chrome()["overlays"]["room_activity"], True)
    hist = host.chrome()["room_history"]
    actors = {c["actor"] for c in hist["changes"]}
    note(f"{len(hist['changes'])} changes by {len(actors)} actor(s)")
    note(f"contributors as the sheet names them: {hist['contributors']}")
    claim("the room document records changes from two contributors",
          len(actors) >= 2, True)
    claim("and the sheet names both of them",
          sorted(n.lower() for n in hist["contributors"]) == ["alice", "bob"], True)
    host.snapshot(os.path.join(shots, "13-history-both.png"))

    peer.run_palette_command("show room activity", expect_id="result:0")
    peer.settle(1.2)
    peer_hist = peer.chrome()["room_history"]
    peer_actors = {c["actor"] for c in peer_hist["changes"]}
    note(f"peer sees {len(peer_hist['changes'])} changes by "
         f"{len(peer_actors)} actor(s): {peer_hist['contributors']}")
    claim("the peer's replica of the history has both contributors too",
          len(peer_actors) >= 2, True)
    peer.snapshot(os.path.join(shots, "14-peer-history.png"))
    peer.key("Escape")
    peer.settle()

    step("8.", "roll back, and the content is what it was")
    rows = host.chrome()["room_history"]["rows"]
    note(f"{len(rows)} row(s) on screen; "
         f"{sum(1 for r in rows if r['revert_rect'])} offer a restore")
    # The newest row is the state we are in, so restoring to it would be a
    # no-op. Take the newest row *before* it that offers a restore.
    restorable = [r for r in rows if r["revert_rect"]]
    target = restorable[-2] if len(restorable) >= 2 else None
    claim("the history offers a version to restore to", bool(target), True)
    if target:
        note(f"restoring to row {target['index']}: "
             f"{target['summary']!r} by {target['author']!r}")
        x, y, w, h = target["revert_rect"]
        host.click_at(x + w / 2, y + h / 2)
        host.settle(1.2)
        restored = host.active_workspace()["name"]
        note(f"the room is now named {restored!r}")
        claim("restoring an earlier version brought the earlier content back",
              restored, host_mark)
        claim("and the restore is itself a change, not a rewrite of the past",
              len(host.chrome()["room_history"]["changes"]) > len(hist["changes"]), True)
        crossed_back = wait_for(
            lambda: peer.active_workspace()["name"] == restored)
        claim("the peer's copy followed the restore",
              peer.active_workspace()["name"], restored)
    host.snapshot(os.path.join(shots, "15-after-restore.png"))
    host.key("Escape")
    host.settle()
    host.snapshot(os.path.join(shots, "16-final.png"))


def rename_active_workspace(s: Session, name: str) -> None:
    """Rename the room through the chrome's own name-edit path.

    Opens workspace settings, clicks the name field, replaces what is
    there and commits with Enter — the sequence a user performs, so the
    change is stamped into the room document the same way theirs is.
    """
    s.run_palette_command("workspace settings", expect_id="result:0")
    if not s.chrome()["overlays"]["ws_settings"]:
        raise RuntimeError(f"{s.name}: workspace settings did not open")
    # The modal reopens on whichever tab it was last left on, and the
    # name field only exists on General.
    if s.chrome()["ws_settings_tab"] != "General":
        s.click("ws_settings_tab[general]")
    s.click("ws_settings_name_input")
    if s.chrome()["editing_workspace_name"] is None:
        raise RuntimeError(f"{s.name}: the name field did not take focus")
    for _ in range(64):
        s.key("Backspace")
    s.type_text(name)
    s.key("Enter")
    s.settle()
    s.close_ws_settings()


if __name__ == "__main__":
    sys.exit(main())
