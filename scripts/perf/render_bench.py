#!/usr/bin/env python3
"""Render benchmarks at native 4K for the two performance lanes (TIL-582).

The KPI is "Frame time p99 at 4K/144 Hz" (Linear document "Performance and
cost KPIs"): at most 6.9 ms for a damage-limited frame and 16.7 ms for a full
repaint. Two release harnesses measure it, and this script runs both the same
way in every lane:

- `ui-perf dashboard_perf_bench` (`crates/dev/ui-examples`): a ~250-widget
  toolkit window driven through hover, click and animation events, each
  redraw drawn into an offscreen target;
- `redraw_bench` (`crates/session/shell/examples`): 1,000 clicks through the
  shell's `FrameBuilder`, each painted, rendered by the DRM bridge, read back
  and swizzled into an XRGB buffer. No page flip or scanout.

Subcommands:

- `counters UI_PERF`: the per-change gate. Runner-independent counters only:
  allocator calls and bytes per redraw, native pixels rasterised per redraw,
  and allocations per 1,000 events that need no redraw, against the budgets
  `docs/ui-performance-benchmark.md` documents. No clock is compared.
- `timing UI_PERF REDRAW_BENCH`: the scheduled lane. Frame-time samples from
  both harnesses become `render.ui.frame_us.4k` and
  `render.shell.redraw_us.4k` records (`tools/perf/record.py`).

Both start with the damage-verification preflight: every retained frame must
match a full render of the same scene, or no number of the run counts.

    scripts/perf/render_bench.py counters target/release/examples/ui-perf --out target/render
    scripts/perf/render_bench.py timing target/release/examples/ui-perf \\
        target/release/examples/redraw_bench --env-json env.json --out target/render \\
        --records target/render/render.jsonl

A software renderer (lavapipe) times the CPU's rasterisation, not a GPU's: its
records carry `renderer=lavapipe` and are compared only with each other.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "tools/perf"))

import record  # noqa: E402

NATIVE_4K = {"TILO_UI_DASH_WIDTH": "3840", "TILO_UI_DASH_HEIGHT": "2160"}

# docs/ui-performance-benchmark.md, "Pixel-equivalence preflight".
VERIFY = {**NATIVE_4K, "TILO_UI_DASH_VERIFY": "1", "TILO_UI_DASH_VERIFY_FRAMES": "40",
          "TILO_UI_DASH_ITERS": "40"}
# The documented acceptance budgets for the animated 4K redraw and the
# one-million-event no-redraw gate, with fewer events: both are rates, and a
# per-change run need not wait for the full count. The raster-pixel budget sits
# about 30 % above the 95,000-116,000 px/redraw the lane measured in October
# 2026 (TIL-744), where 600,000 let a redraw grow five-fold unnoticed.
REDRAW_COUNTERS = {**NATIVE_4K, "TILO_UI_DASH_ITERS": "200",
                   "TILO_UI_DASH_ALLOC_CALL_BUDGET_PER_REDRAW": "650",
                   "TILO_UI_DASH_ALLOC_BYTE_BUDGET_PER_REDRAW": "70000",
                   "TILO_UI_DASH_RASTER_PIXEL_BUDGET": "150000"}
IDLE_COUNTERS = {"TILO_UI_DASH_ITERS": "100000", "TILO_UI_DASH_STATIC": "1", "TILO_UI_DASH_FROZEN": "1",
                 "TILO_UI_DASH_ALLOC_BUDGET_PER_1000": "1"}
UI_TIMING = {**NATIVE_4K, "TILO_UI_DASH_ITERS": "1000"}
UI_SAMPLES_ENV = "TILO_UI_DASH_SAMPLES"
REDRAW_SAMPLES_ENV = "TILO_REDRAW_BENCH_SAMPLES"
REDRAW_SAMPLE_COUNT = 1000

# A run measures the harness's defaults: no inherited benchmark switch or
# renderer optimisation override may reach it.
SCRUBBED_PREFIXES = ("TILO_UI_DASH_", "TILO_REDRAW_BENCH_", "TILO_DRM_XRGB_", "VELLO_")

UI_REDRAWS = re.compile(r"^redrawn events:\s+(\d+)\b", re.M)
UI_ADAPTER = re.compile(r"^UI renderer adapter: name=\"([^\"]*)\"", re.M)
REDRAW_ADAPTER = re.compile(r"^DRM bridge adapter: name=\"([^\"]*)\"", re.M)
REDRAW_GATE = re.compile(r"^gates: (mean<=\d+us p99<=\d+us) \((Pass|Fail)\)$", re.M)
# Counter lines the harness prints when a budget is set; each must appear, so a
# run that silently stopped checking one fails.
COUNTER_LINES = {
    "redraw": (re.compile(r"^raster-pixel-budget: PASS\b", re.M),
               re.compile(r"^allocation-call-redraw-budget: PASS\b", re.M),
               re.compile(r"^allocation-traffic-byte-redraw-budget: PASS\b", re.M)),
    "idle": (re.compile(r"^allocation-budget: PASS\b", re.M),),
}
VERIFY_PASS = re.compile(r"^damage-verify: PASS\b", re.M)


def bench_env(overrides: dict[str, str], base: dict[str, str] | None = None) -> dict[str, str]:
    env = {name: value for name, value in (os.environ if base is None else base).items()
           if not name.startswith(SCRUBBED_PREFIXES)}
    env.update(overrides)
    return env


def run(label: str, argv: list[str], overrides: dict[str, str], out: Path) -> tuple[int, str]:
    """Run one harness, keep its output in `out/<label>.log`, and echo it."""
    print(f"==> {label}: {' '.join(f'{k}={v}' for k, v in sorted(overrides.items()))} {' '.join(argv)}",
          flush=True)
    result = subprocess.run(argv, env=bench_env(overrides), stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, text=True, check=False)
    (out / f"{label}.log").write_text(result.stdout, encoding="utf-8")
    sys.stdout.write(result.stdout)
    print(f"<== {label}: exit {result.returncode}", flush=True)
    return result.returncode, result.stdout


def verify(ui_perf: str, out: Path) -> bool:
    status, output = run("verify", [ui_perf, "dashboard_perf_bench"], VERIFY, out)
    return status == 0 and VERIFY_PASS.search(output) is not None


def counter_failures(name: str, status: int, output: str) -> list[str]:
    """Why a counter run did not pass, or nothing: exit 0 and every budget line printed PASS."""
    failures = [] if status == 0 else [f"{name}: exit {status}"]
    failures += [f"{name}: no `{pattern.pattern.lstrip('^')}` line"
                 for pattern in COUNTER_LINES[name] if not pattern.search(output)]
    return failures


def counters(args: argparse.Namespace) -> int:
    args.out.mkdir(parents=True, exist_ok=True)
    failures = [] if verify(args.ui_perf, args.out) else ["verify: retained frames differ from full renders"]
    for name, overrides in (("redraw", REDRAW_COUNTERS), ("idle", IDLE_COUNTERS)):
        failures += counter_failures(name, *run(name, [args.ui_perf, "dashboard_perf_bench"], overrides, args.out))
    for failure in failures:
        print(f"::error::render counters: {failure}")
    return 1 if failures else 0


def read_samples(path: Path, expected: int | None, what: str) -> list[float]:
    if not path.is_file():
        raise ValueError(f"{what} wrote no samples to {path}")
    samples = record.load_samples(path)
    if not samples:
        raise ValueError(f"{what} wrote an empty samples file")
    if expected is not None and len(samples) != expected:
        raise ValueError(f"{what} wrote {len(samples)} samples, expected {expected}")
    return samples


def ui_frame_record(output: str, samples_file: Path, env: dict[str, Any]) -> record.Record:
    redraws = UI_REDRAWS.search(output)
    if redraws is None:
        raise ValueError("ui-perf printed no `redrawn events:` line")
    samples = read_samples(samples_file, int(redraws.group(1)), "ui-perf")
    adapter = UI_ADAPTER.search(output)
    meta = {**env.get("meta", {}), "events": UI_TIMING["TILO_UI_DASH_ITERS"], "redraws": redraws.group(1),
            "adapter": adapter.group(1) if adapter else "unknown", "verified_frames": VERIFY["TILO_UI_DASH_VERIFY_FRAMES"]}
    base = {name: value for name, value in env.items() if name != "meta"}
    return record.from_samples("render.ui.frame_us.4k", "us", samples, meta=meta, **base)


def redraw_record(output: str, samples_file: Path, env: dict[str, Any]) -> tuple[record.Record, bool]:
    """The shell redraw record, and whether the harness's own gate passed."""
    gate = REDRAW_GATE.search(output)
    if gate is None:
        raise ValueError("redraw_bench printed no `gates:` line")
    samples = read_samples(samples_file, REDRAW_SAMPLE_COUNT, "redraw_bench")
    adapter = REDRAW_ADAPTER.search(output)
    meta = {**env.get("meta", {}), "gate": f"{gate.group(1)} {gate.group(2)}",
            "adapter": adapter.group(1) if adapter else "unknown"}
    base = {name: value for name, value in env.items() if name != "meta"}
    return record.from_samples("render.shell.redraw_us.4k", "us", samples, meta=meta, **base), gate.group(2) == "Pass"


def timing(args: argparse.Namespace) -> int:
    args.out.mkdir(parents=True, exist_ok=True)
    env = record.load_env(args.env_json)
    if not verify(args.ui_perf, args.out):
        print("::error::render timing: retained frames differ from full renders; nothing is recorded")
        return 1
    ui_samples = args.out / "ui-frame-us.txt"
    redraw_samples = args.out / "shell-redraw-us.txt"
    for stale in (ui_samples, redraw_samples):
        stale.unlink(missing_ok=True)
    status = 0
    records: list[record.Record] = []
    try:
        ui_status, ui_output = run("ui-timing", [args.ui_perf, "dashboard_perf_bench"],
                                   {**UI_TIMING, UI_SAMPLES_ENV: str(ui_samples)}, args.out)
        if ui_status != 0:
            raise ValueError(f"ui-perf exited {ui_status}")
        records.append(ui_frame_record(ui_output, ui_samples, env))
        redraw_status, redraw_output = run("shell-redraw", [args.redraw_bench],
                                           {REDRAW_SAMPLES_ENV: str(redraw_samples)}, args.out)
        redraw, gate_passed = redraw_record(redraw_output, redraw_samples, env)
        records.append(redraw)
        if redraw_status != 0 and gate_passed:
            raise ValueError(f"redraw_bench exited {redraw_status} with its gate passed")
        if not gate_passed:
            # The harness gate is a hardware threshold; the record carries the
            # outcome and the lane's comparison judges the trend.
            print(f"::warning::redraw_bench's own gate failed on this renderer ({redraw.meta['gate']}); "
                  "the record is kept")
    except ValueError as error:
        print(f"::error::render timing: {error}")
        status = 1
    for item in records:
        print(f"{item.kpi}: n={item.n} p50={item.p50:.0f} p95={item.p95:.0f} p99={item.p99:.0f} "
              f"max={item.max:.0f} {item.unit}")
        if args.records is not None:
            record.write(args.records, item)
    return status


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    gate = commands.add_parser("counters", help="the per-change counter gate")
    gate.add_argument("ui_perf")
    gate.add_argument("--out", type=Path, default=REPO / "target/render-bench")
    lane = commands.add_parser("timing", help="frame-time records for the scheduled lane")
    lane.add_argument("ui_perf")
    lane.add_argument("redraw_bench")
    lane.add_argument("--env-json", type=Path, required=True, help="tools/perf/record.py env output")
    lane.add_argument("--out", type=Path, default=REPO / "target/render-bench")
    lane.add_argument("--records", type=Path, help="append the records to this JSON-lines file")
    args = parser.parse_args(argv)
    return counters(args) if args.command == "counters" else timing(args)


if __name__ == "__main__":
    sys.exit(main())
