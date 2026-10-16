#!/usr/bin/env python3
"""Measure what the desktop costs the machine, idle and in use.

`frame-loop-bench.py` answers "does the loop hold its cadence?". This answers
the laptop question — what does sitting there, and then typing, scrolling and
opening the palette, cost in CPU, wakeups, memory and GPU time — for the
compositor *and every process it started*, on the production binary.

One run is a fixed sequence of phases against one disposable headless session:

    idle-empty      nothing open, nobody touching anything
    idle-apps       the same after --apps launched and settled
    pointer         60 Hz pointer motion over the desktop
    palette         open Search, type a query, dismiss — repeatedly
    quick-settings  open/close Quick Settings
    windows         Super+Left/Right between the open windows
    type            typing into the focused app at ~15 characters/s
    scroll          wheel-scrolling the focused app

Per phase, per process group (shell, each app, sandbox plumbing):

    cpu%        utime+stime over the phase, as % of one core
    wakeups/s   voluntary+involuntary context switches of every thread — the
                number a battery cares about, and one a low CPU% hides
    threads     at phase end
    rss / pss   at phase end. PSS divides shared pages (the runtime dylib,
                fonts) among the processes mapping them; RSS double-counts.
    gpu ms/s    `drm-engine-*` time from /proc/PID/fdinfo

and, from the compositor's own frame trace (`TILO_FRAME_TRACE`), loop ticks/s,
painted frames/s, work/scene p50/p95/p99 and — where the build records it —
what woke each tick. The compositor's two-second paint-stats line
(`TILO_PAINT_STATS`) stays in the log so an idle phase that paints names its
cause.

The run exits 1 if any process in the session holds a GPU it should not —
on a hybrid laptop, the discrete GPU woken for nothing.

Examples
--------
    # The compositor must honour TILO_TEST_CONTROL, which only a
    # dev-overrides build does (the installed /usr/bin/tilo-shell ignores it):
    cargo build --release -p shell --features dev-overrides --bin shell --bin tilo-launcher

    # Baseline on main, then the change, then compare. Apps come from the
    # installed store unless --apps-data-dir/--runtime-bundle-dir say otherwise.
    scripts/perf/resource-bench.py --label baseline
    scripts/perf/resource-bench.py --label after
    scripts/perf/resource-bench.py --compare target/resource-bench/baseline.json \\
                                            target/resource-bench/after.json

Nothing here touches the operator's session: the compositor is headless, owns
a private XDG tree under a temporary directory, never blanks, locks or
suspends (TILO_IDLE_*_SECS=0), and its whole process group is killed on exit.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import re
import signal
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
CLK_TCK = os.sysconf("SC_CLK_TCK")

# The target laptop: a 1080p panel beside a 5K display at 2x, both 144 Hz.
DEFAULT_OUTPUTS = "1920x1080~144,5120x2880@2~144+1920+0"
# Not the terminal: it needs the session's `ush` service, which a headless
# session does not run.
DEFAULT_APPS = "so.tilo.notes,so.tilo.file-explorer,so.tilo.calculator,so.tilo.settings,so.tilo.chat"
PHASES = ["idle-empty", "idle-apps", "pointer", "palette", "quick-settings", "windows", "type", "scroll"]
TOTAL_KEYS = ("procs", "cpu_pct", "wakeups_s", "threads", "rss_mb", "pss_mb", "private_mb", "gpu_ms_s",
              "pss_estimated")


# --------------------------------------------------------------------------
# /proc sampling
# --------------------------------------------------------------------------


def read(path: str) -> str | None:
    try:
        with open(path, "rb") as fh:
            return fh.read().decode(errors="replace")
    except OSError:
        return None


def stat_fields(pid: int) -> list[str] | None:
    """`[comm, state, ppid, ...]`: /proc/PID/stat with comm unwrapped."""
    raw = read(f"/proc/{pid}/stat")
    if raw is None:
        return None
    head, _, tail = raw.rpartition(") ")
    return [head.split(" (", 1)[-1]] + tail.split()


def descendants(root: int) -> list[int]:
    children: dict[int, list[int]] = {}
    for entry in os.listdir("/proc"):
        if entry.isdigit() and (fields := stat_fields(int(entry))):
            children.setdefault(int(fields[2]), []).append(int(entry))
    out, todo = [], [root]
    while todo:
        pid = todo.pop()
        out.append(pid)
        todo.extend(children.get(pid, []))
    return out


def ctx_switches(pid: int) -> int:
    total = 0
    try:
        tasks = os.listdir(f"/proc/{pid}/task")
    except OSError:
        return 0
    for tid in tasks:
        for line in (read(f"/proc/{pid}/task/{tid}/status") or "").splitlines():
            if line.startswith(("voluntary_ctxt_switches", "nonvoluntary_ctxt_switches")):
                total += int(line.split()[1])
    return total


def memory(pid: int) -> dict:
    """RSS/PSS/private from smaps_rollup; for a non-dumpable process (the shell)
    that file is unreadable, so fall back to /proc/PID/status: RSS stands in as
    an upper bound for PSS and RssAnon for private, and `pss_estimated` says so."""
    out = {"rss_kb": 0, "pss_kb": 0, "private_kb": 0, "pss_estimated": 0}
    rollup = read(f"/proc/{pid}/smaps_rollup")
    if not rollup:
        status = {}
        for line in (read(f"/proc/{pid}/status") or "").splitlines():
            key, _, value = line.partition(":")
            if value.split() and value.split()[0].isdigit():
                status[key] = int(value.split()[0])
        rss = status.get("VmRSS", 0)
        return {"rss_kb": rss, "pss_kb": rss, "private_kb": status.get("RssAnon", 0),
                "pss_estimated": 1 if rss else 0}
    for line in rollup.splitlines():
        parts = line.split()
        if len(parts) < 2 or not parts[1].isdigit():
            continue
        key, value = parts[0].rstrip(":"), int(parts[1])
        if key == "Rss":
            out["rss_kb"] = value
        elif key == "Pss":
            out["pss_kb"] = value
        elif key in ("Private_Clean", "Private_Dirty"):
            out["private_kb"] += value
    return out


def gpu_drivers() -> dict[str, str]:
    """PCI address -> kernel driver, for every DRM render node."""
    out = {}
    for node in Path("/sys/class/drm").glob("renderD*"):
        try:
            out[(node / "device").resolve().name] = (node / "device" / "driver").resolve().name
        except OSError:
            continue
    return out


def gpu_usage(pid: int) -> tuple[dict[str, int], set[str]]:
    """Engine nanoseconds per DRM client, and every GPU the process holds open."""
    engines: dict[str, int] = {}
    devices: set[str] = set()
    try:
        fds = os.listdir(f"/proc/{pid}/fd")
    except OSError:
        return {}, set()
    for fd in fds:
        try:
            target = os.readlink(f"/proc/{pid}/fd/{fd}")
        except OSError:
            continue
        if target.startswith("/dev/nvidia"):
            devices.add("nvidia")
            continue
        if not target.startswith("/dev/dri/"):
            continue
        pdev = client = None
        ns = 0
        for line in (read(f"/proc/{pid}/fdinfo/{fd}") or "").splitlines():
            if line.startswith("drm-pdev:"):
                pdev = line.split()[1]
            elif line.startswith("drm-client-id:"):
                client = line.split()[1]
            elif line.startswith("drm-engine-") and line.rstrip().endswith(" ns"):
                ns += int(line.split()[1])
        devices.add(pdev or target)
        if pdev and client:
            key = f"{pdev}#{client}"
            engines[key] = max(engines.get(key, 0), ns)
    return engines, devices


def group_of(pid: int, comm: str, shell_pid: int) -> str:
    if pid == shell_pid:
        return "shell"
    argv0 = (read(f"/proc/{pid}/cmdline") or "").split("\0")[0]
    name = os.path.basename(argv0) or comm
    if name in ("bwrap", "tilo-launcher", "xdg-dbus-proxy"):
        return "sandbox-plumbing"
    return name


class Snapshot:
    def __init__(self, shell_pid: int):
        self.at = time.monotonic()
        self.procs: dict[int, dict] = {}
        for pid in descendants(shell_pid):
            fields = stat_fields(pid)
            if fields is None:
                continue
            gpu, devices = gpu_usage(pid)
            self.procs[pid] = {
                "group": group_of(pid, fields[0], shell_pid),
                # utime/stime are stat fields 14/15; fields[0] is field 2.
                "cpu_ticks": int(fields[12]) + int(fields[13]),
                "threads": int(fields[18]),
                "ctx": ctx_switches(pid),
                "gpu": gpu,
                "devices": sorted(devices),
                **memory(pid),
            }


def diff(a: Snapshot, b: Snapshot) -> dict:
    seconds = max(b.at - a.at, 1e-6)
    groups: dict[str, dict] = {}
    for pid, now in b.procs.items():
        g = groups.setdefault(now["group"], dict.fromkeys(TOTAL_KEYS, 0))
        g["procs"] += 1
        g["threads"] += now["threads"]
        g["rss_mb"] += now["rss_kb"] / 1024
        g["pss_mb"] += now["pss_kb"] / 1024
        g["private_mb"] += now["private_kb"] / 1024
        g["pss_estimated"] += now["pss_estimated"]
        before = a.procs.get(pid)
        if before is None:
            continue
        g["cpu_pct"] += (now["cpu_ticks"] - before["cpu_ticks"]) / CLK_TCK / seconds * 100
        g["wakeups_s"] += max(0, now["ctx"] - before["ctx"]) / seconds
        for key, ns in now["gpu"].items():
            g["gpu_ms_s"] += max(0, ns - before["gpu"].get(key, ns)) / 1e6 / seconds
    total = {k: sum(g[k] for g in groups.values()) for k in TOTAL_KEYS}
    return {"seconds": seconds, "groups": groups, "total": total}


# --------------------------------------------------------------------------
# frame trace
# --------------------------------------------------------------------------


def read_trace(path: Path) -> list[dict]:
    rows: list[dict] = []
    required = {"start_us", "period_us", "sleep_us", "scene_us", "submit_us", "testctl_us", "painted"}
    with path.open() as fh:
        header = fh.readline().strip().split(",")
        if len(set(header)) != len(header) or not required <= set(header):
            raise ValueError(f"{path}: missing or duplicate frame trace columns")
        for line_number, line in enumerate(fh, 2):
            parts = line.strip().split(",")
            if len(parts) != len(header):
                raise ValueError(f"{path}:{line_number}: truncated frame trace row")
            row = {k: int(v) if v.isdigit() else v for k, v in zip(header, parts)}
            if (any(type(row[k]) is not int for k in required)
                    or row["painted"] not in (0, 1)):
                raise ValueError(f"{path}:{line_number}: invalid frame trace measurement")
            rows.append(row)
    return rows


def pct(values: list[int], p: int) -> int:
    """Nearest-rank percentile, matching `metrics::percentiles`."""
    if not values:
        return 0
    v = sorted(values)
    rank = max(1, -(-p * len(v) // 100))
    return v[min(rank, len(v)) - 1]


def frame_stats(rows: list[dict], lo_us: int, hi_us: int) -> dict:
    window = [r for r in rows if lo_us <= r["start_us"] < hi_us]
    seconds = max((hi_us - lo_us) / 1e6, 1e-6)
    painted = [r for r in window if r.get("painted")]
    work = [max(0, b["period_us"] - a["sleep_us"]) for a, b in zip(window, window[1:]) if b["period_us"]]
    wakes: dict[str, int] = {}
    for r in window:
        if isinstance(r.get("wake"), str) and r["wake"]:
            wakes[r["wake"]] = wakes.get(r["wake"], 0) + 1
    return {
        "ticks_s": len(window) / seconds,
        "painted_s": len(painted) / seconds,
        "work_us": {str(p): pct(work, p) for p in (50, 95, 99)},
        "scene_us": {str(p): pct([r["scene_us"] for r in painted], p) for p in (50, 95, 99)},
        "submit_us": {str(p): pct([r["submit_us"] for r in painted], p) for p in (50, 95, 99)},
        "wakes": dict(sorted(wakes.items(), key=lambda kv: -kv[1])),
    }


# --------------------------------------------------------------------------
# driving the session
# --------------------------------------------------------------------------


class ControlError(RuntimeError):
    """The compositor rejected a command; retain its diagnostic for callers."""


class Control:
    def __init__(self, path: Path):
        self.path = path

    def send(self, obj: dict, timeout: float = 30.0) -> dict:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.settimeout(timeout)
            s.connect(str(self.path))
            s.sendall((json.dumps(obj) + "\n").encode())
            buf = b""
            while not buf.endswith(b"\n"):
                chunk = s.recv(1 << 16)
                if not chunk:
                    break
                buf += chunk
        if not buf.endswith(b"\n"):
            raise RuntimeError(f"{obj['op']}: control connection closed without a complete response")
        reply = json.loads(buf)
        if not isinstance(reply, dict) or not reply:
            raise RuntimeError(f"{obj['op']}: invalid control response: {reply!r}")
        if "err" in reply:
            raise ControlError(f"{obj['op']}: {reply['err']}")
        return reply

    def mods(self, logo: bool = False) -> None:
        self.send({"op": "modifiers", "ctrl": False, "shift": False, "alt": False, "logo": logo})

    def super_key(self, key: str) -> None:
        self.mods(logo=True)
        self.send({"op": "key", "key": key})
        self.mods()


def foreign_frames(ctl: Control) -> list[dict]:
    reply = ctl.send({"op": "foreign_frames"})
    body = reply.get("foreign_frames", reply)
    if isinstance(body, dict):
        body = body.get("frames", body)
    if not isinstance(body, list):
        raise RuntimeError(f"foreign_frames: invalid control response: {reply!r}")
    return body


def max_refresh_hz(outputs: str) -> float:
    rates = [float(m) for m in re.findall(r"~([0-9.]+)", outputs)]
    return max(rates, default=60.0)


def launch(ctl: Control, app_id: str, timeout: float) -> tuple[bool, float]:
    started = time.monotonic()
    deadline = started + timeout
    before = {f.get("surface_id") for f in foreign_frames(ctl)}
    invoked = False
    while not invoked and time.monotonic() < deadline:
        try:
            ctl.send({"op": "actions_invoke", "provider": "applications", "id": app_id})
            invoked = True
        except ControlError:
            time.sleep(0.5)  # the application index is built off-thread
    while invoked and time.monotonic() < deadline:
        for frame in foreign_frames(ctl):
            if (frame.get("surface_id") not in before and frame.get("app_id") == app_id
                    and frame.get("frame_seq", 0) > 0):
                return True, time.monotonic() - started
        time.sleep(0.2)
    return False, time.monotonic() - started


def drive(ctl: Control, phase: str, seconds: float, size: tuple[int, int]) -> None:
    if phase.startswith("idle"):
        time.sleep(seconds)
        return
    end = time.monotonic() + seconds
    w, h = size
    n = 0
    while time.monotonic() < end:
        if phase == "pointer":
            ctl.send({"op": "pointer_move", "x": w * (0.2 + 0.6 * (n % 120) / 120), "y": h * 0.5})
            time.sleep(1 / 60)
        elif phase == "palette":
            ctl.super_key("Space")
            ctl.send({"op": "type", "text": "calc"})
            time.sleep(0.4)
            ctl.send({"op": "key", "key": "Escape"})
            time.sleep(0.4)
        elif phase == "quick-settings":
            ctl.super_key("q")
            time.sleep(0.6)
            ctl.super_key("q")
            time.sleep(0.6)
        elif phase == "windows":
            ctl.super_key("ArrowLeft" if (n // 3) % 2 else "ArrowRight")
            time.sleep(0.35)
        elif phase == "type":
            ctl.send({"op": "type", "text": "the quick brown fox "[n % 20]})
            time.sleep(1 / 15)
        elif phase == "scroll":
            ctl.send({"op": "pointer_move", "x": w * 0.5, "y": h * 0.5})
            ctl.send({"op": "scroll", "dx": 0.0, "dy": 3.0 if (n // 40) % 2 else -3.0})
            time.sleep(1 / 30)
        n += 1


def first_output_size(outputs: str) -> tuple[int, int]:
    mode = outputs.split(",")[0]
    for sep in "@~+r":
        mode = mode.split(sep)[0]
    w, h = mode.split("x")
    return int(w), int(h)


def kill_group(proc: subprocess.Popen) -> None:
    for sig, wait in ((signal.SIGTERM, 10), (signal.SIGKILL, 5)):
        try:
            os.killpg(proc.pid, sig)
            proc.wait(timeout=wait)
            return
        except ProcessLookupError:
            return
        except subprocess.TimeoutExpired:
            continue


def run(args: argparse.Namespace) -> dict:
    shell = Path(args.shell_bin)
    if not shell.exists():
        sys.exit(f"no shell binary at {shell}; build it: cargo build --release -p shell --bin shell")
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    trace, log = out / f"{args.label}.csv", out / f"{args.label}.log"
    # Short root: the control socket path must fit in sun_path.
    root = Path(tempfile.mkdtemp(prefix="tilo-rb-"))
    run_dir = root / "run"
    run_dir.mkdir(mode=0o700)
    ctl_path = run_dir / "ctl.sock"
    env = {k: v for k, v in os.environ.items() if k not in ("WAYLAND_DISPLAY", "DISPLAY")}
    env.update({
        "HOME": str(root / "home"),
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_DATA_HOME": str(root / "data"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "XDG_STATE_HOME": str(root / "state"),
        "XDG_RUNTIME_DIR": str(run_dir),
        "TILO_STATE_PATH": str(root / "config" / "state.json"),
        "TILO_IDENTITY_ROOT": str(root / "identity"),
        "TILO_PEER_KEY_PATH": str(root / "keys" / "peer-key"),
        "TILO_TEST_CONTROL": str(ctl_path),
        "TILO_FRAME_TRACE": str(trace),
        "TILO_FRAME_WARMUP_MS": "0",
        "TILO_PAINT_STATS": "1",
        "TILO_HEADLESS_OUTPUTS": args.outputs,
        # Headless reports no refresh rate; pace like the fastest display.
        "TILO_REFRESH_HZ": str(max_refresh_hz(args.outputs)),
        # A benchmark session must never act on the host's power state.
        "TILO_IDLE_BLANK_SECS": "0",
        "TILO_IDLE_LOCK_SECS": "0",
        "TILO_IDLE_SUSPEND_SECS": "0",
    })
    if args.watch:
        # Rasterize every painted frame, as a real display would.
        env["TILO_HEADLESS_WATCH"] = "1"
    for key, value in (("TILO_APPS_DATA_DIR", args.apps_data_dir),
                       ("TILO_RUNTIME_BUNDLE_DIR", args.runtime_bundle_dir),
                       ("TILO_LAUNCHER_BIN", args.launcher_bin)):
        if value:
            env[key] = value
    subprocess.run([str(shell), "--provision-identity", "--disable-peer-sync"], env=env,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=120, check=False)

    drivers = gpu_drivers()
    allowed = set(args.allowed_gpu.split(",")) if args.allowed_gpu else {
        pdev for pdev, driver in drivers.items() if driver != "nvidia"}
    result: dict = {"label": args.label, "shell": str(shell), "outputs": args.outputs,
                    "watch": args.watch, "gpus": drivers, "apps": [], "phases": {}, "violations": []}
    windows: dict[str, tuple[float, float]] = {}
    first_request_at = None
    with log.open("wb") as logfh:
        proc = subprocess.Popen(
            [str(shell), "--mode", "native", "--backend", "headless", "--disable-peer-sync"],
            env=env, stdout=logfh, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            deadline = time.monotonic() + args.startup_timeout
            while not ctl_path.exists():
                if proc.poll() is not None or time.monotonic() > deadline:
                    raise SystemExit(f"compositor did not come up (a build without "
                                     f"dev-overrides never opens the control socket); see {log}")
                time.sleep(0.1)
            ctl = Control(ctl_path)
            # The first tick that services the socket anchors the trace's clock to ours.
            first_request_at = time.monotonic()
            ctl.send({"op": "list_targets"})
            time.sleep(args.settle)
            size = first_output_size(args.outputs)

            def phase(name: str) -> None:
                print(f"[resource-bench] {name} ({args.seconds:.0f}s)", flush=True)
                a = Snapshot(proc.pid)
                t0 = time.monotonic()
                drive(ctl, name, args.seconds, size)
                windows[name] = (t0, time.monotonic())
                b = Snapshot(proc.pid)
                result["phases"][name] = diff(a, b)
                for pid, p in b.procs.items():
                    bad = [d for d in p["devices"] if d == "nvidia" or (":" in d and d not in allowed)]
                    if bad:
                        result["violations"].append(f"{name}: {p['group']} (pid {pid}) holds GPU {bad}")

            phase("idle-empty")
            apps = list(filter(None, args.apps.split(",")))
            if apps:
                # The application index is built by the palette's first open,
                # not by the action API; open it once as a person would.
                ctl.super_key("Space")
                time.sleep(2)
                ctl.send({"op": "key", "key": "Escape"})
            for app in apps:
                ok, took = launch(ctl, app, args.app_timeout)
                print(f"[resource-bench] launch {app}: {'painted' if ok else 'FAILED'} in {took:.1f}s",
                      flush=True)
                result["apps"].append({"id": app, "painted": ok, "seconds": round(took, 2)})
                if not ok:
                    result["violations"].append(f"{app}: no painted frame within {args.app_timeout}s")
            time.sleep(args.settle)
            for name in PHASES[1:]:
                if name not in args.skip.split(","):
                    phase(name)
        finally:
            kill_group(proc)
            shutil.rmtree(root, ignore_errors=True)

    rows = read_trace(trace)
    marker = next((r for r in rows if r.get("testctl_us", 0) > 0), None)
    if marker is not None and first_request_at is not None:
        for name, (t0, t1) in windows.items():
            lo = marker["start_us"] + int((t0 - first_request_at) * 1e6)
            hi = marker["start_us"] + int((t1 - first_request_at) * 1e6)
            result["phases"][name]["frames"] = frame_stats(rows, lo, hi)
    else:
        result["violations"].append(f"no frame trace to read at {trace}")
    return result


# --------------------------------------------------------------------------
# reporting
# --------------------------------------------------------------------------


def print_report(r: dict) -> None:
    print(f"\n=== {r['label']}  ({r['shell']}, outputs {r['outputs']}, watch={r['watch']}) ===")
    for app in r["apps"]:
        print(f"  launch {app['id']:<28} {'ok' if app['painted'] else 'FAILED':<7} {app['seconds']:.1f}s")
    print(f"\n  {'phase':<15}{'cpu%':>8}{'wake/s':>9}{'thr':>6}{'rss MB':>9}{'pss MB':>9}"
          f"{'gpu ms/s':>10}{'ticks/s':>9}{'paint/s':>9}   work p50/p95/p99 ms")
    for name, e in r["phases"].items():
        t, f = e["total"], e.get("frames", {})
        w = f.get("work_us", {})
        print(f"  {name:<15}{t['cpu_pct']:8.2f}{t['wakeups_s']:9.1f}{t['threads']:6d}{t['rss_mb']:9.0f}"
              f"{t['pss_mb']:9.0f}{t['gpu_ms_s']:10.2f}{f.get('ticks_s', 0):9.1f}{f.get('painted_s', 0):9.1f}"
              f"   {w.get('50', 0) / 1000:.2f}/{w.get('95', 0) / 1000:.2f}/{w.get('99', 0) / 1000:.2f}")
    if r["phases"]:
        name, last = list(r["phases"].items())[-1]
        print(f"\n  per process group at the end of {name}:")
        print(f"  {'group':<28}{'n':>3}{'cpu%':>8}{'wake/s':>9}{'thr':>6}{'rss MB':>9}{'pss MB':>9}{'priv MB':>9}")
        for g, v in sorted(last["groups"].items(), key=lambda kv: -kv[1]["pss_mb"]):
            print(f"  {g:<28}{v['procs']:>3}{v['cpu_pct']:8.2f}{v['wakeups_s']:9.1f}{v['threads']:6d}"
                  f"{v['rss_mb']:9.0f}{v['pss_mb']:9.0f}{v['private_mb']:9.0f}")
    for name in ("idle-empty", "idle-apps"):
        if e := r["phases"].get(name):
            print(f"\n  {name}, by wakeups:")
            for g, v in sorted(e["groups"].items(), key=lambda kv: -kv[1]["wakeups_s"]):
                print(f"    {g:<28} cpu {v['cpu_pct']:6.2f}%  wake {v['wakeups_s']:7.1f}/s")
            if wakes := e.get("frames", {}).get("wakes"):
                print(f"    compositor wake reasons: {wakes}")
    for v in r["violations"]:
        print(f"  VIOLATION: {v}")


def compare(a: dict, b: dict) -> None:
    print(f"\n=== {a['label']} -> {b['label']} ===")
    print(f"  {'phase':<15}{'cpu%':>17}{'wake/s':>19}{'pss MB':>15}{'paint/s':>15}{'work p99 ms':>17}")
    for name in PHASES:
        ea, eb = a["phases"].get(name), b["phases"].get(name)
        if not ea or not eb:
            continue
        ta, tb = ea["total"], eb["total"]
        fa, fb = ea.get("frames", {}), eb.get("frames", {})
        print(f"  {name:<15}{ta['cpu_pct']:8.2f} ->{tb['cpu_pct']:7.2f}"
              f"{ta['wakeups_s']:9.1f} ->{tb['wakeups_s']:8.1f}"
              f"{ta['pss_mb']:7.0f} ->{tb['pss_mb']:6.0f}"
              f"{fa.get('painted_s', 0):7.1f} ->{fb.get('painted_s', 0):6.1f}"
              f"{fa.get('work_us', {}).get('99', 0) / 1000:8.2f} ->"
              f"{fb.get('work_us', {}).get('99', 0) / 1000:7.2f}")


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--shell-bin", default=str(REPO / "target/release/shell"))
    p.add_argument("--launcher-bin")
    p.add_argument("--apps-data-dir")
    p.add_argument("--runtime-bundle-dir")
    p.add_argument("--apps", default=DEFAULT_APPS, help="comma-separated app ids; empty for none")
    p.add_argument("--outputs", default=DEFAULT_OUTPUTS)
    p.add_argument("--no-watch", dest="watch", action="store_false",
                   help="don't rasterize painted frames (the headless default)")
    p.add_argument("--seconds", type=float, default=10.0, help="length of each phase")
    p.add_argument("--settle", type=float, default=4.0)
    p.add_argument("--skip", default="", help="comma-separated phases to skip")
    p.add_argument("--allowed-gpu", help="comma-separated PCI addresses the session may open")
    p.add_argument("--startup-timeout", type=float, default=60.0)
    p.add_argument("--app-timeout", type=float, default=30.0)
    p.add_argument("--label", default="run")
    p.add_argument("--out", default=str(REPO / "target/resource-bench"))
    p.add_argument("--compare", nargs=2, metavar=("A", "B"))
    args = p.parse_args()
    if args.compare:
        compare(*(json.loads(Path(x).read_text()) for x in args.compare))
        return 0
    result = run(args)
    path = Path(args.out) / f"{args.label}.json"
    path.write_text(json.dumps(result, indent=2))
    print_report(result)
    print(f"\n  wrote {path}")
    return 1 if result["violations"] else 0


if __name__ == "__main__":
    sys.exit(main())
