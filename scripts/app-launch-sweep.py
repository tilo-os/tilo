#!/usr/bin/env python3
"""Launch installed qOS packages through the compositor's real launch
path and report which ones adopt a toplevel.

Drives the whole chain, no mocks:

    headless compositor -> Ctrl+K palette -> Applications provider row
    -> shell::launcher::launch -> qos-launcher -> bwrap -> the app
    -> wl_display_connect -> xdg_toplevel -> tab adoption

Success is read off the compositor's own `state_dump` — a
`TileContent::Foreign` entry in the live WorkspaceTree — not off the
process table. Each app's stderr is captured from the compositor log, so
a failure reports its real error rather than "it didn't work".

**One fresh compositor per app.** These are debug builds whose vello
pipeline compile runs for tens of seconds; stacking dozens of live GPU
clients in one session turns a slow app into a false negative, and any
number measured that way says more about the harness than the product.

`--shots` additionally holds each adopted app until its first frame
lands and writes a PNG. Adoption and first frame are different events:
a snapshot taken between them shows the compositor's "waiting for first
frame" placeholder, not the app.

Examples:

    # Every installed package, product environment, PNG per app.
    scripts/app-launch-sweep.py --out target/sweep --shots

    # Baseline comparison: hand-export the ICD variables the way an
    # operator would have to before this was fixed in the product.
    scripts/app-launch-sweep.py --out target/sweep-pinned --pin

    # One app, quickly.
    scripts/app-launch-sweep.py --out target/one --only calculator
"""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import signal
import socket
import subprocess
import sys
import time

# The manual workaround this harness exists to measure against: an
# operator exporting the loader-selection variables by hand before
# starting the compositor. `--pin` reproduces it for a baseline run.
# The product must never need these.
PIN_ENV = {
    "VK_DRIVER_FILES": "/usr/share/vulkan/icd.d/radeon_icd.x86_64.json",
    "VK_ICD_FILENAMES": "/usr/share/vulkan/icd.d/radeon_icd.x86_64.json",
    "__EGL_VENDOR_LIBRARY_FILENAMES": "/usr/share/glvnd/egl_vendor.d/50_mesa.json",
}

# Scrubbed from every run so a measurement reflects the binaries, not
# whatever the invoking shell happened to carry. Includes the display
# vars: a compositor must not find an outer session to attach to.
SCRUB = list(PIN_ENV) + [
    "MESA_LOADER_DRIVER_OVERRIDE",
    "DRI_PRIME",
    "__GLX_VENDOR_LIBRARY_NAME",
    "DISPLAY",
    "WAYLAND_DISPLAY",
]

ANSI = re.compile(r"\x1b\[[0-9;]*m")

# Lines worth quoting back when an app fails. Deliberately broad: the
# point of a failing row is to carry the app's own words.
ERROR_HINTS = (
    "bwrap:", "error", "panic", "refuse", "failed", "cannot",
    "no such", "warning:", "denied",
)


def call(sock_path: str, payload: dict, timeout: float = 30.0) -> dict:
    """Send one command to the test-control socket, read one reply."""
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


def _walk(node, visit) -> None:
    if isinstance(node, dict):
        visit(node)
        for v in node.values():
            _walk(v, visit)
    elif isinstance(node, list):
        for v in node:
            _walk(v, visit)


def foreign_tiles(state) -> set:
    """`(app_id, surface_id)` for every `TileContent::Foreign` in the tree.

    A Foreign tile is the compositor's own record that it adopted a
    client's xdg_toplevel. Nothing else in the dump proves an app got
    that far.
    """
    found = set()

    def visit(d):
        f = d.get("Foreign")
        if isinstance(f, dict) and "app_id" in f:
            found.add((f["app_id"], f.get("surface_id")))

    _walk(state, visit)
    return found


def installed_packages(data_dir: pathlib.Path) -> list[str]:
    registry = data_dir / "registry.toml"
    if not registry.exists():
        raise SystemExit(f"no package registry at {registry} — install packages first")
    return re.findall(r'^id = "(.*?)"', registry.read_text(), re.M)


def has_gui_entrypoint(data_dir: pathlib.Path, package_id: str) -> bool:
    """Whether the package declares a GUI entrypoint at all.

    A `type = "cli"` / `kind = "service"` package can never adopt a
    toplevel, and the Applications provider never offers it. Counting it
    as a launch failure would be dishonest in both directions — so it is
    reported in its own bucket instead of quietly leaving the
    denominator.
    """
    manifest = data_dir / "payloads" / package_id / "manifest.toml"
    if not manifest.exists():
        return False
    text = manifest.read_text()
    m = re.search(r"^\s*type\s*=\s*\"(\w+)\"", text, re.M)
    return m is not None and m.group(1) in ("gui", "both")


def reap(pattern: str) -> None:
    subprocess.run(["pkill", "-9", "-f", pattern], stderr=subprocess.DEVNULL)


class Compositor:
    """A headless compositor with its test-control socket, as a context
    manager. Guarantees teardown even when a probe raises."""

    def __init__(self, shell_bin, launcher_bin, logpath, env_extra, startup_timeout):
        self.sock = f"/tmp/qos-app-sweep-{os.getpid()}.sock"
        self.shell_bin = shell_bin
        self.launcher_bin = launcher_bin
        self.logpath = logpath
        self.env_extra = env_extra
        self.startup_timeout = startup_timeout
        self.proc = None
        self.log = None
        self.started = False
        self.failure = None

    def __enter__(self):
        try:
            os.unlink(self.sock)
        except FileNotFoundError:
            pass
        env = {**os.environ, "QOS_TEST_CONTROL": self.sock,
               "QOS_LAUNCHER_BIN": str(self.launcher_bin), "RUST_LOG": "info"}
        for k in SCRUB:
            env.pop(k, None)
        env.update(self.env_extra)
        self.log = self.logpath.open("wb")
        self.proc = subprocess.Popen(
            [str(self.shell_bin), "--backend", "headless"],
            env=env, stdout=self.log, stderr=subprocess.STDOUT,
            preexec_fn=os.setsid,
        )
        deadline = time.time() + self.startup_timeout
        while time.time() < deadline:
            if os.path.exists(self.sock):
                self.started = True
                time.sleep(1.0)  # let the first paint land
                return self
            if self.proc.poll() is not None:
                self.failure = (
                    f"compositor exited with status {self.proc.returncode} "
                    f"before binding its control socket"
                )
                return self
            time.sleep(0.1)
        self.failure = (
            f"compositor never bound its control socket within "
            f"{self.startup_timeout:.0f}s (startup did not complete)"
        )
        return self

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

    def open_palette_and_launch(self, short_name: str) -> None:
        """Ctrl+K, type the app's name, activate the top row.

        Exactly what a person does, with nothing in between. This used to
        need two chip clicks first, because Ctrl+K seeded the scope to
        the focused document and that scope never consults the
        Applications provider — the palette offered nothing and Enter did
        nothing. A harness needing a workaround to launch an installed
        app is a harness reporting a product bug; the workaround is gone
        because the bug is.
        """
        call(self.sock, {"op": "modifiers", "ctrl": True, "shift": False,
                         "alt": False, "logo": False})
        call(self.sock, {"op": "key", "key": "k"})
        call(self.sock, {"op": "modifiers", "ctrl": False, "shift": False,
                         "alt": False, "logo": False})
        time.sleep(0.4)
        call(self.sock, {"op": "type", "text": short_name})
        time.sleep(0.8)   # let the providers score and the list settle
        call(self.sock, {"op": "key", "key": "Enter"})

    def wait_for_adoption(self, settle: float):
        deadline = time.time() + settle
        while time.time() < deadline:
            time.sleep(0.5)
            reply = call(self.sock, {"op": "state_dump"})
            tiles = foreign_tiles(reply.get("ok", reply))
            if tiles:
                return sorted(f"{a}#{s}" for a, s in tiles)
        return None

    def foreign_frames(self):
        """Per-surface frame counters the compositor is keeping.

        Returns a list of {surface_id, app_id, frame_seq, last_error},
        or None if the compositor is too old to answer - the caller then
        has nothing better than the picture to go on, and should say so
        rather than guess.
        """
        try:
            reply = call(self.sock, {"op": "foreign_frames"})
        except Exception:  # noqa: BLE001
            return None
        if not isinstance(reply, dict):
            return None
        body = reply.get("foreign_frames", reply.get("ok", reply))
        if isinstance(body, dict):
            body = body.get("frames", body)
        return body if isinstance(body, list) else None

    def snapshot_when_painted(self, out_png: pathlib.Path, hold: float,
                              min_hold: float) -> None:
        """Dismiss the palette, then snapshot repeatedly until the tile's
        content stops changing — i.e. the app's own first frame has
        landed and settled, rather than the placeholder.

        Stability alone is not enough to stop. The compositor's
        "waiting for first frame" placeholder is a static picture, so it
        is *perfectly* stable from the moment it appears — a run that
        returned on two equal sizes would capture the placeholder for
        every app whose shaders are still compiling and report it as the
        app. In a debug build that compile is 10-40s. So `min_hold` has
        to elapse before stability is allowed to end the wait; only
        after that does an unchanging picture mean "this is what the app
        looks like".
        """
        call(self.sock, {"op": "key", "key": "Escape"})
        start = time.time()
        end = start + hold
        last = -1
        while time.time() < end:
            time.sleep(5)
            call(self.sock, {"op": "snapshot", "path": str(out_png)})
            size = out_png.stat().st_size
            if size == last and time.time() - start >= min_hold:
                return
            last = size
        call(self.sock, {"op": "snapshot", "path": str(out_png)})


def app_log_lines(logpath: pathlib.Path, package_id: str) -> list[str]:
    """The app's own output, as the compositor relayed it."""
    text = ANSI.sub("", logpath.read_text(errors="replace"))
    tag = f"[app {package_id}] "
    return [l.split(tag, 1)[-1] for l in text.splitlines() if tag in l]


def was_launched(logpath: pathlib.Path, package_id: str) -> bool:
    """Did the compositor actually fork a launch for this package?

    Distinguishes a real app failure from the palette never having
    offered the row — two very different results that otherwise both
    read as "no tab appeared".
    """
    text = ANSI.sub("", logpath.read_text(errors="replace"))
    return f"shell: launched {package_id}" in text


def load_previous(out: pathlib.Path) -> dict:
    """Records a previous run in this directory left behind.

    A partial `results.json` is normal now that it is written after every
    package, and a corrupt one (killed mid-write) must not abort the
    resume - the worst case is redoing work, which is what would have
    happened anyway.
    """
    path = out / "results.json"
    if not path.exists():
        return {}
    try:
        prior = json.loads(path.read_text())
    except (json.JSONDecodeError, OSError) as e:
        print(f"note: ignoring unreadable {path} ({e}); starting fresh",
              file=sys.stderr)
        return {}
    return prior if isinstance(prior, dict) else {}


def adopt_orphan_capture(png: pathlib.Path) -> dict:
    """A record for a capture found on disk with no results.json entry.

    `frames` is None, not a guess: the compositor that could have
    answered for it is long gone, so anything downstream has to fall
    back to reading the picture and say that it did.
    """
    return {
        "adopted": ["(recovered from an earlier run)"],
        "gui": True,
        "compositor_started": True,
        "launched": True,
        "errors": [],
        "png": str(png),
        "frames": None,
        "recovered": True,
    }


def is_complete(record: dict, want_shots: bool) -> bool:
    """Is this record finished enough that redoing it would prove nothing?

    A package with no GUI entrypoint is settled. Otherwise the run has to
    have got as far as adopting a window - a previous NO-TAB is worth
    retrying, since that is usually what the caller just fixed. With
    --shots the capture must also still be on disk.
    """
    if record.get("gui") is False:
        return True
    if not record.get("adopted"):
        return False
    if want_shots:
        png = record.get("png")
        return bool(png) and pathlib.Path(png).exists()
    return True


def write_results(out: pathlib.Path, results: dict) -> None:
    """Write `results.json` so an interrupted run leaves a usable file.

    Via a temporary file and an atomic rename: a kill during the write
    would otherwise truncate the record of every package that already
    ran, which is exactly the loss this is here to prevent.
    """
    path = out / "results.json"
    tmp = path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(results, indent=2))
    tmp.replace(path)


def run_one(args, package_id: str, out: pathlib.Path, env_extra: dict) -> dict:
    logpath = out / f"{package_id}.log"
    short = package_id.rsplit(".", 1)[-1]
    adopted, shot, frames = None, None, None
    with Compositor(args.shell, args.launcher, logpath, env_extra,
                    args.startup_timeout) as comp:
        if comp.failure:
            return {"adopted": None, "errors": [comp.failure],
                    "compositor_started": False, "launched": False,
                    "png": None, "frames": None}
        comp.open_palette_and_launch(short)
        adopted = comp.wait_for_adoption(args.settle)
        if adopted and args.shots:
            shot = out / f"{package_id}.png"
            comp.snapshot_when_painted(shot, args.hold, args.min_hold)
        if adopted:
            # Ground truth, straight from the compositor's own frame
            # bookkeeping: frame_seq counts frames actually taken from
            # the client, so 0 means the tile is still the first-frame
            # placeholder however much it may look like an app that
            # legitimately starts empty. Read it while the compositor is
            # still up - it dies with the `with` block.
            frames = comp.foreign_frames()
    reap(f"qos-apps/{package_id}")

    launched = was_launched(logpath, package_id)
    lines = app_log_lines(logpath, package_id)
    errors = [l for l in lines if any(h in l.lower() for h in ERROR_HINTS)]
    if not launched:
        errors = ["the palette never activated a row for this package — "
                  "no launch was forked (harness or search-provider issue, "
                  "not an app failure)"] + errors
    return {
        "adopted": adopted,
        "errors": errors or lines[-3:],
        "compositor_started": True,
        "launched": launched,
        "png": str(shot) if shot and shot.exists() else None,
        "frames": frames,
    }


def main() -> int:
    here = pathlib.Path(__file__).resolve().parent.parent
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--worktree", type=pathlib.Path, default=here,
                   help="repo root holding target/debug (default: this script's repo)")
    p.add_argument("--out", type=pathlib.Path, required=True,
                   help="directory for per-app logs, PNGs and results.json")
    p.add_argument("--only", default=None,
                   help="comma-separated substrings; only matching packages run")
    p.add_argument("--pin", action="store_true",
                   help="export the manual ICD variables (baseline comparison)")
    p.add_argument("--shots", action="store_true",
                   help="hold each adopted app for its first frame and write a PNG")
    p.add_argument("--settle", type=float, default=60.0,
                   help="seconds to wait for a toplevel (default: 60)")
    p.add_argument("--hold", type=float, default=60.0,
                   help="seconds to wait for the first frame with --shots")
    p.add_argument("--min-hold", type=float, default=45.0,
                   help="seconds a --shots capture must wait before an "
                        "unchanging picture is accepted as the app's own "
                        "(the first-frame placeholder is static, so it looks "
                        "stable immediately)")
    p.add_argument("--force-recapture", action="store_true",
                   help="re-run packages that already have a capture in "
                        "--out instead of resuming past them")
    p.add_argument("--startup-timeout", type=float, default=45.0,
                   help="seconds to wait for the control socket (default: 45)")
    p.add_argument("--data-dir", type=pathlib.Path,
                   default=pathlib.Path.home() / ".local/share/qos-apps",
                   help="installed-package root")
    args = p.parse_args()

    args.shell = args.worktree / "target/debug/shell"
    args.launcher = args.worktree / "target/debug/qos-launcher"
    for b in (args.shell, args.launcher):
        if not b.exists():
            print(f"FAIL: {b} is not built", file=sys.stderr)
            return 1
    args.out.mkdir(parents=True, exist_ok=True)

    packages = installed_packages(args.data_dir)
    if args.only:
        needles = args.only.split(",")
        packages = [p_ for p_ in packages if any(n in p_ for n in needles)]
    if not packages:
        print("FAIL: no packages matched", file=sys.stderr)
        return 1

    env_extra = dict(PIN_ENV) if args.pin else {}
    print(f"packages: {len(packages)}   manual-ICD-pin: {args.pin}   "
          f"settle: {args.settle:.0f}s", flush=True)
    if results:
        print(f"resuming: {len(results)} prior record(s) in {args.out}",
              flush=True)
    print(flush=True)

    # A run costs upwards of an hour, so an interruption must not throw
    # away what it already proved. Records from a previous run in this
    # directory are adopted, and packages that already have a capture are
    # skipped unless the caller explicitly asks for a redo.
    results = load_previous(args.out)
    resumed = 0
    for package_id in packages:
        if not args.force_recapture:
            prior = results.get(package_id)
            if prior is None and args.shots:
                # A capture on disk with no record: what an older run
                # left behind, since it only wrote results.json at the
                # end. The PNG is the evidence; reconstruct enough of a
                # record to keep it, rather than spending an hour
                # redoing work that plainly succeeded.
                png = args.out / f"{package_id}.png"
                if png.exists():
                    prior = adopt_orphan_capture(png)
                    results[package_id] = prior
            if prior is not None and is_complete(prior, args.shots):
                resumed += 1
                continue
        gui = has_gui_entrypoint(args.data_dir, package_id)
        if not gui:
            results[package_id] = {
                "adopted": None, "gui": False, "compositor_started": None,
                "launched": False,
                "errors": ["package declares no GUI entrypoint "
                           "(type != gui/both); the Applications provider "
                           "never offers it, so nothing is ever launched"],
                "png": None,
            }
            print(f"NO-GUI   {package_id:<32} no graphical entrypoint — not launched",
                  flush=True)
            write_results(args.out, results)
            continue
        r = run_one(args, package_id, args.out, env_extra)
        r["gui"] = True
        results[package_id] = r
        if r["adopted"]:
            detail = ", ".join(r["adopted"])
            status = "ADOPTED"
        else:
            status = "NO-TAB "
            detail = r["errors"][-1][:130] if r["errors"] else "(no output)"
        print(f"{status}  {package_id:<32} {detail}", flush=True)
        # Persist after every package, not once at the end: a killed run
        # then leaves a record that is usable and resumable rather than
        # nothing at all.
        write_results(args.out, results)

    if resumed:
        print(f"\nresumed past {resumed} package(s) already captured in "
              f"{args.out} (--force-recapture to redo them)", flush=True)

    gui_pkgs = [k for k, v in results.items() if v.get("gui")]
    adopted = [k for k, v in results.items() if v.get("adopted")]
    no_gui = [k for k, v in results.items() if not v.get("gui")]
    stillborn = [k for k, v in results.items()
                 if v.get("gui") and v.get("compositor_started") is False]

    print()
    print(f"adopted a toplevel: {len(adopted)}/{len(results)} installed "
          f"({len(adopted)}/{len(gui_pkgs)} of the GUI packages)")
    if no_gui:
        print(f"no GUI entrypoint, never launched: {len(no_gui)} "
              f"({', '.join(sorted(no_gui))})")
    if stillborn:
        print(f"compositor never started for: {len(stillborn)} package(s)")
    failed = [k for k in gui_pkgs if k not in adopted]
    if failed:
        print("failed: " + ", ".join(sorted(failed)))
    write_results(args.out, results)
    print(f"\nper-app logs + results.json in {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
