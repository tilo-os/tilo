#!/usr/bin/env python3
"""Opening an app is never silent, and never says "install".

Two claims, and the second is the one that matters.

**Nothing the user reads names the mechanism.** They typed a name and
pressed Enter; whether bytes had to arrive first is the OS's business.
So no row, subtitle, notification title or body may say installed,
install, download, fetch or available.

**No moment of the wait is unacknowledged.** Opening an app the machine
does not have takes seconds, and seconds of nothing is indistinguishable
from a keystroke that did not register. This script samples the screen
state continuously from the keystroke to the app's first frame and
requires an indication to be present in *every* sample — not merely
present at the start and again at the end, which is what a flash
followed by silence would also produce.

  t0            Enter on the selected palette row
  ...           sampled every ~150 ms: is something on screen saying so?
  first frame   the compositor has taken a buffer from the client
  after         the indication is gone, because the window is the answer

The end state is checked too. An indication that never retires is its
own defect: it would still be on screen over the running app.

What the app is
---------------
A real `.tapp`, really packed and really signed, served over loopback by
`serve-test-registry` — the same setup `install-open-timing.py` measures
against and the same one used on set. The session's app dir is empty, so
"the machine does not have it" is a fact about a directory rather than a
claim.

Usage
-----
    scripts/headless-examples/open-feedback-proof.py

    cargo build -p shell -p install -p launcher -p word-processor
    cargo build -p app-cli --bins --examples

Exits non-zero if any claim fails.
"""
from __future__ import annotations

import argparse
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
PACKAGE_ID = "so.tilo.word-processor"
DISPLAY_NAME = "Word Processor"
QUERY = "word processor"

# The vocabulary the owner banned, plus the neighbours it hides behind.
# "Part of the appeal is that you don't have to explicitly install, and
# the wording is part of the experience."
BANNED = ("install", "download", "fetch", "available", "registry", "package")

# How often the screen state is sampled across the wait. Tight enough
# that a gap a person would notice cannot fall between two samples.
SAMPLE_S = 0.15

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


def call(sock_path: str, payload: dict, timeout: float = 30.0) -> dict:
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
    """One compositor whose app dir is empty."""

    def __init__(self, root: pathlib.Path, shell: pathlib.Path, tools: dict,
                 store_config: pathlib.Path, runtime_bundles: pathlib.Path,
                 short_root: pathlib.Path):
        self.dir = root
        self.dir.mkdir(parents=True, exist_ok=True)
        short_root.mkdir(parents=True, exist_ok=True)
        self.sock = str(short_root / "c.sock")
        self.apps_dir = self.dir / "apps"
        self.store_cache = self.dir / "store-cache"
        for d in (self.apps_dir, self.store_cache):
            d.mkdir(parents=True, exist_ok=True)
        runtime = short_root / "run"
        runtime.mkdir(mode=0o700, exist_ok=True)
        home = self.dir / "home"
        home.mkdir(exist_ok=True)

        env = {k: v for k, v in os.environ.items() if k not in SCRUB}
        env.update({
            "TILO_TEST_CONTROL": self.sock,
            "TILO_STATE_PATH": str(self.dir / "config" / "state.json"),
            "HOME": str(home),
            "XDG_DATA_HOME": str(self.dir / "data"),
            "XDG_CACHE_HOME": str(self.dir / "cache"),
            "XDG_STATE_HOME": str(self.dir / "state"),
            "XDG_CONFIG_HOME": str(self.dir / "config"),
            "XDG_RUNTIME_DIR": str(runtime),
            "TILO_PEER_KEY_PATH": str(self.dir / "keys" / "peer-key"),
            # Empty, so the app is genuinely absent.
            "TILO_APPS_DATA_DIR": str(self.apps_dir),
            "TILO_RUNTIME_BUNDLE_DIR": str(runtime_bundles),
            "TILO_STORE_CONFIG_DIR": str(store_config),
            "TILO_STORE_CACHE_DIR": str(self.store_cache),
            "TILO_HEADLESS_SIZE": "1600x1000",
            # The host is de_DE; without this the chrome renders in
            # German and every string assertion below is meaningless.
            "LC_ALL": "en_US.UTF-8",
            "LANG": "en_US.UTF-8",
            "RUST_LOG": "info",
        })
        env.update(tools)
        self.log_path = self.dir / "shell.log"
        self.log = self.log_path.open("w")
        self.proc = subprocess.Popen(
            [str(shell), "--backend", "headless"],
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

    def chrome(self) -> dict:
        return self.state().get("chrome", {})

    def palette(self) -> dict:
        return self.chrome().get("palette", {})

    def notifications(self) -> list:
        return self.chrome().get("notifications", []) or []

    def tab_titles(self) -> list[str]:
        tree = self.state().get("workspace_tree", {})
        out = []
        for ws in tree.get("workspaces", []) or []:
            for strip in ws.get("strips", []) or []:
                for g in strip.get("window_groups", []) or []:
                    for t in g.get("tabs", []) or []:
                        out.append(t.get("title", ""))
        return out

    def foreign_frames(self) -> list:
        r = call(self.sock, {"op": "foreign_frames"})
        body = r.get("foreign_frames", r.get("ok", r))
        if isinstance(body, dict):
            body = body.get("frames", body)
        return body if isinstance(body, list) else []

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


class Registry:
    """`serve-test-registry` on loopback, serving one real signed `.tapp`."""

    def __init__(self, example_bin, payload, config_dir, log_path):
        self.example_bin = example_bin
        self.payload = payload
        self.config_dir = config_dir
        self.log_path = log_path
        self.proc = None
        self.log = None
        self.base_url = None

    def __enter__(self):
        self.config_dir.mkdir(parents=True, exist_ok=True)
        self.log = self.log_path.open("w+b")
        self.proc = subprocess.Popen(
            [str(self.example_bin), "--config-dir", str(self.config_dir),
             "--payload", str(self.payload), "--package-id", PACKAGE_ID,
             "--name", DISPLAY_NAME],
            stdout=self.log, stderr=subprocess.STDOUT,
            preexec_fn=die_with_parent(),
        )
        track(self.proc)
        deadline = time.time() + 300.0
        while time.time() < deadline:
            if self.proc.poll() is not None:
                raise SystemExit(f"the registry exited early "
                                 f"({self.proc.returncode}); see {self.log_path}")
            m = re.search(r"registry (http://\S+) serving",
                          self.log_path.read_text(errors="replace"))
            if m:
                self.base_url = m.group(1)
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


def banned_words_in(text: str) -> list[str]:
    low = text.lower()
    return [w for w in BANNED if w in low]


def find_row(pal: dict) -> dict | None:
    for row in pal.get("rows", []):
        if PACKAGE_ID in json.dumps(row):
            return row
    return None


def wait_for_row(s: Session, timeout: float):
    deadline = time.time() + timeout
    last = []
    while time.time() < deadline:
        pal = s.palette()
        last = pal.get("rows", [])
        row = find_row(pal)
        if row is not None:
            return pal, row
        s.key("Backspace")
        time.sleep(0.25)
        s.type_text(QUERY[-1])
        time.sleep(0.6)
    raise Failure(f"no palette row for {PACKAGE_ID} within {timeout:.0f}s; "
                  f"last rows: {json.dumps(last)[:400]}")


def main() -> int:
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--out", type=pathlib.Path,
                   default=REPO / "target" / "open-feedback-proof")
    p.add_argument("--shell", type=pathlib.Path, default=None)
    p.add_argument("--payload", type=pathlib.Path, default=None)
    p.add_argument("--timeout", type=float, default=300.0)
    p.add_argument("--keep", action="store_true")
    args = p.parse_args()

    def tool(name: str) -> pathlib.Path | None:
        for profile in ("demo", "debug"):
            path = REPO / "target" / profile / name
            if path.exists():
                return path
        return None

    shell = args.shell or tool("shell")
    example = REPO / "target" / "debug" / "examples" / "serve-test-registry"
    payload = args.payload or (
        REPO / "target" / "tilo-apps-live" / "payloads" / PACKAGE_ID / "bin"
        / "word-processor")
    tools = {}
    for var, name in (("TILO_APP_BIN", "app"),
                      ("TILO_INSTALL_BIN", "tilo-install"),
                      ("TILO_LAUNCHER_BIN", "tilo-launcher")):
        found = tool(name)
        if found:
            tools[var] = str(found)

    missing = []
    if not shell or not shell.exists():
        missing.append("shell")
    if not example.exists():
        missing.append(f"serve-test-registry ({example})")
    if not payload.exists():
        missing.append(f"word-processor payload ({payload})")
    for var in ("TILO_APP_BIN", "TILO_INSTALL_BIN", "TILO_LAUNCHER_BIN"):
        if var not in tools:
            missing.append(var)
    if missing:
        print("FAIL: not built:\n  " + "\n  ".join(missing), file=sys.stderr)
        print("\n  cargo build -p shell -p install -p launcher -p word-processor"
              "\n  cargo build -p app-cli --bins --examples", file=sys.stderr)
        return 2

    # A stale compositor produces a verdict about code that is not under
    # test, and it produces it silently.
    src = REPO / "crates" / "session" / "shell" / "src"
    newest = max((f.stat().st_mtime for f in src.rglob("*.rs")), default=0)
    if shell.stat().st_mtime < newest:
        print(f"REFUSING: {shell} predates crates/session/shell/src. Build first.",
              file=sys.stderr)
        return 2

    runtime_bundles = pathlib.Path.home() / ".local/share/tilo-runtime"
    if not runtime_bundles.exists():
        print(f"FAIL: no runtime bundle store at {runtime_bundles}.",
              file=sys.stderr)
        return 2

    args.out.mkdir(parents=True, exist_ok=True)
    store_config = args.out / "store-config"
    shutil.rmtree(store_config, ignore_errors=True)
    root = args.out / "session"
    shutil.rmtree(root, ignore_errors=True)
    short_root = pathlib.Path(tempfile.mkdtemp(prefix="/tmp/qs-"))

    print(f"shell:   {shell}")
    print(f"payload: {payload}  ({payload.stat().st_size / 1e6:.0f} MB)")

    with Registry(example, payload, store_config, args.out / "registry.log") as reg:
        print(f"registry: {reg.base_url}  (loopback)\n")
        s = Session(root, shell, tools, store_config, runtime_bundles, short_root)
        try:
            step("a session that does not have the word processor")
            if not check("the compositor came up", s.wait_up()):
                print(s.log_path.read_text()[-3000:])
                return 1
            check("its app dir is empty",
                  not (s.apps_dir / "registry.toml").exists())

            step("the palette offers it, in an ordinary row")
            s.modifiers(logo=True)
            s.key("k")
            s.modifiers()
            time.sleep(0.5)
            if not check("the palette opened",
                         s.palette().get("visible") is True):
                return 1
            time.sleep(1.5)
            s.type_text(QUERY)
            time.sleep(0.8)
            pal, row = wait_for_row(s, timeout=60.0)
            s.snapshot(args.out / "01-palette-row.png")

            row_text = json.dumps(row)
            bad = banned_words_in(row_text)
            check("nothing in the row names a mechanism", not bad,
                  f"found {bad} in {row_text[:200]}" if bad else row.get("label", ""))
            if pal.get("selected_id") != row.get("id"):
                raise Failure(
                    f"Enter would activate {pal.get('selected_id')!r}, not "
                    f"{row.get('id')!r}")

            step("Enter — and from here nothing may be silent")
            before = {f.get("surface_id") for f in s.foreign_frames()}
            t0 = time.monotonic()
            s.key("Enter")

            # The very first look, as soon as the op returns.
            first = s.notifications()
            first_dt = time.monotonic() - t0
            check("something is on screen immediately", bool(first),
                  f"{len(first)} notification(s) at +{first_dt * 1000:.0f} ms")
            if first:
                n = first[0]
                shown = f"{n.get('title', '')} {n.get('body', '')}"
                check("it says the app is opening and loading",
                      "load" in shown.lower(),
                      repr(shown))
                bad = banned_words_in(shown)
                check("and it names no mechanism", not bad,
                      f"found {bad} in {shown!r}" if bad else repr(shown))
            s.snapshot(args.out / "02-immediately-after-enter.png")

            step("sampling every moment of the wait")

            def painted() -> bool:
                for f in s.foreign_frames():
                    if f.get("surface_id") in before:
                        continue
                    if (f.get("frame_seq") or 0) > 0:
                        return True
                return False

            gaps: list[float] = []
            samples = 0
            mid_shot_done = False
            t_frame = None
            deadline = time.time() + args.timeout
            while time.time() < deadline:
                # Frames first. The two reads are separate round trips, so
                # asking about notifications first and frames second would
                # report the handover itself as a gap: the compositor
                # retires the indication on the frame that presents the
                # window, which lands between the two questions.
                if painted():
                    t_frame = time.monotonic()
                    break
                notes = s.notifications()
                samples += 1
                if not notes:
                    # Empty — but the window may have arrived in the
                    # microseconds since the frame check. Only a stretch
                    # with nothing on screen *and* nothing painted is a
                    # gap the user would have sat through.
                    if painted():
                        t_frame = time.monotonic()
                        break
                    gaps.append(time.monotonic() - t0)
                elif not mid_shot_done and time.monotonic() - t0 > 2.0:
                    s.snapshot(args.out / "03-still-loading.png")
                    mid_shot_done = True
                time.sleep(SAMPLE_S)

            if t_frame is None:
                raise Failure(
                    f"the app never presented a frame; see {s.log_path}")
            waited = t_frame - t0
            check("the app opened", True, f"first frame at +{waited:.1f}s")
            check("no sample of the wait was unacknowledged", not gaps,
                  f"{samples} samples over {waited:.1f}s"
                  if not gaps else
                  f"silent at +{', +'.join(f'{g:.1f}s' for g in gaps[:6])} "
                  f"({len(gaps)}/{samples} samples)")

            step("and it resolves — the window is the answer, not a toast")
            # The retirement is driven from the frame loop, so give it a
            # few frames rather than reading the same instant twice.
            end = time.time() + 15.0
            while time.time() < end and s.notifications():
                time.sleep(0.2)
            check("the indication is gone once the window is there",
                  not s.notifications(),
                  json.dumps(s.notifications())[:200])
            time.sleep(1.0)
            s.snapshot(args.out / "04-open-and-quiet.png")

            titles = s.tab_titles()
            print(f"  (tabs on screen: {titles})")

            print(f"\nshots in {args.out}")
            for name in sorted(os.listdir(args.out)):
                if name.endswith(".png"):
                    print(f"  {args.out / name}")
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
