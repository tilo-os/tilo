#!/usr/bin/env python3
"""How long it takes to open an app this machine does not have.

Installing an app from the palette is meant to feel effortless. That
is a claim about a duration — so this measures it, on the real install
path, and prints a number that can be checked with a stopwatch.

What is timed
-------------
The whole gesture, from the keystroke that activates the palette row to
the app's own first frame. Not the download, and not the verify: the
thing a person actually waits through.

  t0                Enter is sent to the selected palette row
  → staged          the `.tapp` has been fetched and written to the
                    staging dir  (resolve + fetch)
  → installed       the install root now lists the package, so
                    `tilo-install` has verified the ed25519 publisher
                    signature and the BLAKE3 digest and unpacked it
                    (verify + unpack)
  → first frame     the compositor has taken a buffer from the client
                    (`frame_seq > 0`)  (launch + first paint)

`frame_seq > 0` is the end of the gesture and not "a tab appeared": a
tab shows the compositor's own placeholder until the client presents,
and by eye an app that legitimately starts on a blank page is
indistinguishable from one that has not painted at all.

The boundaries in between are read off the filesystem the real code
writes to — the staged `.tapp` appearing, the install root's
`registry.toml` naming the package — so nothing here needs the
compositor to be instrumented and nothing measures a code path the
film does not take.

Why it is a clean session every run
-----------------------------------
Each run gets an empty `TILO_APPS_DATA_DIR` (so the app is genuinely
absent, not absent-by-assertion) and an empty `TILO_STORE_CACHE_DIR`
(so the `.tapp` is genuinely fetched rather than found already staged).
The runtime bundle every packaged app pins is *not* reset: it is
machine state installed once, like a base image, and it is present on
the machine that gets filmed.

What the network is
-------------------
Loopback. `serve-test-registry` binds `127.0.0.1` and serves a really
packed, really signed `.tapp`. A run against
a registry across the internet is a different measurement and this
script does not pretend otherwise. Every run prints the conditions it
measured under — payload size, filesystem, load average — alongside the
numbers, so a timing and the machine that produced it cannot drift
apart later.

Usage
-----
    scripts/headless-examples/install-open-timing.py --runs 5

    cargo build -p shell -p install -p launcher -p word-processor
    cargo build -p app-cli --bins --examples

Exits non-zero if any run fails to install and paint.
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
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.request

from reaper import die_with_parent, track

REPO = pathlib.Path(__file__).resolve().parents[2]
PACKAGE_ID = "so.tilo.word-processor"
DISPLAY_NAME = "Word Processor"
# What the operator types. The row has to be found by typing, the way a
# person finds it.
QUERY = "word processor"

# Polling intervals. Tight enough that the quantisation is not part of
# the answer: a 1s poll would round every phase to the nearest second,
# which is the difference between "1.2s" and "2s" in a sentence someone
# reads out loud.
FRAME_POLL_S = 0.025
FILE_POLL_S = 0.010

SCRUB = ("DISPLAY", "WAYLAND_DISPLAY", "VK_DRIVER_FILES", "VK_ICD_FILENAMES",
         "__EGL_VENDOR_LIBRARY_FILENAMES", "MESA_LOADER_DRIVER_OVERRIDE",
         "DRI_PRIME", "__GLX_VENDOR_LIBRARY_NAME")


# ---------------------------------------------------------------------
# control socket
# ---------------------------------------------------------------------

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


class Failure(Exception):
    pass


class Session:
    """One compositor, isolated except for the runtime bundle store."""

    def __init__(self, root: pathlib.Path, shell: pathlib.Path, tools: dict,
                 store_config: pathlib.Path, runtime_bundles: pathlib.Path,
                 short_root: pathlib.Path):
        # The session root is on real storage, and that is a
        # measurement decision rather than tidiness. `/tmp` here is a
        # 16 GB tmpfs, so an install root under it unpacks 446 MB into
        # RAM and reports a verify-and-unpack time no disk can match.
        # Session dirs go under `--out`, on the filesystem the repo
        # lives on.
        self.dir = root
        self.dir.mkdir(parents=True, exist_ok=True)
        # Only the sockets go somewhere short: AF_UNIX paths are capped
        # near 108 bytes, and a long root fails the compositor's own
        # bind in a way that looks like a startup hang. They are a few
        # bytes each, so where they live cannot move a timing.
        short_root.mkdir(parents=True, exist_ok=True)
        self.sock = str(short_root / "c.sock")
        self.apps_dir = self.dir / "apps"
        self.store_cache = self.dir / "store-cache"
        self.staging = self.store_cache / "staged-tapp"
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
            # Empty: this is what makes "the machine does not have it"
            # a fact about a directory rather than a claim.
            "TILO_APPS_DATA_DIR": str(self.apps_dir),
            # Machine state a launch cannot do without, and not part of
            # what an install fetches.
            "TILO_RUNTIME_BUNDLE_DIR": str(runtime_bundles),
            "TILO_STORE_CONFIG_DIR": str(store_config),
            # Empty: so the `.tapp` is fetched, not found.
            "TILO_STORE_CACHE_DIR": str(self.store_cache),
            "TILO_HEADLESS_SIZE": "1280x800",
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
                time.sleep(1.0)   # let the first paint land
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


# ---------------------------------------------------------------------
# the local signed registry
# ---------------------------------------------------------------------

class Registry:
    """`serve-test-registry` on loopback, serving one real signed `.tapp`.

    Started once and shared by every run: packing a large payload takes
    the better part of a minute and is set-up, not part of what a user
    waits for. What each run re-does is the part the user waits for —
    the fetch, the verify and the launch.
    """

    def __init__(self, example_bin: pathlib.Path, payload: pathlib.Path,
                 config_dir: pathlib.Path, log_path: pathlib.Path):
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


# ---------------------------------------------------------------------
# one measured run
# ---------------------------------------------------------------------

def find_row(pal: dict) -> dict | None:
    """The row that opens our package, found the way a reader finds it.

    Row ids are positional (`result:0`, …), so the row's own identity is
    in the text — the applications provider puts the package id in the
    subtitle.
    """
    for row in pal.get("rows", []):
        if PACKAGE_ID in json.dumps(row):
            return row
    return None


def wait_for_row(s: Session, timeout: float) -> tuple[dict, dict]:
    """Wait until the palette offers the package.

    The catalogue answers from an in-memory cache and never blocks a
    keystroke on HTTP, so the first query after a cold start
    legitimately has nothing in it and schedules a fetch. Nudging the
    query re-runs the providers against a now-warm cache, which is what
    a person does anyway by continuing to type.

    This wait is deliberately *before* t0: finding the row is the
    palette's latency, not the install's, and folding a cold catalogue
    cache into "how long installing takes" would be measuring the wrong
    thing in the flattering direction as often as not.
    """
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


def settled_size(path: pathlib.Path) -> int | None:
    try:
        return path.stat().st_size
    except FileNotFoundError:
        return None


def measure_run(s: Session, timeout: float, shot: pathlib.Path | None) -> dict:
    """Drive one install-on-open and time each boundary.

    Everything after t0 is polled rather than instrumented, so this
    measures the shipped path exactly as the film runs it.
    """
    # Open the palette in global scope — the only scope that offers an
    # app the machine does not have.
    s.modifiers(logo=True)
    s.key("Space")
    s.modifiers()
    time.sleep(0.5)
    pal = s.palette()
    if pal.get("visible") is not True:
        raise Failure(f"Super+Space did not open the palette: {pal}")
    # Let the cold catalogue cache fill before the name is finished.
    time.sleep(1.5)
    s.type_text(QUERY)
    time.sleep(0.8)
    pal, row = wait_for_row(s, timeout=45.0)
    # Enter takes the *selected* row, so "the row exists" is not the
    # same claim as "Enter installs the word processor".
    if pal.get("selected_id") != row.get("id"):
        raise Failure(f"Enter would activate {pal.get('selected_id')!r}, not "
                      f"{row.get('id')!r} ({row.get('label')!r})")

    before = {f.get("surface_id") for f in s.foreign_frames()}
    registry_toml = s.apps_dir / "registry.toml"

    t0 = time.monotonic()
    s.key("Enter")

    t_staged = None       # the .tapp is on disk, fully written
    t_installed = None    # the install root names the package
    t_frame = None
    frame = None
    staged_bytes = 0
    last_size = None
    stable_since = None

    deadline = time.time() + timeout
    next_frame_poll = 0.0
    while time.time() < deadline:
        now = time.monotonic()

        # --- resolve + fetch: the staged .tapp ------------------------
        if t_staged is None:
            hits = list(s.staging.glob("*.tapp")) if s.staging.exists() else []
            if hits:
                size = settled_size(hits[0])
                if size is not None and size > 0:
                    if size == last_size:
                        # Two consecutive polls at the same size: the
                        # write has landed. `download()` buffers the
                        # whole body before writing, so this is the end
                        # of the fetch and not a pause in it.
                        if stable_since is None:
                            stable_since = now
                        elif now - stable_since >= 0.05:
                            t_staged = stable_since
                            staged_bytes = size
                    else:
                        stable_since = None
                    last_size = size

        # --- verify + unpack: the install root ------------------------
        if t_installed is None and registry_toml.exists():
            try:
                if PACKAGE_ID in registry_toml.read_text(errors="replace"):
                    t_installed = now
                    if t_staged is None:
                        # The staging file was created and removed
                        # between two polls. Rare, and better reported
                        # than silently attributed to another phase.
                        t_staged = now
            except OSError:
                pass

        # --- launch + first paint ------------------------------------
        if t_installed is not None and now >= next_frame_poll:
            next_frame_poll = now + FRAME_POLL_S
            for f in s.foreign_frames():
                if f.get("surface_id") in before:
                    continue
                if (f.get("frame_seq") or 0) > 0:
                    t_frame = time.monotonic()
                    frame = f
                    break
            if t_frame is not None:
                break

        time.sleep(FILE_POLL_S)

    if shot is not None:
        try:
            s.key("Escape")
            time.sleep(0.3)
            s.snapshot(shot)
        except Exception:
            pass

    if t_frame is None:
        raise Failure(
            "the word processor never presented a frame "
            f"(staged={t_staged is not None}, installed={t_installed is not None}); "
            f"see {s.log_path}")

    return {
        "fetch_s": t_staged - t0,
        "verify_unpack_s": t_installed - t_staged,
        "launch_paint_s": t_frame - t_installed,
        "total_s": t_frame - t0,
        "tapp_bytes": staged_bytes,
        "frame_seq": frame.get("frame_seq"),
    }


# ---------------------------------------------------------------------
# the sub-phase breakdown, on the same binaries
# ---------------------------------------------------------------------

def breakdown(tools: dict, store_config: pathlib.Path, base_url: str,
              scratch: pathlib.Path) -> dict:
    """Split the fetch into resolve and transfer, by running the same
    two things `app install` runs internally.

    `install()` resolves against a live `GET /entries` every time — it
    does not reuse the palette's cache — so timing the same call is
    timing the same work, not a proxy for it.
    """
    env = {**os.environ, "TILO_STORE_CONFIG_DIR": str(store_config),
           "TILO_APPS_DATA_DIR": str(scratch / "empty-apps")}
    (scratch / "empty-apps").mkdir(parents=True, exist_ok=True)

    t = time.monotonic()
    out = subprocess.run([tools["TILO_APP_BIN"], "list", "--json"],
                         env=env, capture_output=True, text=True, check=True)
    resolve_s = time.monotonic() - t
    listing = json.loads(out.stdout)
    app = next((a for a in listing["apps"] if a["package_id"] == PACKAGE_ID), None)
    if app is None:
        raise Failure(f"{PACKAGE_ID} not offered: {listing}")

    url = app["download_url"]
    dest = scratch / "probe.tapp"
    t = time.monotonic()
    with urllib.request.urlopen(url) as r:
        data = r.read()
    transfer_s = time.monotonic() - t
    dest.write_bytes(data)

    # The exact argv the catalogue uses — `--force`, and pointedly no
    # `--allow-self-signed`, so this verifies for real.
    apps_dir = scratch / "install-probe"
    shutil.rmtree(apps_dir, ignore_errors=True)
    apps_dir.mkdir(parents=True, exist_ok=True)
    ienv = {**os.environ, "TILO_APPS_DATA_DIR": str(apps_dir)}
    t = time.monotonic()
    res = subprocess.run([tools["TILO_INSTALL_BIN"], "install", str(dest), "--force"],
                         env=ienv, capture_output=True, text=True)
    install_s = time.monotonic() - t
    if res.returncode != 0:
        raise Failure(f"tilo-install refused the package: {res.stderr.strip()}")

    return {"resolve_s": resolve_s, "transfer_s": transfer_s,
            "install_s": install_s, "bytes": len(data)}


# ---------------------------------------------------------------------

def filesystem_of(path: pathlib.Path) -> str:
    """The filesystem `path` lands on, longest mountpoint wins.

    Reported because it moves the answer: unpacking 446 MB onto a
    tmpfs is a RAM write and reads a second faster than a real disk.
    """
    best = ("", "unknown")
    try:
        target = str(path.resolve())
        for line in pathlib.Path("/proc/mounts").read_text().splitlines():
            parts = line.split()
            if len(parts) < 3:
                continue
            mount, fstype = parts[1], parts[2]
            if (target == mount or target.startswith(mount.rstrip("/") + "/")) \
                    and len(mount) > len(best[0]):
                best = (mount, fstype)
    except OSError:
        pass
    return best[1]


def spread(values: list[float]) -> str:
    if len(values) == 1:
        return f"{values[0]:.2f}s"
    return (f"median {statistics.median(values):.2f}s  "
            f"(min {min(values):.2f}, max {max(values):.2f})")


def main() -> int:
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--runs", type=int, default=5,
                   help="clean sessions to measure (default: 5)")
    p.add_argument("--out", type=pathlib.Path,
                   default=REPO / "target" / "install-open-timing",
                   help="where logs and screenshots land")
    p.add_argument("--shell", type=pathlib.Path, default=None,
                   help="compositor binary (default: target/demo/shell, "
                        "falling back to target/debug/shell)")
    p.add_argument("--payload", type=pathlib.Path, default=None,
                   help="the word-processor binary to pack and serve "
                        "(default: the tree scripts/headless-examples/apps-live.sh "
                        "assembles)")
    p.add_argument("--timeout", type=float, default=300.0,
                   help="seconds one install-and-open may take (default: 300)")
    p.add_argument("--keep", action="store_true",
                   help="leave session dirs behind")
    args = p.parse_args()

    # `demo` first, then `debug`: the optimised build is what a person
    # actually waits on.
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

    runtime_bundles = pathlib.Path.home() / ".local/share/tilo-runtime"
    if not runtime_bundles.exists():
        print(f"FAIL: no runtime bundle store at {runtime_bundles}. Every "
              f"packaged app pins one and none ship it; without it a launch "
              f"fails with 'no bundle was found'.", file=sys.stderr)
        return 2

    args.out.mkdir(parents=True, exist_ok=True)
    store_config = args.out / "store-config"
    shutil.rmtree(store_config, ignore_errors=True)
    scratch = args.out / "scratch"
    shutil.rmtree(scratch, ignore_errors=True)
    scratch.mkdir(parents=True, exist_ok=True)

    print(f"shell:    {shell}")
    print(f"payload:  {payload}  ({payload.stat().st_size / 1e6:.0f} MB)")
    for var, path in sorted(tools.items()):
        print(f"{var:22s} {path}")
    print(f"runtime bundles: {runtime_bundles}")
    print(f"install root:    {args.out / 'sessions'}  "
          f"({filesystem_of(args.out)})")
    print(f"load average before the run: "
          f"{', '.join(f'{v:.2f}' for v in os.getloadavg())}")

    rows: list[dict] = []
    with Registry(example, payload, store_config, args.out / "registry.log") as reg:
        print(f"registry: {reg.base_url}  (loopback HTTP, no TLS)\n")

        print("sub-phase breakdown, on the same binaries the compositor spawns")
        try:
            b = breakdown(tools, store_config, reg.base_url, scratch)
        except Exception as e:
            print(f"FAIL: breakdown: {e}", file=sys.stderr)
            return 1
        print(f"  resolve  (GET /entries, verify entry sigs) {b['resolve_s']:.2f}s")
        print(f"  transfer (GET the .tapp, {b['bytes'] / 1e6:.0f} MB)        "
              f"{b['transfer_s']:.2f}s  "
              f"({b['bytes'] / 1e6 / max(b['transfer_s'], 1e-6):.0f} MB/s)")
        print(f"  tilo-install (ed25519 + BLAKE3 + unpack)    {b['install_s']:.2f}s")
        print()

        for i in range(1, args.runs + 1):
            root = args.out / "sessions" / f"run-{i}"
            shutil.rmtree(root, ignore_errors=True)
            short_root = pathlib.Path(tempfile.mkdtemp(prefix="/tmp/qs-"))
            s = Session(root, shell, tools, store_config, runtime_bundles,
                        short_root)
            try:
                if not s.wait_up():
                    print(f"run {i}: compositor did not start; "
                          f"{s.log_path}", file=sys.stderr)
                    return 1
                shot = args.out / f"run-{i}.png" if i == 1 else None
                r = measure_run(s, args.timeout, shot)
                rows.append(r)
                print(f"run {i}:  total {r['total_s']:6.2f}s   "
                      f"fetch {r['fetch_s']:5.2f}  "
                      f"verify+unpack {r['verify_unpack_s']:5.2f}  "
                      f"launch+paint {r['launch_paint_s']:5.2f}   "
                      f"(tapp {r['tapp_bytes'] / 1e6:.0f} MB, "
                      f"frame_seq {r['frame_seq']})")
            except Failure as e:
                print(f"run {i} FAILED: {e}", file=sys.stderr)
                return 1
            finally:
                s.stop()
                shutil.rmtree(short_root, ignore_errors=True)
                if not args.keep:
                    shutil.rmtree(root, ignore_errors=True)
                else:
                    print(f"  session kept at {root}")

    print()
    print(f"{len(rows)} clean sessions, loopback registry, "
          f"{rows[0]['tapp_bytes'] / 1e6:.0f} MB package")
    print(f"  palette row → first frame   {spread([r['total_s'] for r in rows])}")
    print(f"    resolve + fetch           {spread([r['fetch_s'] for r in rows])}")
    print(f"    verify + unpack           {spread([r['verify_unpack_s'] for r in rows])}")
    print(f"    launch + first paint      {spread([r['launch_paint_s'] for r in rows])}")
    if args.out.joinpath("run-1.png").exists():
        print(f"\n  {args.out / 'run-1.png'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
