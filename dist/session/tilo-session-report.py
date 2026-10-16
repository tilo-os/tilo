#!/usr/bin/python3
"""Summarise private Tilo session flight records without controlling the desktop."""

from __future__ import annotations

import argparse
import csv
import json
import os
import re
import sys
from collections import Counter
from pathlib import Path


PAINT = re.compile(
    r"paint-stats: (?P<painted>\d+)/(?P<ticks>\d+) painted .*?"
    r"(?P<hz>[0-9.]+) Hz target \| present=(?P<present>\d+) "
    r"backpressure=(?P<backpressure>\d+)"
)
ANSI = re.compile(r"\x1b\[[0-9;]*m")
APP_FAILURE = re.compile(
    r"package_id=(?P<id>[\w.-]+).*?display_name=(?P<name>.+?)\s+exit_code=",
)
RENDER_PHASE_FIELD = re.compile(
    r"\b(frames|direct_frames|direct_dropped|flip_completions|unprofiled_frames|window_us|bridge_us|scanout_us|readback_bytes|output_bytes|avg_direct_us|max_direct_us|direct_failures)=(\d+)\b"
)
DIRECT_STATUS = re.compile(
    r"\bdirect_scanout_status=(?P<status>[\w-]+)(?:\s+direct_scanout_reason=(?P<reason>\"(?:[^\"\\]|\\.)*\"|\S+))?"
)
KMS_MODE = re.compile(
    r"DRM scanout: modeset complete .*?connector=(?P<connector>\d+) .*?"
    r"width=(?P<width>\d+) .*?height=(?P<height>\d+) .*?"
    r"refresh_mhz=(?P<refresh_mhz>\d+)"
)
KEY_LATENCY = re.compile(
    r"KEY-LATENCY app=(?P<app>\S*) key_to_commit_us=(?P<key_to_commit_us>\d+) "
    r"key_to_submit_us=(?P<key_to_submit_us>\d+) key_to_present_us=(?P<key_to_present_us>\d+)"
)
KEY_UNANSWERED = re.compile(r"KEY-LATENCY app=(?P<app>\S*) unanswered=1")
LAUNCH_PACKAGE = re.compile(r"\bpackage_id=(?:\x1b\[[0-9;]*m)*(?P<id>[\w.-]+)")
LAUNCH_FIELD = re.compile(
    r"\b(request_to_dispatch_us|request_to_spawn_us|request_to_map_us|"
    r"spawn_to_map_us|request_to_first_present_us|spawn_to_first_present_us|"
    r"map_to_first_present_us)=(?:Some\()?(\d+)"
)


def percentile(values: list[int], percentile_value: int) -> int:
    if not values:
        return 0
    values.sort()
    rank = max(1, (percentile_value * len(values) + 99) // 100)
    return values[min(rank - 1, len(values) - 1)]


def trace_report(path: Path) -> dict[str, object]:
    if not path.is_file():
        return {"available": False, "path": str(path)}
    rows: list[dict[str, str]] = []
    try:
        with path.open(newline="", errors="replace") as stream:
            rows = list(csv.DictReader(stream))
    except (OSError, csv.Error) as error:
        return {"available": False, "path": str(path), "error": str(error)}

    def numbers(field: str, *, painted_only: bool = False) -> list[int]:
        result = []
        for row in rows:
            if painted_only and row.get("painted") != "1":
                continue
            value = row.get(field, "")
            if value:
                try:
                    result.append(int(value))
                except ValueError:
                    continue
        return result

    start = numbers("start_us")
    painted = sum(row.get("painted") == "1" for row in rows)
    span_us = max(start, default=0) - min(start, default=0)
    report: dict[str, object] = {
        "available": True,
        "path": str(path),
        "rows": len(rows),
        "painted": painted,
        "span_us": span_us,
        "painted_fps": painted * 1_000_000 / span_us if span_us else 0.0,
    }
    for field in (
        "period_us",
        "input_us",
        "pump_us",
        "layout_us",
        "scene_us",
        "submit_us",
        "clients_us",
        "sleep_us",
        "input_latency_us",
        "input_retirement_latency_us",
        "deadline_late_us",
    ):
        values = numbers(field, painted_only=field in {"layout_us", "scene_us", "submit_us"})
        report[field] = {
            "count": len(values),
            "p50": percentile(values.copy(), 50),
            "p95": percentile(values.copy(), 95),
            "p99": percentile(values.copy(), 99),
            "max": max(values, default=0),
        }
    # Main-thread work of a painted frame: layout, scene build and submit.
    frame_work = [
        sum(int(row.get(field) or 0) for field in ("layout_us", "scene_us", "submit_us"))
        for row in rows
        if row.get("painted") == "1"
    ]
    report["frame_work_us"] = {
        "count": len(frame_work),
        "p50": percentile(frame_work.copy(), 50),
        "p95": percentile(frame_work.copy(), 95),
        "max": max(frame_work, default=0),
    }
    report["backpressured_ticks"] = sum(
        row.get("backpressured") == "1" for row in rows
    )
    painted_starts = [
        int(row["start_us"])
        for row in rows
        if row.get("painted") == "1" and row.get("start_us", "").isdigit()
    ]
    paint_gaps = [
        current - previous
        for previous, current in zip(painted_starts, painted_starts[1:])
    ]
    report["paint_period_us"] = {
        "count": len(paint_gaps),
        "p50": percentile(paint_gaps.copy(), 50),
        "p95": percentile(paint_gaps.copy(), 95),
        "p99": percentile(paint_gaps.copy(), 99),
        "max": max(paint_gaps, default=0),
    }
    return report


def flight_report(path: Path, events_path: Path | None = None) -> dict[str, object]:
    if not path.is_file():
        return {"available": False, "path": str(path)}
    try:
        lines = path.read_text(errors="replace").splitlines()
    except OSError as error:
        return {"available": False, "path": str(path), "error": str(error)}
    # The recorder keeps the log events (launch timing, launch failures) in
    # their own record, where hours of measurement lines cannot displace
    # them. A record from before it did has them in the full record only.
    event_lines = lines
    if events_path is not None and events_path.is_file():
        try:
            event_lines = events_path.read_text(errors="replace").splitlines()
        except OSError:
            event_lines = lines
    paint = [match.groupdict() for line in lines if (match := PAINT.search(line))]
    failures = Counter()
    for line in event_lines:
        if "application stopped before presenting a window" not in line:
            continue
        match = APP_FAILURE.search(ANSI.sub("", line))
        failures[match.group("id") if match else "unknown"] += 1
    slow = [line for line in lines if "SLOW-ITER " in line]
    panics = [
        line
        for line in lines
        if "panicked at " in line
        or "tilo-flight-record-v1" in line
        and "status=134" in line
    ]
    render_phases = []
    direct_status_samples = []
    kms_modes = []
    launch_samples: dict[str, dict[str, list[int]]] = {}
    key_samples: dict[str, dict[str, list[int]]] = {}
    for line in lines:
        clean = ANSI.sub("", line)
        if match := KMS_MODE.search(clean):
            kms_modes.append({key: int(value) for key, value in match.groupdict().items()})
        if match := DIRECT_STATUS.search(clean):
            direct_status_samples.append(match.groupdict())
        if match := KEY_LATENCY.search(clean):
            app = key_samples.setdefault(match.group("app") or "unknown", {})
            for key, value in match.groupdict().items():
                if key != "app":
                    app.setdefault(key, []).append(int(value))
        elif match := KEY_UNANSWERED.search(clean):
            app = key_samples.setdefault(match.group("app") or "unknown", {})
            app.setdefault("unanswered", []).append(1)
        if "DRM render phase summary" not in clean:
            continue
        fields = {key: int(value) for key, value in RENDER_PHASE_FIELD.findall(clean)}
        if all(
            key in fields
            for key in ("frames", "window_us", "readback_bytes", "output_bytes")
        ):
            render_phases.append(fields)
    for line in event_lines:
        clean = ANSI.sub("", line)
        launch_fields = {key: int(value) for key, value in LAUNCH_FIELD.findall(clean)}
        if launch_fields and (package := LAUNCH_PACKAGE.search(clean)):
            metrics = launch_samples.setdefault(package.group("id"), {})
            for key, value in launch_fields.items():
                metrics.setdefault(key, []).append(value)
    direct_enabled = any("DRM direct scanout enabled" in line for line in lines)
    direct_fallback = [
        ANSI.sub("", line)
        for line in lines
        if "DRM direct scanout unavailable" in line
        or "DRM direct frame failed" in line
        or "DRM direct scanout degraded" in line
    ]
    launch_latency = {}
    for package, metrics in sorted(launch_samples.items()):
        launch_latency[package] = {
            key: {
                "count": len(values),
                "p50": percentile(values.copy(), 50),
                "p95": percentile(values.copy(), 95),
                "max": max(values),
            }
            for key, values in sorted(metrics.items())
        }
    key_latency = {}
    for app, metrics in sorted(key_samples.items()):
        key_latency[app] = {
            key: (
                len(values)
                if key == "unanswered"
                else {
                    "count": len(values),
                    "p50": percentile(values.copy(), 50),
                    "p95": percentile(values.copy(), 95),
                    "p99": percentile(values.copy(), 99),
                    "max": max(values),
                }
            )
            for key, values in sorted(metrics.items())
        }
    idle_wake_storm_max = 0
    previous: dict[str, str] | None = None
    for sample in paint:
        if (
            sample["painted"] == "0"
            and previous is not None
            and sample["present"] == previous["present"]
            and sample["backpressure"] == previous["backpressure"]
        ):
            idle_wake_storm_max = max(idle_wake_storm_max, int(sample["ticks"]))
        previous = sample
    return {
        "available": True,
        "path": str(path),
        "bytes": path.stat().st_size,
        "lines": len(lines),
        "paint_samples": paint,
        "paint_last": paint[-1] if paint else None,
        "app_failures": dict(sorted(failures.items())),
        "slow_iterations": len(slow),
        "slow_iteration_tail": slow[-20:],
        "fatal_panic": bool(panics),
        "panic_tail": panics[-10:],
        "idle_wake_storm_max_ticks": idle_wake_storm_max,
        "direct_scanout_enabled": direct_enabled,
        "direct_scanout_status_last": (
            direct_status_samples[-1] if direct_status_samples else None
        ),
        "direct_scanout_fallback_tail": direct_fallback[-10:],
        "render_phase_samples": render_phases,
        "render_phase_last": render_phases[-1] if render_phases else None,
        "kms_modes": kms_modes,
        "launch_latency_us": launch_latency,
        "key_latency_us": key_latency,
        "xwayland_ready": any("Connected to Xwayland" in line for line in lines),
        "xwayland_unavailable": any("X11 compatibility unavailable" in line for line in lines),
    }


def render_gate(
    report: dict[str, object],
    *,
    require_direct: bool,
    render_budget_us: int,
    frame_work_budget_us: int,
) -> list[str]:
    """Why this login's presentation misses the render-path contract, if it does.

    Direct GPU scanout is the presenter wherever the renderer and the display
    share a GPU; once it ran, it must still run at the end (a failure only
    hands frames to the readback presenter until a retry). Frame times stay
    within the budgets on every path.
    """
    flight = report["flight"]
    frames = report["frames"]
    if not flight.get("available"):
        return [f"no session flight record at {flight.get('path')}"]
    phases = [phase for phase in flight["render_phase_samples"] if phase["frames"] > 0]
    if not phases:
        return ["the compositor logged no DRM render phase summary with frames"]
    failures = []
    last = phases[-1]
    status_last = flight["direct_scanout_status_last"] or {}
    status = status_last.get("status")
    if require_direct or flight["direct_scanout_enabled"]:
        if status != "enabled" or last.get("readback_bytes", 0) != 0:
            failures.append(
                f"direct GPU scanout is not presenting (status={status} "
                f"reason={status_last.get('reason')} readback_bytes={last.get('readback_bytes')}); "
                f"degradations: {flight['direct_scanout_fallback_tail']}"
            )
    for phase in phases:
        direct = phase.get("direct_frames", 0)
        if direct and phase.get("avg_direct_us", 0) > render_budget_us:
            failures.append(
                f"direct frames averaged {phase['avg_direct_us']} us over {direct} frames, "
                f"budget {render_budget_us} us"
            )
        readback = phase["frames"] - direct
        if readback > 0 and phase["bridge_us"] // readback > render_budget_us:
            failures.append(
                f"readback frames averaged {phase['bridge_us'] // readback} us over {readback} "
                f"frames, budget {render_budget_us} us"
            )
    if frames.get("available"):
        work = frames["frame_work_us"]["p95"]
        if work > frame_work_budget_us:
            failures.append(
                f"painted frames' main-thread work p95 is {work} us, budget {frame_work_budget_us} us"
            )
    return failures


def main() -> int:
    default_root = Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local/state")) / "tilo/session-logs"
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=default_root)
    parser.add_argument("--previous", action="store_true", help="report the prior login")
    parser.add_argument("--output", type=Path, help="write private JSON instead of stdout")
    parser.add_argument(
        "--render-gate",
        action="store_true",
        help="exit 1 unless presentation meets the render-path contract and budgets",
    )
    parser.add_argument(
        "--require-direct",
        action="store_true",
        help="with --render-gate: direct GPU scanout must present even if it never started",
    )
    parser.add_argument(
        "--key-latency",
        action="store_true",
        help="print key-to-present latency of typing, per app (from KEY-LATENCY records)",
    )
    parser.add_argument("--render-budget-ms", type=int, default=33)
    parser.add_argument("--frame-work-budget-ms", type=int, default=16)
    args = parser.parse_args()
    flight_name = "previous.log" if args.previous else "latest.log"
    events_name = "previous-events.log" if args.previous else "events.log"
    trace_name = "previous-frame-trace.csv" if args.previous else "frame-trace.csv"
    report = {
        "schema": "tilo-session-report-v1",
        "flight": flight_report(args.root / flight_name, args.root / events_name),
        "frames": trace_report(args.root / trace_name),
    }
    if args.key_latency:
        keys = report["flight"].get("key_latency_us") or {}
        if not keys:
            print("no typed keys were answered in this login's record")
        for app, metrics in keys.items():
            print(f"{app}: unanswered={metrics.get('unanswered', 0)}")
            for stage in ("key_to_commit_us", "key_to_submit_us", "key_to_present_us"):
                if stats := metrics.get(stage):
                    print(
                        f"  {stage[:-3]:<16} n={stats['count']:<5} p50={stats['p50'] / 1000:.1f}ms "
                        f"p95={stats['p95'] / 1000:.1f}ms p99={stats['p99'] / 1000:.1f}ms "
                        f"max={stats['max'] / 1000:.1f}ms"
                    )
        return 0
    if args.render_gate:
        failures = render_gate(
            report,
            require_direct=args.require_direct,
            render_budget_us=args.render_budget_ms * 1000,
            frame_work_budget_us=args.frame_work_budget_ms * 1000,
        )
        print(f"render gate: last phase summary {report['flight'].get('render_phase_last')}")
        print(f"render gate: last direct scanout status {report['flight'].get('direct_scanout_status_last')}")
        print(f"render gate: main-thread frame work {report['frames'].get('frame_work_us')}")
        for failure in failures:
            print(f"render gate: FAIL {failure}", file=sys.stderr)
        if not failures:
            print("render gate: PASS")
        return 1 if failures else 0
    payload = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        descriptor = os.open(args.output, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        os.fchmod(descriptor, 0o600)
        with os.fdopen(descriptor, "w") as stream:
            stream.write(payload)
    else:
        sys.stdout.write(payload)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
