#!/usr/bin/env python3
"""Measure the compositor's presentation loop.

Spawns `shell --mode native` with the frame-loop instrumentation enabled
(`TILO_FRAME_TRACE` / `TILO_FRAME_SUMMARY`, see
`crates/session/shell/src/compositor/metrics.rs`), optionally drives input through the
`TILO_TEST_CONTROL` socket and/or runs a continuously-painting Wayland
client against it, then reports the frame-time distribution.

The point is repeatability: every number this prints comes from the same
committed instrumentation the live binary carries, so a before/after
comparison is a comparison of the compositor, not of two ad-hoc scripts.

Examples
--------
    # Idle desktop, headless backend, 8 seconds.
    scripts/perf/frame-loop-bench.py --seconds 8

    # Under load from a fullscreen client that repaints every frame.
    scripts/perf/frame-loop-bench.py --seconds 8 --load fullscreen

    # Inject a pointer move every 100 ms so input-to-present latency is measured.
    scripts/perf/frame-loop-bench.py --seconds 8 --input-hz 10

    # Compare two runs.
    scripts/perf/frame-loop-bench.py --seconds 8 --label before --out target/bench
    ...
    scripts/perf/frame-loop-bench.py --seconds 8 --label after  --out target/bench
    scripts/perf/frame-loop-bench.py --compare target/bench/before.json \\
                                               target/bench/after.json

Backends
--------
`--backend headless` (default) needs no display and no TTY: it is the
only one that runs anywhere. `--backend winit` needs a Wayland/X11
parent — use `--nested` to wrap it in a headless `cage` so no window
appears on the operator's desktop. `--backend drm` needs a free TTY.
"""

from __future__ import annotations

import argparse
import csv
import json
import os
import re
import signal
import shutil
import socket
import subprocess
import sys
import threading
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]


def required_load_client(shell_bin: Path, load: str) -> Path | None:
    """A requested workload must exist before starting a compositor."""
    if load == "none":
        return None
    client = shell_bin.parent / "tilo-loadclient"
    if not client.is_file():
        raise FileNotFoundError(f"Requested {load} load requires {client}")
    return client


def stop_process_group(proc: subprocess.Popen) -> None:
    """Reap only the disposable session this runner started, including early failures."""
    if proc.poll() is not None:
        return
    try:
        os.killpg(proc.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        proc.wait(timeout=5)


# --------------------------------------------------------------------------
# trace parsing / statistics
# --------------------------------------------------------------------------

# Column order is defined by `metrics::CSV_HEADER`; we read the header
# rather than hard-coding indices so a new column can't silently shift
# every number this script prints.
TEXT_TRACE_COLUMNS = {"wake", "input_step", "paint_reasons", "paint_clients", "paint_animations"}
REQUIRED_TRACE_COLUMNS = {
    "start_us", "period_us", "input_us", "pump_us", "layout_us", "scene_us",
    "submit_us", "clients_us", "sleep_us", "painted", "present_async",
}


def read_trace(path: Path) -> tuple[list[str], list[dict]]:
    rows: list[dict] = []
    with path.open(encoding="utf-8", newline="") as fh:
        reader = csv.reader(fh, strict=True)
        try:
            header = next(reader, [])
        except csv.Error:
            raise ValueError("Malformed frame trace CSV header") from None
        if not all(header) or len(set(header)) != len(header):
            raise ValueError("Missing or duplicate frame trace columns")
        missing = REQUIRED_TRACE_COLUMNS.difference(header)
        if missing:
            raise ValueError(f"Missing required frame trace columns: {', '.join(sorted(missing))}")
        try:
            for parts in reader:
                if not parts:
                    continue
                if len(parts) != len(header):
                    raise ValueError(
                        f"Incomplete frame trace row {reader.line_num}: "
                        f"expected {len(header)} fields, found {len(parts)}"
                    )
                row = {}
                for key, value in zip(header, parts):
                    if key in TEXT_TRACE_COLUMNS:
                        row[key] = value
                    elif not value and key in REQUIRED_TRACE_COLUMNS:
                        raise ValueError(
                            f"Missing required frame trace value for {key} at row {reader.line_num}"
                        )
                    else:
                        try:
                            row[key] = int(value) if value else None
                        except ValueError:
                            raise ValueError(
                                f"Invalid numeric frame trace value for {key} at row {reader.line_num}"
                            ) from None
                rows.append(row)
        except csv.Error:
            raise ValueError(f"Malformed frame trace CSV at row {reader.line_num}") from None
    if not rows:
        raise ValueError("No frame trace samples were recorded")
    return header, rows


def pct(values: list[int], p: float) -> int:
    """Nearest-rank percentile, matching `metrics::percentiles`."""
    if not values:
        return 0
    v = sorted(values)
    n = len(v)
    rank = max(1, -(-int(p * n) // 100))
    return v[min(rank, n) - 1]


def summarize(rows: list[dict], warmup_us: int, target_us: int) -> dict:
    rows = [r for r in rows if r["start_us"] >= warmup_us]
    if not rows:
        return {"ticks": 0}
    painted = [r for r in rows if r["painted"]]
    periods = [r["period_us"] for r in rows if r["period_us"] > 0]
    paint_gaps = [
        b["start_us"] - a["start_us"] for a, b in zip(painted, painted[1:])
    ]
    span = rows[-1]["start_us"] - rows[0]["start_us"]
    lat = [r["input_latency_us"] for r in rows if r.get("input_latency_us")]
    retirement = [
        r["input_retirement_latency_us"]
        for r in rows
        if r.get("input_retirement_latency_us")
    ]
    on_cadence = (
        sum(1 for g in paint_gaps if abs(g - target_us) <= 2000) / len(paint_gaps)
        if paint_gaps and target_us
        else 0.0
    )

    def block(vals: list[int]) -> dict:
        return {
            "count": len(vals),
            "min": min(vals) if vals else 0,
            "p50": pct(vals, 50),
            "p95": pct(vals, 95),
            "p99": pct(vals, 99),
            "max": max(vals) if vals else 0,
            "mean": int(sum(vals) / len(vals)) if vals else 0,
        }

    return {
        "ticks": len(rows),
        "painted": len(painted),
        "span_us": span,
        "fps": len(painted) * 1e6 / span if span else 0.0,
        "target_period_us": target_us,
        "on_cadence_ratio": on_cadence,
        "tick_period": block(periods),
        "paint_period": block(paint_gaps),
        # `period_us` spans the previous tick's start to this one's, so
        # the park inside it is the *previous* sample's. Mirrors
        # `metrics::FrameSample::work_us`.
        "work": block(
            [
                max(0, b["period_us"] - a["sleep_us"])
                for a, b in zip(rows, rows[1:])
                if b["period_us"]
            ]
        ),
        "input": block([r["input_us"] for r in rows]),
        "pump": block([r["pump_us"] for r in rows]),
        "layout": block([r["layout_us"] for r in painted]),
        "scene": block([r["scene_us"] for r in painted]),
        "submit": block([r["submit_us"] for r in painted]),
        "clients": block([r["clients_us"] for r in rows]),
        "sleep": block([r["sleep_us"] for r in rows]),
        "input_latency": block(lat),
        "input_retirement_latency": block(retirement),
        "backpressured_ticks": sum(bool(r.get("backpressured")) for r in rows),
        "deadline_lateness": block(
            [r.get("deadline_late_us", 0) or 0 for r in painted]
        ),
        "present_async": bool(painted and painted[-1]["present_async"]),
    }


def fmt_block(name: str, b: dict) -> str:
    if not b["count"]:
        return f"  {name:<14} (no samples)"
    return (
        f"  {name:<14} n={b['count']:<6} "
        f"p50={b['p50'] / 1000:7.2f}ms  p95={b['p95'] / 1000:7.2f}ms  "
        f"p99={b['p99'] / 1000:7.2f}ms  max={b['max'] / 1000:8.2f}ms"
    )


def print_summary(s: dict, title: str) -> None:
    print(f"\n=== {title} ===")
    if not s.get("ticks"):
        print("  no samples (run too short, or the compositor never painted)")
        return
    print(
        f"  {s['ticks']} ticks, {s['painted']} painted over "
        f"{s['span_us'] / 1e6:.2f}s  ->  {s['fps']:.1f} painted fps"
    )
    tgt = s["target_period_us"]
    if tgt:
        print(
            f"  target period {tgt / 1000:.2f}ms "
            f"({1e6 / tgt:.1f} Hz); frames on cadence: "
            f"{s['on_cadence_ratio'] * 100:.1f}%"
        )
    print(fmt_block("tick period", s["tick_period"]))
    print(fmt_block("paint period", s["paint_period"]))
    print(fmt_block("work/tick", s["work"]))
    print(fmt_block("  input", s["input"]))
    print(fmt_block("  pump", s["pump"]))
    print(fmt_block("  layout", s["layout"]))
    print(fmt_block("  scene", s["scene"]))
    print(fmt_block("  submit", s["submit"]))
    print(fmt_block("  clients", s["clients"]))
    print(fmt_block("parked", s["sleep"]))
    kind = "input->queued" if s.get("present_async") else "input->present"
    print(fmt_block(kind, s["input_latency"]))
    empty = {"count": 0}
    print(fmt_block("input->retired", s.get("input_retirement_latency", empty)))
    print(fmt_block("deadline late", s.get("deadline_lateness", empty)))
    print(f"  backpressured ticks: {s.get('backpressured_ticks', 0)}")


def print_compare(a: dict, b: dict, na: str, nb: str) -> None:
    print(f"\n=== {na}  vs  {nb} ===")

    def row(label: str, ka: float, kb: float, unit: str = "ms", scale=1000.0) -> None:
        if not ka and not kb:
            return
        delta = (kb - ka) / ka * 100 if ka else float("nan")
        print(
            f"  {label:<24} {ka / scale:9.2f}{unit} -> {kb / scale:9.2f}{unit} "
            f"({delta:+7.1f}%)"
        )

    row("painted fps", a["fps"], b["fps"], "", 1.0)
    row("paint period p50", a["paint_period"]["p50"], b["paint_period"]["p50"])
    row("paint period p99", a["paint_period"]["p99"], b["paint_period"]["p99"])
    row("work/tick p50", a["work"]["p50"], b["work"]["p50"])
    row("work/tick p99", a["work"]["p99"], b["work"]["p99"])
    row("submit p50", a["submit"]["p50"], b["submit"]["p50"])
    row("submit p99", a["submit"]["p99"], b["submit"]["p99"])
    # A percentage change against a series with no samples is not a
    # result, it is a division by zero dressed up as one. Say which side
    # is missing instead.
    la, lb = a["input_latency"], b["input_latency"]
    if la["count"] and lb["count"]:
        row("input latency p50", la["p50"], lb["p50"])
        row("input latency p99", la["p99"], lb["p99"])
    elif lb["count"]:
        print(
            f"  {'input latency':<24}       n/a  -> "
            f"{lb['p50'] / 1000:9.2f}ms p50 (no samples in {na}; "
            f"not a delta)"
        )
    elif la["count"]:
        print(
            f"  {'input latency':<24} {la['p50'] / 1000:9.2f}ms p50 ->       n/a "
            f"(no samples in {nb}; not a delta)"
        )
    print(
        f"  {'on-cadence':<24} {a['on_cadence_ratio'] * 100:9.1f}%  -> "
        f"{b['on_cadence_ratio'] * 100:9.1f}%"
    )


# --------------------------------------------------------------------------
# driving a live compositor
# --------------------------------------------------------------------------


def wait_for_socket(path: Path, timeout: float) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists():
            try:
                s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                s.settimeout(1.0)
                s.connect(str(path))
                s.close()
                return True
            except OSError:
                pass
        time.sleep(0.05)
    return False


def wait_for_wayland_socket(name_file: Path, timeout: float) -> str | None:
    """The compositor prints `tilo compositor listening on "wayland-N"`."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if name_file.exists():
            text = name_file.read_text(errors="replace")
            for line in text.splitlines():
                if "compositor listening on" in line:
                    return line.rsplit('"', 2)[-2]
        time.sleep(0.05)
    return None


class Control:
    """Minimal TILO_TEST_CONTROL client (see scripts/headless-examples/)."""

    def __init__(self, path: Path, wake_fd: int | None = None):
        self.path = path
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(5.0)
        self.sock.connect(str(path))
        self.buf = b""
        self.wake_fd = wake_fd

    def send(self, obj: dict) -> dict:
        self.sock.sendall((json.dumps(obj) + "\n").encode())
        if self.wake_fd is not None:
            os.eventfd_write(self.wake_fd, 1)
        while b"\n" not in self.buf:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("test-control socket closed")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return json.loads(line)

    def send_debug(self, obj: dict) -> dict:
        started = time.monotonic()
        print(f"[bench] control -> {obj.get('op')}", flush=True)
        reply = self.send(obj)
        print(
            f"[bench] control <- {obj.get('op')} "
            f"({time.monotonic() - started:.3f}s)",
            flush=True,
        )
        return reply

    def close(self) -> None:
        self.sock.close()

    def reconnect(self) -> None:
        """Replace a wedged diagnostic connection without ending the run."""
        self.sock.close()
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(5.0)
        self.sock.connect(str(self.path))
        self.buf = b""


def drive_pointer(ctl: Control, hz: float, seconds: float, width: int, height: int) -> int:
    """Move the pointer at `hz`; unlike clicks, every move is a distinct input."""
    if hz <= 0:
        return 0
    interval = 1.0 / hz
    end = time.monotonic() + seconds
    n = 0
    while time.monotonic() < end:
        x = width * 0.25 + (n % 80) * width * 0.005
        y = height * 0.25 + (n % 50) * height * 0.008
        reply = ctl.send({"op": "pointer_move", "x": float(x), "y": float(y)})
        if not response_ok(reply):
            raise RuntimeError(f"pointer input refused: {reply}")
        n += 1
        # `send` is a synchronous round trip and already consumes part of the
        # period. Sleep only the remainder, otherwise the requested 20 Hz
        # silently degrades to roughly 16 Hz whenever one frame takes 10 ms.
        next_at = end - seconds + n * interval
        remaining = next_at - time.monotonic()
        if remaining > 0:
            time.sleep(remaining)
    return n


def response_ok(reply: dict) -> bool:
    return isinstance(reply, dict) and "err" not in reply


def process_cpu_ticks(pid: int) -> int:
    """Read process+retired-thread CPU time without touching the compositor hot path."""
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()
    # Fields after the comm start at proc field 3; utime/stime/cutime/cstime are 14..17.
    return sum(int(fields[index]) for index in range(11, 15))


def cpu_percent_between(pid: int, seconds: float) -> float:
    started = time.monotonic()
    first = process_cpu_ticks(pid)
    time.sleep(seconds)
    last = process_cpu_ticks(pid)
    elapsed = time.monotonic() - started
    return (last - first) / os.sysconf("SC_CLK_TCK") / elapsed * 100.0 if elapsed else 0.0


def foreign_frames(ctl: Control) -> list[dict]:
    reply = ctl.send({"op": "foreign_frames"})
    body = reply.get("foreign_frames", reply.get("ok", reply))
    if isinstance(body, dict):
        body = body.get("frames", body)
    return body if isinstance(body, list) else []


def launch_and_wait_for_app(
    ctl: Control,
    app_id: str,
    timeout: float,
    snapshot: Path | None = None,
    system_apps_dir: str | None = None,
    *,
    require_presented: bool = False,
) -> tuple[bool, float, str]:
    """Launch through the real palette keyboard path and require a first frame."""
    before = {f.get("surface_id") for f in foreign_frames(ctl)}
    started = time.monotonic()
    # This is intentionally the interaction a person performs: open Search,
    # type the package identity, then activate the selected retained result.
    # `actions_invoke` re-resolves a fresh headless catalogue and therefore
    # cannot catch a UI-path regression where Enter blocks the compositor.
    for command in (
        {"op": "modifiers", "ctrl": False, "shift": False, "alt": False, "logo": True},
        {"op": "key", "key": "k"},
        {"op": "modifiers", "ctrl": False, "shift": False, "alt": False, "logo": False},
        {"op": "type", "text": app_id},
    ):
        reply = ctl.send_debug(command)
        if not response_ok(reply):
            return False, 0.0, f"palette launch input refused: {reply}"
    # The retained application index is built off-thread. Give it a bounded
    # chance to publish and repaint before Enter; this wait is outside the
    # compositor and remains responsive to the concurrent input driver.
    time.sleep(min(1.0, timeout / 3.0))
    reply = ctl.send_debug({"op": "key", "key": "Enter"})
    if not response_ok(reply):
        return False, 0.0, f"palette activation refused: {reply}"
    deadline = started + timeout
    while time.monotonic() < deadline:
        frames = foreign_frames(ctl)
        for frame in frames:
            if (
                frame.get("surface_id") not in before
                and frame.get("app_id") == app_id
                and frame.get("frame_seq", 0) > 0
                and (not require_presented or frame.get("presented") is True)
            ):
                if snapshot is not None:
                    reply = ctl.send({"op": "snapshot", "path": str(snapshot)})
                    if not response_ok(reply):
                        return False, time.monotonic() - started, (
                            f"app painted but snapshot failed: {reply}"
                        )
                return True, time.monotonic() - started, "painted"
        # A capture worker completion wakes the compositor directly. Keep the
        # observer from becoming a continuously-chatty Wayland/control client
        # that changes the scheduling we are trying to measure.
        time.sleep(0.1)
    return False, timeout, "new app surface did not map and present before the deadline"


def dismiss_first_login(ctl: Control, timeout: float = 5.0) -> None:
    """Complete the disposable session's first-login flow via real key input."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        reply = ctl.send({"op": "list_targets"})
        targets = reply.get("targets", reply.get("ok", reply))
        names = {
            entry.get("name")
            for entry in targets
            if isinstance(entry, dict) and isinstance(entry.get("name"), str)
        } if isinstance(targets, list) else set()
        onboarding = sorted(name for name in names if name.startswith("onboarding."))
        if not onboarding:
            return
        clicked = False
        for name in (
            "onboarding.begin.button",
            "onboarding.understood.row",
            "onboarding.create.button",
            "onboarding.backup.skip-acknowledged.row",
            "onboarding.backup.continue.button",
            "onboarding.tour.skip.button",
            "onboarding.finish.button",
        ):
            if name in names:
                ctl.send({"op": "click_named", "name": name})
                clicked = True
                break
        if not clicked:
            return
        time.sleep(0.2)


def gate_failures(stats: dict, args: argparse.Namespace) -> list[str]:
    failures: list[str] = []
    if stats.get("ticks", 0) < args.min_ticks:
        failures.append(f"only {stats.get('ticks', 0)} measured ticks")
    if stats.get("fps", 0.0) < args.min_fps:
        failures.append(f"{stats.get('fps', 0.0):.1f} fps < {args.min_fps:.1f}")
    span_us = stats.get("span_us", 0)
    ticks_per_second = (
        stats.get("ticks", 0) * 1e6 / span_us if span_us else float("inf")
    )
    if ticks_per_second > args.max_ticks_per_second:
        failures.append(
            f"{ticks_per_second:.1f} ticks/s > {args.max_ticks_per_second:.1f} "
            "(possible unparked compositor spin)"
        )
    if stats.get("input_failure"):
        failures.append(stats["input_failure"])
    latency = stats.get("input_latency", {})
    if latency.get("count", 0) < args.min_input_samples:
        failures.append(
            f"only {latency.get('count', 0)} input-latency samples "
            f"(< {args.min_input_samples})"
        )
    elif latency.get("p99", 0) > args.max_input_p99_ms * 1000:
        failures.append(
            f"input latency p99 {latency['p99'] / 1000:.1f} ms > "
            f"{args.max_input_p99_ms:.1f} ms"
        )
    retirement = stats.get("input_retirement_latency", {})
    if retirement.get("count", 0) < args.min_input_samples:
        failures.append(
            f"only {retirement.get('count', 0)} input-retirement samples "
            f"(< {args.min_input_samples})"
        )
    elif retirement.get("p99", 0) > args.max_retirement_p99_ms * 1000:
        failures.append(
            f"input retirement p99 {retirement['p99'] / 1000:.1f} ms > "
            f"{args.max_retirement_p99_ms:.1f} ms"
        )
    if stats.get("on_cadence_ratio", 0.0) < args.min_on_cadence:
        failures.append(
            f"on-cadence {stats.get('on_cadence_ratio', 0.0) * 100:.1f}% < "
            f"{args.min_on_cadence * 100:.1f}%"
        )
    if stats.get("backpressured_ticks", 0) > args.max_backpressured_ticks:
        failures.append(
            f"{stats['backpressured_ticks']} backpressured ticks > "
            f"{args.max_backpressured_ticks}"
        )
    if stats.get("cpu_failure"):
        failures.append(f"CPU sampler failed: {stats['cpu_failure']}")
    elif stats.get("cpu_percent") is None:
        failures.append("CPU sampler produced no result")
    elif stats["cpu_percent"] > args.max_cpu_percent:
        failures.append(
            f"compositor CPU {stats['cpu_percent']:.1f}% > {args.max_cpu_percent:.1f}%"
        )
    if args.launch_app and not stats.get("app_painted"):
        failures.append(stats.get("app_failure", f"{args.launch_app} did not paint"))
    return failures


def run_once(args: argparse.Namespace, out_dir: Path) -> dict:
    shell_bin = Path(args.shell_bin) if args.shell_bin else REPO / "target/release/shell"
    if not shell_bin.exists():
        sys.exit(
            f"no shell binary at {shell_bin}\n"
            f"build it first: .claude/tilo-tools/qbuild-wt build -p shell --bin shell --release"
        )
    required_load_client(shell_bin, args.load)
    session_root = Path(tempfile.mkdtemp(prefix="tilo-frame-bench-"))
    try:
        return run_session(args, out_dir, session_root)
    finally:
        if args.keep_session_root:
            print(f"[bench] retained session root: {session_root}")
        else:
            shutil.rmtree(session_root, ignore_errors=True)


def run_session(args: argparse.Namespace, out_dir: Path, session_root: Path) -> dict:
    shell_bin = Path(args.shell_bin) if args.shell_bin else REPO / "target/release/shell"
    load_client = required_load_client(shell_bin, args.load)
    label = args.label or f"{args.backend}-{args.load}"
    trace = out_dir / f"{label}.csv"
    summary = out_dir / f"{label}.json"
    log = out_dir / f"{label}.log"
    # A failed launch must never reuse evidence from an earlier run with this label.
    for artifact in (trace, summary, out_dir / f"{label}.stats.json"):
        artifact.unlink(missing_ok=True)
    (session_root / "run").mkdir(mode=0o700)
    # Keep the control socket inside the same private runtime directory passed
    # to the compositor.  Choosing it from the caller's XDG_RUNTIME_DIR first
    # and then replacing XDG_RUNTIME_DIR below makes the driver wait on a path
    # the child can never create.
    ctl_path = session_root / "run" / f"tilo-bench-{os.getpid()}.sock"
    env = dict(os.environ)
    env["TILO_FRAME_TRACE"] = str(trace)
    env["TILO_FRAME_SUMMARY"] = str(summary)
    env["TILO_FRAME_WARMUP_MS"] = str(args.warmup_ms)
    # Startup/app-launch time is outside the measured interval. Give the live
    # process enough lifetime to reach that interval before its clean exit.
    process_seconds = args.seconds + (args.app_timeout if args.launch_app else 0.0) + 2.0
    env["TILO_RUN_SECONDS"] = str(process_seconds)
    env["TILO_TEST_CONTROL"] = str(ctl_path)
    wake_fd = None
    env["TILO_HEADLESS_SIZE"] = f"{args.width}x{args.height}"
    env["HOME"] = str(session_root / "home")
    env["XDG_CONFIG_HOME"] = str(session_root / "config")
    env["XDG_DATA_HOME"] = str(session_root / "data")
    env["XDG_CACHE_HOME"] = str(session_root / "cache")
    env["XDG_STATE_HOME"] = str(session_root / "state")
    env["XDG_RUNTIME_DIR"] = str(session_root / "run")
    env["TILO_STATE_PATH"] = str(session_root / "config" / "state.json")
    env["TILO_IDENTITY_ROOT"] = str(session_root / "identity-root")
    env["TILO_PEER_KEY_PATH"] = str(session_root / "keys" / "peer-key")
    # The explicit test-control session is an already-booted desktop harness,
    # not a first-login test. Provision its throwaway identity up front so the
    # acceptance window measures the desktop and Chat rather than onboarding.
    provision = subprocess.run(
        [str(shell_bin), "--provision-identity", "--disable-peer-sync"],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        timeout=args.startup_timeout,
    )
    if provision.returncode != 0:
        sys.exit(f"[bench] could not provision disposable identity:\n{provision.stdout}")
    process_started = time.monotonic()
    if args.refresh_hz:
        env["TILO_REFRESH_HZ"] = str(args.refresh_hz)
    env["RUST_BACKTRACE"] = "1"
    if args.rust_log:
        env["RUST_LOG"] = args.rust_log
    if args.launch_app:
        if args.launcher_bin:
            env["TILO_LAUNCHER_BIN"] = str(Path(args.launcher_bin).resolve())
        if args.apps_data_dir:
            env["TILO_APPS_DATA_DIR"] = str(Path(args.apps_data_dir).resolve())
        if args.system_apps_dir:
            env["TILO_APPS_SYSTEM_DIR"] = str(
                Path(args.system_apps_dir).resolve()
            )
        if args.system_payload_dir:
            env["TILO_APPS_SYSTEM_PAYLOAD_DIR"] = str(
                Path(args.system_payload_dir).resolve()
            )
        if args.runtime_bundle_dir:
            env["TILO_RUNTIME_BUNDLE_DIR"] = str(
                Path(args.runtime_bundle_dir).resolve()
            )
        if args.trusted_publishers_file:
            env["TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE"] = str(
                Path(args.trusted_publishers_file).resolve()
            )
    if args.backend == "headless":
        # A headless run must not pick up the operator's session.
        env.pop("WAYLAND_DISPLAY", None)
        env.pop("DISPLAY", None)

    cmd = [
        str(shell_bin),
        "--mode",
        "native",
        "--backend",
        args.backend,
        "--disable-peer-sync",
    ]
    if args.nested:
        cage = shutil.which("cage")
        if not cage:
            sys.exit("--nested needs `cage` on PATH")
        # wlroots headless backend: a real Wayland parent with no window
        # on the operator's desktop.
        env["WLR_BACKENDS"] = "headless"
        env["WLR_LIBINPUT_NO_DEVICES"] = "1"
        env.pop("WAYLAND_DISPLAY", None)
        cmd = [cage, "-s", "--"] + cmd

    app_painted = None
    app_launch_seconds = None
    app_failure = None
    app_snapshot = out_dir / f"{label}.app.png" if args.launch_app else None
    input_failure = None

    print(f"[bench] {label}: {' '.join(cmd)}")
    with log.open("wb") as logfh:
        proc = subprocess.Popen(
            cmd,
            env=env,
            stdout=logfh,
            stderr=subprocess.STDOUT,
            pass_fds=(),
            start_new_session=True,
        )

        load_proc = None
        try:
            if not wait_for_socket(ctl_path, timeout=args.startup_timeout):
                sys.exit(
                    f"[bench] control socket never appeared; see {log}\n"
                    + log.read_text(errors="replace")[-3000:]
                )

            if args.load != "none":
                wl = wait_for_wayland_socket(log, timeout=5.0)
                if wl is None:
                    sys.exit(f"[bench] requested load has no Wayland endpoint; see {log}")
                else:
                    if load_client is not None:
                        cenv = dict(os.environ)
                        cenv["WAYLAND_DISPLAY"] = wl
                        # The Wayland socket belongs to this benchmark's
                        # disposable compositor, not to the operator's live
                        # desktop. Pointing at the caller runtime silently
                        # makes the load test exercise nothing.
                        cenv["XDG_RUNTIME_DIR"] = str(session_root / "run")
                        ccmd = [
                            str(load_client),
                            "--size",
                            f"{args.width}x{args.height}",
                            "--app-id",
                            "dev.tilo.frame-loop-load",
                            "--seconds",
                            str(process_seconds),
                        ]
                        if args.load == "fullscreen":
                            ccmd.append("--fullscreen")
                        print(f"[bench] load client on {wl}: {' '.join(ccmd)}")
                        load_proc = subprocess.Popen(
                            ccmd,
                            env=cenv,
                            stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT,
                            text=True,
                        )
                        # Give it a moment to map before input starts.
                        time.sleep(0.5)
                        if load_proc.poll() is not None:
                            raise RuntimeError("requested load client exited before measurement")

            launch_result: dict[str, object] = {
                "painted": None,
                "seconds": None,
                "failure": None,
            }
            launch_thread = None
            launch_ctl = None
            if args.launch_app:
                # Run the user launch interaction synchronously before the
                # sustained input phase. Multiple concurrent request/response
                # streams made failures ambiguous and are not representative
                # of one person's keyboard action. Chat remains alive for the
                # complete measured pointer-load interval below.
                try:
                    launch_ctl = Control(ctl_path, wake_fd)
                    dismiss_first_login(launch_ctl)
                    painted, seconds, failure = launch_and_wait_for_app(
                        launch_ctl,
                        args.launch_app,
                        args.app_timeout,
                        app_snapshot,
                        env.get("TILO_APPS_SYSTEM_DIR"),
                        require_presented=args.require_presented_app,
                    )
                    launch_result.update(
                        painted=painted, seconds=seconds, failure=failure
                    )
                    launch_ctl.close()
                except Exception as exc:
                    launch_result["painted"] = False
                    launch_result["failure"] = f"app driver failed: {exc}"

            # Exclude compositor startup and app first-paint from cadence
            # percentiles. Chat remains alive during the full measured load.
            measurement_start_us = int(
                max(0.0, time.monotonic() - process_started) * 1_000_000
            )

            input_result: dict[str, object] = {"sent": 0, "error": None}
            cpu_result: dict[str, object] = {"percent": None, "error": None}
            def sample_cpu() -> None:
                try:
                    cpu_result["percent"] = cpu_percent_between(proc.pid, args.seconds)
                except (OSError, ValueError) as exc:
                    cpu_result["error"] = str(exc)
            cpu_thread = threading.Thread(target=sample_cpu, daemon=True)
            cpu_thread.start()
            input_thread = None
            if args.input_hz > 0:

                def inject() -> None:
                    try:
                        input_ctl = Control(ctl_path, wake_fd)
                        input_result["sent"] = drive_pointer(
                            input_ctl,
                            args.input_hz,
                            args.seconds,
                            args.width,
                            args.height,
                        )
                        input_ctl.close()
                    except Exception as exc:  # surfaced below as a gate failure
                        input_result["error"] = str(exc)

                input_thread = threading.Thread(target=inject, daemon=True)
                input_thread.start()

            if input_thread is not None:
                input_thread.join(timeout=args.seconds + 2.0)
                sent = int(input_result["sent"])
                print(f"[bench] injected {sent} pointer moves at {args.input_hz} Hz")
                if input_result["error"]:
                    input_failure = f"input driver failed: {input_result['error']}"
            cpu_thread.join(timeout=args.seconds + 2.0)

            if args.launch_app:
                app_painted = bool(launch_result["painted"])
                app_launch_seconds = launch_result["seconds"]
                app_failure = launch_result["failure"]

            status = proc.wait(timeout=process_seconds + args.startup_timeout + 15)
            if status != 0:
                raise RuntimeError(f"compositor exited with status {status}; see {log}")
        except subprocess.TimeoutExpired as error:
            raise RuntimeError(f"compositor overran its deadline; see {log}") from error
        finally:
            stop_process_group(proc)
            if load_proc is not None:
                try:
                    cout, _ = load_proc.communicate(timeout=5)
                except subprocess.TimeoutExpired:
                    load_proc.kill()
                    cout, _ = load_proc.communicate(timeout=5)
                for line in (cout or "").splitlines():
                    if line.startswith("LOADCLIENT"):
                        print(f"[bench] {line}")
                if sys.exc_info()[0] is None:
                    if load_proc.returncode != 0:
                        raise RuntimeError(f"requested load client failed: {load_proc.returncode}")
                    if not re.search(r"^LOADCLIENT frames=[1-9][0-9]*(?:\s|$)", cout or "", re.MULTILINE):
                        raise RuntimeError("requested load client produced no frames")
            ctl_path.unlink(missing_ok=True)

    if not trace.exists():
        sys.exit(f"[bench] no trace written; see {log}")
    _, rows = read_trace(trace)
    # Recompute from the trace rather than trusting the in-process
    # summary: the trace survives an unclean exit, and recomputing keeps
    # the harness able to re-slice a run (different warmup, subranges)
    # without re-running the compositor.
    stats = summarize(
        rows,
        max(args.warmup_ms * 1000, measurement_start_us),
        args.target_us,
    )
    stats["label"] = label
    stats["backend"] = args.backend
    stats["load"] = args.load
    stats["trace"] = str(trace)
    stats["app_id"] = args.launch_app
    stats["app_painted"] = app_painted
    stats["app_launch_seconds"] = app_launch_seconds
    stats["app_failure"] = app_failure
    stats["app_snapshot"] = str(app_snapshot) if app_snapshot else None
    stats["input_failure"] = input_failure
    stats["cpu_percent"] = cpu_result["percent"]
    stats["cpu_failure"] = cpu_result["error"]
    (out_dir / f"{label}.stats.json").write_text(json.dumps(stats, indent=2))
    return stats


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--seconds", type=float, default=8.0, help="run length (default 8)")
    p.add_argument("--backend", default="headless", choices=["headless", "winit", "drm"])
    p.add_argument(
        "--load",
        default="none",
        choices=["none", "windowed", "fullscreen"],
        help="run a continuously-painting Wayland client alongside",
    )
    p.add_argument("--input-hz", type=float, default=0.0, help="pointer-move injection rate")
    p.add_argument(
        "--launch-app",
        metavar="APP_ID",
        help="launch a native app through the applications provider and require a first frame",
    )
    p.add_argument("--app-timeout", type=float, default=10.0)
    p.add_argument("--launcher-bin", help="tilo-launcher binary used by developer builds")
    p.add_argument("--apps-data-dir", help="installed-app registry root")
    p.add_argument("--system-apps-dir", help="immutable system-app registry root")
    p.add_argument("--system-payload-dir", help="immutable system-app payload root")
    p.add_argument("--runtime-bundle-dir", help="runtime bundle root")
    p.add_argument("--rust-log", help="RUST_LOG filter for compositor diagnostics")
    p.add_argument(
        "--keep-session-root",
        action="store_true",
        help="retain the disposable HOME/runtime/app-data tree for postmortem diagnostics",
    )
    p.add_argument(
        "--trusted-publishers-file",
        help="trusted-publisher list for the disposable launcher runtime",
    )
    p.add_argument("--width", type=int, default=1600)
    p.add_argument("--height", type=int, default=900)
    p.add_argument("--warmup-ms", type=int, default=1500)
    p.add_argument(
        "--target-us",
        type=int,
        default=0,
        help="refresh interval the loop should be hitting, microseconds "
        "(default: derived from --refresh-hz, else 60 Hz)",
    )
    p.add_argument(
        "--refresh-hz",
        type=float,
        help="force the compositor's presentation rate via TILO_REFRESH_HZ, "
        "and score the run against it. The only way to measure a non-60 Hz "
        "cadence on a machine whose backend reports no display rate.",
    )
    p.add_argument("--label", help="name for the output files")
    p.add_argument("--out", default="target/frame-bench", help="output directory")
    p.add_argument("--shell-bin", help="path to the shell binary")
    p.add_argument("--startup-timeout", type=float, default=30.0)
    p.add_argument(
        "--gate", action="store_true", help="fail unless responsiveness thresholds pass"
    )
    p.add_argument("--min-fps", type=float, default=120.0)
    p.add_argument("--max-input-p99-ms", type=float, default=25.0)
    p.add_argument("--max-retirement-p99-ms", type=float, default=25.0)
    p.add_argument("--min-on-cadence", type=float, default=0.90)
    p.add_argument("--max-backpressured-ticks", type=int, default=0)
    p.add_argument("--max-cpu-percent", type=float, default=100.0)
    p.add_argument(
        "--require-presented-app",
        action="store_true",
        help="require the launched foreign surface to retire a presented frame, not merely map",
    )
    p.add_argument("--min-input-samples", type=int, default=20)
    p.add_argument("--min-ticks", type=int, default=100)
    p.add_argument(
        "--max-ticks-per-second",
        type=float,
        default=500.0,
        help="reject runaway compositor loops even when rendering is backpressured",
    )
    p.add_argument(
        "--nested",
        action="store_true",
        help="wrap a winit run in a headless `cage` (no window on the desktop)",
    )
    p.add_argument(
        "--compare",
        nargs=2,
        metavar=("BEFORE", "AFTER"),
        help="print a diff of two *.stats.json files and exit",
    )
    p.add_argument(
        "--report",
        metavar="STATS_JSON",
        help="print an existing *.stats.json and exit",
    )
    p.add_argument(
        "--resummarize",
        metavar="TRACE_CSV",
        help="recompute (and rewrite) the stats for an existing trace and exit; "
        "lets a run be re-sliced with a different --warmup-ms / --target-us, "
        "or re-read after a fix to the summariser, without re-running the "
        "compositor",
    )
    args = p.parse_args()

    # A run is scored against the rate the compositor was actually told to
    # hit; scoring a forced 144 Hz run against 60 Hz would report 0% on
    # cadence for a loop doing exactly what it was asked.
    if not args.target_us:
        args.target_us = int(round(1e6 / args.refresh_hz)) if args.refresh_hz else 16667

    if args.resummarize:
        trace = Path(args.resummarize)
        _, rows = read_trace(trace)
        stats = summarize(rows, args.warmup_ms * 1000, args.target_us)
        stats["label"] = args.label or trace.stem
        stats["trace"] = str(trace)
        out = trace.with_suffix("").with_suffix(".stats.json")
        out.write_text(json.dumps(stats, indent=2))
        print_summary(stats, stats["label"])
        print(f"\n[bench] stats: {out}")
        return

    if args.compare:
        a = json.loads(Path(args.compare[0]).read_text())
        b = json.loads(Path(args.compare[1]).read_text())
        print_summary(a, a.get("label", args.compare[0]))
        print_summary(b, b.get("label", args.compare[1]))
        print_compare(a, b, a.get("label", "before"), b.get("label", "after"))
        return

    if args.report:
        s = json.loads(Path(args.report).read_text())
        print_summary(s, s.get("label", args.report))
        return

    out_dir = Path(args.out)
    if not out_dir.is_absolute():
        out_dir = REPO / out_dir
    out_dir.mkdir(parents=True, exist_ok=True)
    stats = run_once(args, out_dir)
    print_summary(stats, stats["label"])
    print(f"\n[bench] trace:   {stats['trace']}")
    print(f"[bench] stats:   {out_dir / (stats['label'] + '.stats.json')}")
    if args.gate:
        failures = gate_failures(stats, args)
        if failures:
            print("\n[bench] FAIL: " + "; ".join(failures), file=sys.stderr)
            raise SystemExit(1)
        print("\n[bench] PASS: responsiveness acceptance thresholds met")


if __name__ == "__main__":
    main()
