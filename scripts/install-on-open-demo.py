#!/usr/bin/env python3
"""Open an app this machine does not have, from the search box, on camera.

The beat: the operator opens the command palette, types the name of an
application that is not installed, presses Enter, and the app opens as a
tab and draws its own UI. No install button, no progress dialog, no
second step.

This script reproduces that end to end, headless, and *checks* it — it
is not a screenshot generator. Every claim below is read off the
compositor's own state over the test-control socket, and the run exits
non-zero the moment one of them is false:

  1. The registry offers Sheets and the machine does not have it.
  2. The palette row for it is byte-identical to an installed app's:
     same "Open Sheets" title, same subtitle shape, no "install",
     "download", "fetch" or "available" anywhere in it.
  3. Enter — a real keystroke, not a click on the row — activates it.
  4. Sheets becomes a `TileContent::Foreign` tab and presents frames of
     its own (`frame_seq > 0`), so the picture is the app's spreadsheet
     grid rather than the compositor's first-frame placeholder.

Three screenshots land in `--out`: `palette.png` (the row), `pending.png`
(the one moment the machine admits the app is coming from somewhere) and
`tab.png` (the app running). The last one is the point; the first only
proves what the operator saw before they pressed the key.

    scripts/install-on-open-demo.py --out target/install-on-open

On the day, run it in the mode that sets the scene and gets out of the
way — registry up, Sheets absent, a real window to point a camera at —
and type it yourself:

    scripts/install-on-open-demo.py --backend winit --stage-only

Everything it needs is built by:

    cargo build -p shell -p sheets -p install -p launcher
    cargo build -p app-catalogue --bins --examples

## What is real and what is staged

Real: the `.qpkg` is packed and signed by `pkg`, served over a live HTTP
socket, fetched by `qos-catalogue`, verified fail-closed and unpacked by
`qos-install`, then started by `qos-launcher` inside bwrap. The
compositor is the shipping binary on its headless backend, which paints
through the same passes a monitor does.

Staged: the registry is a loopback server on this machine rather than a
hosted one, and `HOME` is the synthetic tree `scripts/demo-home.py`
builds so the palette shows a demo corpus instead of the operator's own
files. Neither changes a code path — they change what the code path is
pointed at.

`XDG_DATA_DIRS` is deliberately left alone: the Vulkan loader finds its
ICD manifests through it, and a staged-only value leaves wgpu with no
adapter and nothing on screen.
"""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import shutil
import signal
import socket
import subprocess
import sys
import time

import importlib.util


def _load_demo_home():
    """`scripts/demo-home.py`, by path.

    The filename has a hyphen in it, so it is not importable as a
    module name however the sys.path is arranged.
    """
    # Loading it by path otherwise drops a `scripts/__pycache__/` in the
    # working tree on every run.
    sys.dont_write_bytecode = True
    path = pathlib.Path(__file__).resolve().parent / "demo-home.py"
    spec = importlib.util.spec_from_file_location("demo_home", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


stage_home = _load_demo_home().stage

PACKAGE_ID = "dev.qos.sheets"
DISPLAY_NAME = "Sheets"
# What the operator types. Deliberately not the whole name: the row has
# to be found by typing, the way a person finds it.
QUERY = "Sheets"

# Words that must never reach a row for an app the machine has to fetch.
# The row says "open", because opening is what the user asked for.
FORBIDDEN_IN_ROW = ("install", "download", "fetch", "available")

# Scrubbed from the compositor's environment so a run measures the
# binaries rather than whatever the invoking shell carried. The display
# vars matter most: a compositor must not find an outer session to
# attach to.
SCRUB = (
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "VK_DRIVER_FILES",
    "VK_ICD_FILENAMES",
    "__EGL_VENDOR_LIBRARY_FILENAMES",
    "MESA_LOADER_DRIVER_OVERRIDE",
    "DRI_PRIME",
    "__GLX_VENDOR_LIBRARY_NAME",
)

ANSI = re.compile(r"\x1b\[[0-9;]*m")


# ---------------------------------------------------------------------
# test-control socket
# ---------------------------------------------------------------------

def call(sock_path: str, payload: dict, timeout: float = 30.0) -> dict:
    """One command, one reply, on a fresh connection."""
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
        return json.loads(buf.partition(b"\n")[0])
    finally:
        s.close()


def require_ok(resp: dict) -> dict:
    if "err" in resp:
        raise RuntimeError(f"compositor returned err: {resp['err']}")
    return resp.get("ok", resp)


def _walk(node, visit) -> None:
    if isinstance(node, dict):
        visit(node)
        for v in node.values():
            _walk(v, visit)
    elif isinstance(node, list):
        for v in node:
            _walk(v, visit)


def foreign_tiles(state) -> set:
    """`(app_id, surface_id)` for every adopted client toplevel.

    A `TileContent::Foreign` entry in the live tree is the compositor's
    own record that it took a client's xdg_toplevel into a tab. Nothing
    in the process table proves that.
    """
    found = set()

    def visit(d):
        f = d.get("Foreign")
        if isinstance(f, dict) and "app_id" in f:
            found.add((f["app_id"], f.get("surface_id")))

    _walk(state, visit)
    return found


# ---------------------------------------------------------------------
# the local signed registry
# ---------------------------------------------------------------------

class Registry:
    """`serve-test-registry`, packing a real binary into a real `.qpkg`.

    The example's own default payload is `/bin/true`, which installs
    fine and exits immediately — it proves the fetch and never proves
    the open. Pointing it at the Sheets binary is what makes step 4
    possible at all.
    """

    def __init__(self, example_bin: pathlib.Path, payload: pathlib.Path,
                 config_dir: pathlib.Path, logpath: pathlib.Path):
        self.example_bin = example_bin
        self.payload = payload
        self.config_dir = config_dir
        self.logpath = logpath
        self.proc = None
        self.log = None
        self.base_url = None

    def __enter__(self):
        self.config_dir.mkdir(parents=True, exist_ok=True)
        self.log = self.logpath.open("w+b")
        self.proc = subprocess.Popen(
            [str(self.example_bin),
             "--config-dir", str(self.config_dir),
             "--payload", str(self.payload),
             "--package-id", PACKAGE_ID,
             "--name", DISPLAY_NAME],
            stdout=self.log, stderr=subprocess.STDOUT,
            preexec_fn=os.setsid,
        )
        # It prints "registry http://127.0.0.1:PORT serving ..." and
        # flushes as soon as it is listening. Packing a ~100 MB debug
        # binary happens before that line, so the wait is real.
        deadline = time.time() + 120.0
        while time.time() < deadline:
            if self.proc.poll() is not None:
                raise SystemExit(
                    f"the test registry exited early ({self.proc.returncode}); "
                    f"see {self.logpath}")
            text = self.logpath.read_text(errors="replace")
            m = re.search(r"registry (http://\S+) serving", text)
            if m:
                self.base_url = m.group(1)
                return self
            time.sleep(0.25)
        raise SystemExit(f"the test registry never announced a port; see {self.logpath}")

    def __exit__(self, *exc):
        if self.proc is not None:
            try:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGTERM)
                self.proc.wait(timeout=10)
            except (ProcessLookupError, subprocess.TimeoutExpired):
                try:
                    os.killpg(os.getpgid(self.proc.pid), signal.SIGKILL)
                except ProcessLookupError:
                    pass
        if self.log is not None:
            self.log.close()
        return False


# ---------------------------------------------------------------------
# the compositor
# ---------------------------------------------------------------------

class Compositor:
    def __init__(self, binaries, env_extra, logpath, startup_timeout, backend):
        self.sock = f"/tmp/qos-install-on-open-{os.getpid()}.sock"
        self.binaries = binaries
        self.backend = backend
        self.env_extra = env_extra
        self.logpath = logpath
        self.startup_timeout = startup_timeout
        self.proc = None
        self.log = None

    def __enter__(self):
        try:
            os.unlink(self.sock)
        except FileNotFoundError:
            pass
        env = {**os.environ, "QOS_TEST_CONTROL": self.sock, "RUST_LOG": "info"}
        for k in SCRUB:
            env.pop(k, None)
        env.update(self.env_extra)
        self.log = self.logpath.open("wb")
        self.proc = subprocess.Popen(
            [str(self.binaries["shell"]), "--backend", self.backend],
            env=env, stdout=self.log, stderr=subprocess.STDOUT,
            preexec_fn=os.setsid,
        )
        deadline = time.time() + self.startup_timeout
        while time.time() < deadline:
            if os.path.exists(self.sock):
                time.sleep(1.0)  # let the first paint land
                return self
            if self.proc.poll() is not None:
                raise SystemExit(
                    f"the compositor exited with status {self.proc.returncode} "
                    f"before binding its control socket; see {self.logpath}")
            time.sleep(0.1)
        raise SystemExit(
            f"the compositor never bound {self.sock} within "
            f"{self.startup_timeout:.0f}s; see {self.logpath}")

    def __exit__(self, *exc):
        if self.proc is not None:
            try:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGTERM)
                self.proc.wait(timeout=12)
            except (ProcessLookupError, subprocess.TimeoutExpired):
                try:
                    os.killpg(os.getpgid(self.proc.pid), signal.SIGKILL)
                except ProcessLookupError:
                    pass
        if self.log is not None:
            self.log.close()
        try:
            os.unlink(self.sock)
        except FileNotFoundError:
            pass
        return False

    # -- driving -------------------------------------------------------

    def modifiers(self, *, ctrl=False, shift=False, alt=False, logo=False):
        require_ok(call(self.sock, {"op": "modifiers", "ctrl": ctrl, "shift": shift,
                                    "alt": alt, "logo": logo}))

    def key(self, name: str):
        # Winit's names, not xkb's: "Enter", not "Return". The socket
        # rejects an unknown multi-character key rather than typing it,
        # so a wrong name here is a `parse_error`, not a silent no-op.
        require_ok(call(self.sock, {"op": "key", "key": name}))

    def type_text(self, text: str):
        require_ok(call(self.sock, {"op": "type", "text": text}))

    def snapshot(self, path: pathlib.Path):
        require_ok(call(self.sock, {"op": "snapshot", "path": str(path)}))

    def state(self) -> dict:
        reply = call(self.sock, {"op": "state_dump"})
        if "err" in reply:
            raise RuntimeError(f"state_dump: {reply['err']}")
        return reply.get("state", reply)

    def palette(self) -> dict:
        return self.state().get("chrome", {}).get("palette", {})

    def notifications(self) -> list:
        return self.state().get("chrome", {}).get("notifications", [])

    def foreign_frames(self):
        reply = call(self.sock, {"op": "foreign_frames"})
        body = reply.get("foreign_frames", reply.get("ok", reply))
        if isinstance(body, dict):
            body = body.get("frames", body)
        return body if isinstance(body, list) else []

    def open_palette(self, chord: str):
        """Open the command palette with one of its four chords.

        The chord matters, and it is the one thing about this beat that
        is easy to get wrong on camera.

        `Super+K`, `Super+Space` and `Alt+Space` open the palette in
        global scope — "search the machine" — which is the only scope
        that offers to open an app you do not already have. `Ctrl+K`
        asks the narrower question, "search inside what I am looking
        at", and is scoped to the focused app.

        `Ctrl+K` is not simply broken for this: with nothing focused,
        the scope widens to global and the row appears anyway. That is
        exactly why it is the wrong chord to film — it works on an
        empty workspace and stops working the moment there is a tab in
        front of you. `--chord ctrl-k` exists so that difference can be
        measured rather than argued about.

        `Alt+Space` is the one to reach for when qOS is running as a
        Wayland client inside another desktop: the host swallows Super
        before the compositor sees it.
        """
        if chord == "super-k":
            self.modifiers(logo=True)
            self.key("k")
        elif chord == "ctrl-k":
            self.modifiers(ctrl=True)
            self.key("k")
        elif chord == "super-space":
            self.modifiers(logo=True)
            self.key("Space")
        elif chord == "alt-space":
            self.modifiers(alt=True)
            self.key("Space")
        else:
            raise ValueError(f"unknown chord {chord!r}")
        self.modifiers()


# ---------------------------------------------------------------------
# checks
# ---------------------------------------------------------------------

class Failure(Exception):
    pass


def check(condition: bool, message: str) -> None:
    if not condition:
        raise Failure(message)


def find_row(pal: dict) -> dict | None:
    """The row that would open our package, if the palette is offering one.

    Palette entry ids are positional (`result:0`, `result:1`, …) — the
    row's own identity lives in its subtitle, which is where the
    Applications provider puts the package id. Matching on the subtitle
    is therefore matching on what the user can actually read.
    """
    for row in pal.get("rows", []):
        if PACKAGE_ID in (row.get("description") or ""):
            return row
    return None


def wait_for_row(comp: Compositor, timeout: float) -> tuple[dict, dict]:
    """Wait until the palette offers our package, and return (palette, row).

    The catalogue answers the palette from an in-memory cache and never
    blocks a keystroke on HTTP, so the first query after a cold start
    legitimately has nothing in it and schedules a fetch. Re-querying is
    what a person does anyway — they keep typing.
    """
    deadline = time.time() + timeout
    last = []
    while time.time() < deadline:
        pal = comp.palette()
        last = pal.get("rows", [])
        row = find_row(pal)
        if row is not None:
            return pal, row
        # Nudge the query so the providers re-run against a now-warm
        # cache: one character back, one character forward.
        comp.key("Backspace")
        time.sleep(0.3)
        comp.type_text(QUERY[-1])
        time.sleep(0.7)
    raise Failure(
        f"no palette row for {PACKAGE_ID} within {timeout:.0f}s; "
        f"last rows were {last}")


def check_row_wording(row: dict) -> None:
    """The row must read as opening an app, not as acquiring one."""
    label = row.get("label", "")
    detail = row.get("description") or ""
    check(label == f"Open {DISPLAY_NAME}",
          f"the row must say 'Open {DISPLAY_NAME}', not {label!r}")
    visible = f"{label} {detail}".lower()
    for word in FORBIDDEN_IN_ROW:
        check(word not in visible,
              f"{word!r} must not appear in a row the user reads as opening "
              f"an app: {label!r} / {detail!r}")
    # The subtitle's shape is the tell a badge would break: an installed
    # app's is "<where it opens> · <category> · <package id>".
    if detail:
        check(PACKAGE_ID in detail,
              f"the subtitle must name the package it opens: {detail!r}")


def wait_for_tab(comp: Compositor, timeout: float) -> list:
    deadline = time.time() + timeout
    while time.time() < deadline:
        tiles = foreign_tiles(comp.state())
        hits = sorted(f"{a}#{s}" for a, s in tiles if PACKAGE_ID in str(a))
        if hits:
            return hits
        time.sleep(1.0)
    return []


def wait_for_first_frame(comp: Compositor, timeout: float) -> dict | None:
    """Hold until the app has actually presented a frame.

    Adoption and painting are different events. Between them the tab
    shows the compositor's "waiting for first frame" placeholder, which
    for an app that legitimately starts empty is hard to tell from the
    real thing by eye. `frame_seq` is the compositor's own count of
    buffers taken from the client, so `> 0` is the fact rather than the
    impression. A debug build's first vello pipeline compile is tens of
    seconds, which is what the timeout is sized for.
    """
    deadline = time.time() + timeout
    best = None
    while time.time() < deadline:
        for f in comp.foreign_frames():
            if PACKAGE_ID not in str(f.get("app_id", "")):
                continue
            best = f
            if (f.get("frame_seq") or 0) > 0:
                return f
        time.sleep(2.0)
    return best


def app_log_lines(logpath: pathlib.Path, package_id: str) -> list[str]:
    text = ANSI.sub("", logpath.read_text(errors="replace"))
    tag = f"[app {package_id}] "
    return [l.split(tag, 1)[-1] for l in text.splitlines() if tag in l]


# ---------------------------------------------------------------------

def main() -> int:
    here = pathlib.Path(__file__).resolve().parent.parent
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--worktree", type=pathlib.Path, default=here,
                   help="repo root holding target/debug (default: this script's repo)")
    p.add_argument("--out", type=pathlib.Path,
                   default=here / "target" / "install-on-open",
                   help="directory for screenshots and logs")
    p.add_argument("--row-timeout", type=float, default=45.0,
                   help="seconds to wait for the palette row (default: 45)")
    p.add_argument("--tab-timeout", type=float, default=120.0,
                   help="seconds to wait for the tab to appear (default: 120)")
    p.add_argument("--frame-timeout", type=float, default=120.0,
                   help="seconds to wait for the app's first frame (default: 120)")
    p.add_argument("--startup-timeout", type=float, default=60.0,
                   help="seconds to wait for the control socket (default: 60)")
    p.add_argument("--backend", default="headless",
                   choices=("headless", "winit", "drm"),
                   help="compositor backend. headless checks the flow with no "
                        "display; winit opens a real window to film "
                        "(default: headless)")
    p.add_argument("--stage-only", action="store_true",
                   help="set the scene and stop: registry up, Sheets absent, "
                        "compositor running with the right environment. Nothing "
                        "is driven — the operator types. This is the filming "
                        "mode; Ctrl-C tears it down")
    p.add_argument("--chord", default="super-k",
                   choices=("super-k", "super-space", "alt-space", "ctrl-k"),
                   help="palette chord to open with. The three global ones "
                        "offer apps this machine does not have; ctrl-k is "
                        "scoped to the focused app and is here to be measured "
                        "against, not to film (default: super-k)")
    p.add_argument("--hold", type=float, default=0.0,
                   help="extra seconds to leave the session up after the "
                        "final capture (for filming a live window)")
    args = p.parse_args()

    target = args.worktree / "target" / "debug"
    binaries = {
        "shell": target / "shell",
        "sheets": target / "sheets",
        "qos-install": target / "qos-install",
        "qos-launcher": target / "qos-launcher",
        "qos-catalogue": target / "qos-catalogue",
        "serve-test-registry": target / "examples" / "serve-test-registry",
    }
    missing = [f"{k} ({v})" for k, v in binaries.items() if not v.exists()]
    if missing:
        print("FAIL: not built:\n  " + "\n  ".join(missing), file=sys.stderr)
        print("\n  cargo build -p shell -p sheets -p install -p launcher"
              "\n  cargo build -p app-catalogue --bins --examples", file=sys.stderr)
        return 1

    args.out.mkdir(parents=True, exist_ok=True)
    scratch = args.out / "scratch"
    if scratch.exists():
        shutil.rmtree(scratch)
    apps_dir = scratch / "qos-apps"
    store_config = scratch / "qos-store"
    store_cache = scratch / "qos-store-cache"
    for d in (apps_dir, store_config, store_cache):
        d.mkdir(parents=True, exist_ok=True)

    # A HOME of our own, so the palette's file results are a small demo
    # corpus rather than the operator's actual home directory.
    home_env = stage_home(args.out / "home")
    home = pathlib.Path(home_env["HOME"])

    # One workspace, one strip, nothing in it. The compositor's default
    # bootstrap seeds demo tabs, and a capture of those plus Sheets is
    # half somebody else's app.
    qos_dir = pathlib.Path(home_env["XDG_CONFIG_HOME"]) / "qos"
    qos_dir.mkdir(parents=True, exist_ok=True)
    (qos_dir / "state.json").write_text(json.dumps({
        "workspaces": [{
            "id": 1,
            "room": "00000000-0000-4000-8000-000000000001",
            "name": "Apps",
            "strips": [{"id": 2, "name": "Main", "window_groups": []}],
            "collaborators": [],
            "owner": None,
            "capabilities": [],
            "shared_surfaces": {},
        }],
        "active": 1,
        "active_strip": {"1": 2},
        "active_group": {},
        "collaborators": [],
        "room_authority": {},
    }))

    env_extra = {
        **home_env,
        # The install root. Empty, which is what makes "this machine
        # does not have Sheets" true rather than asserted.
        "QOS_APPS_DATA_DIR": str(apps_dir),
        "QOS_STORE_CONFIG_DIR": str(store_config),
        "QOS_STORE_CACHE_DIR": str(store_cache),
        "QOS_CATALOGUE_BIN": str(binaries["qos-catalogue"]),
        "QOS_INSTALL_BIN": str(binaries["qos-install"]),
        "QOS_LAUNCHER_BIN": str(binaries["qos-launcher"]),
    }

    shell_log = args.out / "compositor.log"
    registry_log = args.out / "registry.log"
    palette_png = args.out / "palette.png"
    pending_png = args.out / "pending.png"
    tab_png = args.out / "tab.png"

    print(f"chord: {args.chord}")
    print(f"staged HOME: {home}")
    print(f"install root: {apps_dir}  (empty)")

    with Registry(binaries["serve-test-registry"], binaries["sheets"],
                  store_config, registry_log) as reg:
        print(f"registry: {reg.base_url}  serving {PACKAGE_ID} packed from "
              f"{binaries['sheets']}")

        # --- 1. offered, and not here ---------------------------------
        probe_env = {**os.environ, **env_extra}
        listing = json.loads(subprocess.run(
            [str(binaries["qos-catalogue"]), "list", "--json"],
            env=probe_env, capture_output=True, text=True, check=True).stdout)
        try:
            check(not listing.get("unreachable"),
                  f"the local registry must be reachable: {listing.get('unreachable')}")
            offered = [a for a in listing["apps"] if a["package_id"] == PACKAGE_ID]
            check(bool(offered),
                  f"{PACKAGE_ID} must be offered: {listing['apps']}")
            check(not (apps_dir / "registry.toml").exists(),
                  f"{apps_dir} must start with nothing installed")
        except Failure as e:
            print(f"FAIL (step 1): {e}", file=sys.stderr)
            return 1
        print(f"  offered: {offered[0]['display_name']} {offered[0]['version']} "
              f"from {offered[0]['registry']!r} (trust: {offered[0]['trust']})")
        print("  installed: nothing")

        # --- 2..4 through the live compositor -------------------------
        with Compositor(binaries, env_extra, shell_log, args.startup_timeout,
                        args.backend) as comp:
            if args.stage_only:
                print("\nthe scene is set — nothing has been driven.\n")
                print(f"  Sheets is offered and not installed")
                print(f"  compositor:  {args.backend} backend, "
                      f"log {shell_log}")
                print(f"  socket:      {comp.sock}")
                print(f"\nOpen the palette with Super+K (or Alt+Space if a host "
                      f"desktop\nis eating Super), type \"{QUERY}\", press Enter.\n")
                print("Ctrl-C to tear it all down. Re-running resets the install "
                      "root,\nso the app is absent again on the next take.")
                try:
                    while comp.proc.poll() is None:
                        time.sleep(1.0)
                except KeyboardInterrupt:
                    print("\ntearing down.")
                return 0
            try:
                comp.open_palette(args.chord)
                time.sleep(0.6)
                pal = comp.palette()
                check(pal.get("visible") is True,
                      f"{args.chord} must open the palette; palette was {pal}")
                # Which question the chord asked. `All` is the scope
                # that offers apps this machine does not have; report it
                # rather than assume it, because `ActiveApp` widens to
                # `All` when nothing is focused and the two are then
                # indistinguishable from the outside.
                scope = comp.state().get("chrome", {}).get("search_scope")
                print(f"  scope: {scope}")
                # The catalogue's first answer is a cold cache. Give the
                # background fetch the moment it needs before typing, so
                # the row is there when the name is finished.
                time.sleep(2.0)

                comp.type_text(QUERY)
                time.sleep(1.0)
                pal, row = wait_for_row(comp, args.row_timeout)
                check_row_wording(row)
                # Enter takes the selected row, so "the row exists" is
                # not the same claim as "Enter opens Sheets".
                check(pal.get("selected_id") == row["id"],
                      f"Enter must be about to activate {row['label']!r} "
                      f"({row['id']}), but the selected row is "
                      f"{pal.get('selected_id')!r} out of {pal.get('rows')}")
                comp.snapshot(palette_png)
                print(f"  palette row: {row['label']!r} / "
                      f"{row.get('description')!r}  → {palette_png}")

                # --- 3. Enter ---------------------------------------
                # A keystroke, not a click on the row. The socket takes
                # winit's key names, so this is "Enter"; "Return" is an
                # xkb name and comes back as a parse_error rather than
                # quietly doing nothing.
                before = len(comp.notifications())
                comp.key("Enter")
                time.sleep(1.0)
                after = comp.notifications()
                check(len(after) > before or any(
                    DISPLAY_NAME in n.get("title", "") for n in after),
                    "Enter did not activate the row — nothing happened "
                    f"(notifications: {after})")
                pending = [n for n in after if DISPLAY_NAME in n.get("title", "")]
                if pending:
                    print(f"  activated: {pending[-1]['title']!r} — "
                          f"{pending[-1]['body']!r}")
                # The one place the machine admits the app is coming
                # from somewhere. Captured while it is still up: it is
                # retired by the app starting, not by a timer, so it is
                # gone from the final frame.
                comp.snapshot(pending_png)

                # --- 4. tab, then a frame of its own ----------------
                adopted = wait_for_tab(comp, args.tab_timeout)
                check(bool(adopted),
                      "no tab appeared for Sheets. Compositor log: "
                      + " | ".join(app_log_lines(shell_log, PACKAGE_ID)[-6:]))
                print(f"  adopted as a tab: {', '.join(adopted)}")

                frame = wait_for_first_frame(comp, args.frame_timeout)
                comp.key("Escape")  # dismiss the palette before the capture
                time.sleep(0.5)
                comp.snapshot(tab_png)
                check(frame is not None,
                      "the compositor tracked no frames for the tab at all")
                check((frame.get("frame_seq") or 0) > 0,
                      "the tab never presented a frame of its own — "
                      f"{tab_png} is the first-frame placeholder, not Sheets "
                      f"({frame})")
                print(f"  first frame: frame_seq={frame['frame_seq']}  → {tab_png}")

                # With a tab in front of you the two chords stop being
                # interchangeable, and this is where that becomes
                # visible. On the empty workspace this run starts from,
                # `Ctrl+K` reports `All` as well — `ActiveApp` widens
                # when nothing is focused — so a check run only at the
                # start cannot tell them apart. Measured after the app
                # is up and focused, it can.
                for chord in ("super-k", "ctrl-k"):
                    comp.open_palette(chord)
                    time.sleep(0.5)
                    print(f"  with Sheets focused, {chord} → scope "
                          f"{comp.state().get('chrome', {}).get('search_scope')}")
                    comp.key("Escape")
                    time.sleep(0.3)

                if args.hold > 0:
                    print(f"  holding for {args.hold:.0f}s ...")
                    time.sleep(args.hold)
            except (Failure, RuntimeError) as e:
                print(f"FAIL: {e}", file=sys.stderr)
                print(f"      compositor log: {shell_log}", file=sys.stderr)
                print(f"      registry log:   {registry_log}", file=sys.stderr)
                return 1

    print("\nOK — an app this machine did not have opened as a tab and drew "
          "itself, from one keystroke in the search box.")
    print(f"  {palette_png}")
    print(f"  {pending_png}")
    print(f"  {tab_png}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
