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

    # Inject a click every 100 ms so input-to-present latency is measured.
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
import json
import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]


# --------------------------------------------------------------------------
# trace parsing / statistics
# --------------------------------------------------------------------------

# Column order is defined by `metrics::CSV_HEADER`; we read the header
# rather than hard-coding indices so a new column can't silently shift
# every number this script prints.
def read_trace(path: Path) -> tuple[list[str], list[dict]]:
    rows: list[dict] = []
    with path.open() as fh:
        header = fh.readline().strip().split(",")
        for line in fh:
            line = line.strip()
            if not line:
                continue
            parts = line.split(",")
            if len(parts) != len(header):
                continue
            row = {}
            for k, v in zip(header, parts):
                row[k] = int(v) if v else None
            rows.append(row)
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

    def __init__(self, path: Path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(5.0)
        self.sock.connect(str(path))
        self.buf = b""

    def send(self, obj: dict) -> dict:
        self.sock.sendall((json.dumps(obj) + "\n").encode())
        while b"\n" not in self.buf:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("test-control socket closed")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return json.loads(line)

    def close(self) -> None:
        self.sock.close()


def drive_input(ctl: Control, hz: float, seconds: float, width: int, height: int) -> int:
    """Inject clicks at `hz` for `seconds`. Returns the count sent.

    Clicks land on a moving point well inside the viewport so they hit
    the shell's own chrome (which repaints) rather than a client tile.
    """
    if hz <= 0:
        return 0
    interval = 1.0 / hz
    end = time.monotonic() + seconds
    n = 0
    while time.monotonic() < end:
        x = width * 0.5 + (n % 20) * 4
        y = height * 0.5 + (n % 13) * 4
        try:
            ctl.send({"op": "click_at", "x": float(x), "y": float(y)})
        except OSError:
            break
        n += 1
        time.sleep(interval)
    return n


def run_once(args: argparse.Namespace, out_dir: Path) -> dict:
    shell_bin = Path(args.shell_bin) if args.shell_bin else REPO / "target/release/shell"
    if not shell_bin.exists():
        sys.exit(
            f"no shell binary at {shell_bin}\n"
            f"build it first: .claude/tilo-tools/qbuild-wt build -p shell --bin shell --release"
        )
    label = args.label or f"{args.backend}-{args.load}"
    trace = out_dir / f"{label}.csv"
    summary = out_dir / f"{label}.json"
    log = out_dir / f"{label}.log"
    ctl_path = Path(f"/run/user/{os.getuid()}/tilo-bench-{os.getpid()}.sock")
    ctl_path.unlink(missing_ok=True)

    env = dict(os.environ)
    env["TILO_FRAME_TRACE"] = str(trace)
    env["TILO_FRAME_SUMMARY"] = str(summary)
    env["TILO_FRAME_WARMUP_MS"] = str(args.warmup_ms)
    env["TILO_RUN_SECONDS"] = str(args.seconds)
    env["TILO_TEST_CONTROL"] = str(ctl_path)
    env["TILO_HEADLESS_SIZE"] = f"{args.width}x{args.height}"
    if args.refresh_hz:
        env["TILO_REFRESH_HZ"] = str(args.refresh_hz)
    env["RUST_BACKTRACE"] = "1"
    if args.backend == "headless":
        # A headless run must not pick up the operator's session.
        env.pop("WAYLAND_DISPLAY", None)
        env.pop("DISPLAY", None)

    cmd = [str(shell_bin), "--mode", "native", "--backend", args.backend]
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

    print(f"[bench] {label}: {' '.join(cmd)}")
    with log.open("wb") as logfh:
        proc = subprocess.Popen(cmd, env=env, stdout=logfh, stderr=subprocess.STDOUT)

        load_proc = None
        try:
            if not wait_for_socket(ctl_path, timeout=args.startup_timeout):
                proc.kill()
                sys.exit(
                    f"[bench] control socket never appeared; see {log}\n"
                    + log.read_text(errors="replace")[-3000:]
                )

            if args.load != "none":
                wl = wait_for_wayland_socket(log, timeout=5.0)
                if wl is None:
                    print("[bench] warning: no wayland socket name in the log; skipping load")
                else:
                    client = shell_bin.parent / "tilo-loadclient"
                    if not client.exists():
                        print(f"[bench] warning: {client} missing; skipping load")
                    else:
                        cenv = dict(os.environ)
                        cenv["WAYLAND_DISPLAY"] = wl
                        cenv["XDG_RUNTIME_DIR"] = os.environ.get(
                            "XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}"
                        )
                        ccmd = [
                            str(client),
                            "--size",
                            f"{args.width}x{args.height}",
                            "--seconds",
                            str(args.seconds),
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

            if args.input_hz > 0:
                ctl = Control(ctl_path)
                sent = drive_input(
                    ctl, args.input_hz, args.seconds - 1.0, args.width, args.height
                )
                ctl.close()
                print(f"[bench] injected {sent} clicks at {args.input_hz} Hz")

            proc.wait(timeout=args.seconds + args.startup_timeout + 15)
        except subprocess.TimeoutExpired:
            print("[bench] compositor overran its deadline; terminating")
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
        finally:
            if load_proc is not None:
                try:
                    cout, _ = load_proc.communicate(timeout=5)
                except subprocess.TimeoutExpired:
                    load_proc.kill()
                    cout = ""
                for line in (cout or "").splitlines():
                    if line.startswith("LOADCLIENT"):
                        print(f"[bench] {line}")
            ctl_path.unlink(missing_ok=True)

    if not trace.exists():
        sys.exit(f"[bench] no trace written; see {log}")
    _, rows = read_trace(trace)
    # Recompute from the trace rather than trusting the in-process
    # summary: the trace survives an unclean exit, and recomputing keeps
    # the harness able to re-slice a run (different warmup, subranges)
    # without re-running the compositor.
    stats = summarize(rows, args.warmup_ms * 1000, args.target_us)
    stats["label"] = label
    stats["backend"] = args.backend
    stats["load"] = args.load
    stats["trace"] = str(trace)
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
    p.add_argument("--input-hz", type=float, default=0.0, help="click injection rate")
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


if __name__ == "__main__":
    main()
