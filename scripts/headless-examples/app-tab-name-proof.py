#!/usr/bin/env python3
"""A tab carries the app's name.

Files was showing up in its own tab as `app`, and the word processor as
`word-processor`. Both are machine identifiers the user never chose:
`app` is the trailing component of the generic `org.qos.app` every
toolkit app used to announce when it had no package id, and
`word-processor` is a package id's last segment printed raw.

So this launches both apps through the real palette, into a real
sandbox, and reads the tab titles the compositor built — plus the
wayland app-id each client actually announced, because that is where
the failure came from and asserting only on the label would not say
which half was fixed.

  claim 1   every tab title is the name the app's manifest declares
  claim 2   no tab is labelled `app`, or any raw package-id segment
  claim 3   the app-id on the wire is the package id, not a shared one

Usage
-----
    scripts/headless-examples/app-tab-name-proof.py

    cargo build -p shell -p launcher

Runs against the machine's own app store
(`~/.local/share/qos-apps`) — those are the packages the demo opens, and
a synthesised fixture would prove nothing about them.

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

from reaper import die_with_parent, track

REPO = pathlib.Path(__file__).resolve().parents[2]

# (package id, what the manifest calls it, what it must never read as)
APPS = [
    ("dev.qos.file-explorer", "Files", ("app", "file-explorer")),
    ("dev.qos.word-processor", "Word Processor", ("app", "word-processor")),
]

SCRUB = ("DISPLAY", "WAYLAND_DISPLAY", "VK_DRIVER_FILES", "VK_ICD_FILENAMES",
         "__EGL_VENDOR_LIBRARY_FILENAMES", "MESA_LOADER_DRIVER_OVERRIDE",
         "DRI_PRIME", "__GLX_VENDOR_LIBRARY_NAME")

failures: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return bool(ok)


def step(what: str) -> None:
    print(f"\n· {what}")


class Failure(Exception):
    pass


def call(sock_path: str, payload: dict, timeout: float = 60.0) -> dict:
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


class Session:
    def __init__(self, root: pathlib.Path, shell: pathlib.Path, tools: dict,
                 apps_dir: pathlib.Path, runtime_bundles: pathlib.Path,
                 short_root: pathlib.Path):
        self.dir = root
        self.dir.mkdir(parents=True, exist_ok=True)
        short_root.mkdir(parents=True, exist_ok=True)
        self.sock = str(short_root / "c.sock")
        runtime = short_root / "run"
        runtime.mkdir(mode=0o700, exist_ok=True)
        home = self.dir / "home"
        home.mkdir(exist_ok=True)

        env = {k: v for k, v in os.environ.items() if k not in SCRUB}
        env.update({
            "QOS_TEST_CONTROL": self.sock,
            "QOS_STATE_PATH": str(self.dir / "config" / "state.json"),
            "HOME": str(home),
            "XDG_DATA_HOME": str(self.dir / "data"),
            "XDG_CACHE_HOME": str(self.dir / "cache"),
            "XDG_STATE_HOME": str(self.dir / "state"),
            "XDG_CONFIG_HOME": str(self.dir / "config"),
            "XDG_RUNTIME_DIR": str(runtime),
            "QOS_PEER_KEY_PATH": str(self.dir / "keys" / "peer-key"),
            "QOS_APPS_DATA_DIR": str(apps_dir),
            "QOS_RUNTIME_BUNDLE_DIR": str(runtime_bundles),
            "NIRIOS_HEADLESS_SIZE": "2000x1000",
            # de_DE host, and bwrap does not clear the environment.
            "LC_ALL": "en_US.UTF-8",
            "LANG": "en_US.UTF-8",
            "RUST_LOG": "info",
        })
        env.update(tools)
        self.log_path = self.dir / "shell.log"
        self.log = self.log_path.open("w")
        self.proc = subprocess.Popen(
            # `--mode native`: the smithay compositor, the only mode a
            # sandboxed client can attach to. A tab label for a foreign
            # toplevel does not exist without one.
            [str(shell), "--mode", "native", "--backend", "headless"],
            env=env, stdout=self.log, stderr=subprocess.STDOUT,
            preexec_fn=die_with_parent(),
        )
        track(self.proc)

    def wait_up(self, timeout: float = 120.0) -> bool:
        end = time.time() + timeout
        while time.time() < end:
            if os.path.exists(self.sock):
                time.sleep(1.0)
                return True
            if self.proc.poll() is not None:
                return False
            time.sleep(0.05)
        return False

    def ok(self, payload: dict) -> dict:
        r = call(self.sock, payload)
        if "err" in r:
            raise Failure(f"{payload.get('op')}: {r['err']}")
        return r.get("ok", r)

    def key(self, name: str):
        self.ok({"op": "key", "key": name})

    def modifiers(self, **kw):
        self.ok({"op": "modifiers", "ctrl": kw.get("ctrl", False),
                 "shift": kw.get("shift", False), "alt": kw.get("alt", False),
                 "logo": kw.get("logo", False)})

    def type_text(self, text: str):
        self.ok({"op": "type", "text": text})

    def snapshot(self, path: pathlib.Path):
        self.ok({"op": "snapshot", "path": str(path)})

    def state(self) -> dict:
        r = call(self.sock, {"op": "state_dump"})
        if "err" in r:
            raise Failure(f"state_dump: {r['err']}")
        return r.get("state", r)

    def palette(self) -> dict:
        return self.state().get("chrome", {}).get("palette", {})

    def foreign_tabs(self) -> list[tuple[str, str]]:
        """(tab title, wayland app-id) for every adopted client."""
        tree = self.state().get("workspace_tree", {})
        out = []
        for ws in tree.get("workspaces", []) or []:
            for strip in ws.get("strips", []) or []:
                for g in strip.get("window_groups", []) or []:
                    for t in g.get("tabs", []) or []:
                        tile = t.get("tile")
                        if isinstance(tile, dict) and "Foreign" in tile:
                            out.append((t.get("title", ""),
                                        tile["Foreign"].get("app_id", "")))
        return out

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


def open_app(s: Session, query: str, package_id: str, timeout=120.0) -> bool:
    """Open one app the way a person does: palette, name, Enter."""
    s.modifiers(logo=True)
    s.key("k")
    s.modifiers()
    time.sleep(0.6)
    if s.palette().get("visible") is not True:
        raise Failure("the palette did not open")
    s.type_text(query)
    time.sleep(1.0)
    pal = s.palette()
    row = None
    for r in pal.get("rows", []):
        if package_id in json.dumps(r):
            row = r
            break
    if row is None:
        raise Failure(f"no palette row for {package_id}; "
                      f"rows={json.dumps(pal.get('rows', []))[:400]}")
    if pal.get("selected_id") != row.get("id"):
        raise Failure(f"Enter would activate {pal.get('selected_id')!r}, "
                      f"not {row.get('id')!r}")
    s.key("Enter")
    end = time.time() + timeout
    while time.time() < end:
        for _, app_id in s.foreign_tabs():
            last = package_id.rsplit(".", 1)[-1]
            if app_id.lower() in (package_id.lower(), last.lower()):
                return True
        time.sleep(0.4)
    return False


def main() -> int:
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--out", type=pathlib.Path,
                   default=REPO / "target" / "app-tab-name-proof")
    p.add_argument("--shell", type=pathlib.Path, default=None)
    p.add_argument("--apps-dir", type=pathlib.Path, default=None)
    p.add_argument("--keep", action="store_true")
    args = p.parse_args()

    def tool(name: str) -> pathlib.Path | None:
        for profile in ("demo", "debug"):
            path = REPO / "target" / profile / name
            if path.exists():
                return path
        return None

    shell = args.shell or tool("shell")
    apps_dir = args.apps_dir or (pathlib.Path.home() / ".local/share/qos-apps")
    runtime_bundles = pathlib.Path.home() / ".local/share/qos-runtime"
    tools = {}
    for var, name in (("QOS_LAUNCHER_BIN", "qos-launcher"),):
        found = tool(name)
        if found:
            tools[var] = str(found)

    missing = []
    if not shell or not shell.exists():
        missing.append("shell")
    if "QOS_LAUNCHER_BIN" not in tools:
        missing.append("qos-launcher")
    if not (apps_dir / "registry.toml").exists():
        missing.append(f"an app store at {apps_dir}")
    if not runtime_bundles.exists():
        missing.append(f"a runtime bundle store at {runtime_bundles}")
    if missing:
        print("FAIL: not available:\n  " + "\n  ".join(missing), file=sys.stderr)
        print("\n  cargo build -p shell -p launcher", file=sys.stderr)
        return 2

    src = REPO / "shell" / "src"
    newest = max((f.stat().st_mtime for f in src.rglob("*.rs")), default=0)
    if shell.stat().st_mtime < newest:
        print(f"REFUSING: {shell} predates shell/src. Build first.",
              file=sys.stderr)
        return 2

    args.out.mkdir(parents=True, exist_ok=True)
    root = args.out / "session"
    shutil.rmtree(root, ignore_errors=True)
    short_root = pathlib.Path(tempfile.mkdtemp(prefix="/tmp/qs-"))

    print(f"shell:    {shell}")
    print(f"app store: {apps_dir}")

    s = Session(root, shell, tools, apps_dir, runtime_bundles, short_root)
    try:
        step("a session, and the apps opened by name from the palette")
        if not check("the compositor came up", s.wait_up()):
            print(s.log_path.read_text()[-3000:])
            return 1

        for package_id, expected, _ in APPS:
            query = expected.lower()
            opened = open_app(s, query, package_id)
            if not check(f"{expected} opened", opened):
                print(s.log_path.read_text()[-3000:])
                return 1
            time.sleep(1.5)

        time.sleep(2.0)
        # Two apps at their natural widths scroll the first group's tab
        # bar off the left of the strip, and a shot that cannot show
        # both labels does not settle a claim about both labels. Give
        # each group a share that fits.
        s.ok({"op": "set_group_width", "permille": 420})
        time.sleep(1.0)
        # The strip scrolls to follow focus, and opening the second app
        # left the first one's tab bar off the left edge. Focus the first
        # group again so the strip scrolls back to it; at this width the
        # second stays beside it.
        s.ok({"op": "click_at", "x": 200.0, "y": 600.0})
        time.sleep(1.5)
        s.snapshot(args.out / "tabs.png")
        tabs = s.foreign_tabs()
        print(f"\n  tabs on screen: {tabs}")

        step("what the tabs read")
        for package_id, expected, never in APPS:
            last = package_id.rsplit(".", 1)[-1]
            mine = [(t, a) for t, a in tabs
                    if a.lower() in (package_id.lower(), last.lower())]
            if not check(f"a tab exists for {package_id}", bool(mine)):
                continue
            title, app_id = mine[0]
            check(f"it reads {expected!r}", title == expected,
                  f"reads {title!r}")
            for bad in never:
                check(f"and never {bad!r}", title != bad, f"reads {title!r}")
            check(f"the app-id on the wire is {package_id!r}",
                  app_id == package_id, f"announced {app_id!r}")

        check("no tab anywhere is labelled 'app'",
              all(t != "app" for t, _ in tabs),
              json.dumps([t for t, _ in tabs]))

        print(f"\nshot: {args.out / 'tabs.png'}")
    except Failure as e:
        print(f"\nFAILED: {e}", file=sys.stderr)
        return 1
    finally:
        s.stop()
        shutil.rmtree(short_root, ignore_errors=True)
        if not args.keep:
            shutil.rmtree(root, ignore_errors=True)

    print()
    if failures:
        print(f"FAILED: {len(failures)}")
        for f in failures:
            print(f"  - {f}")
        return 1
    print("all claims hold")
    return 0


if __name__ == "__main__":
    sys.exit(main())
