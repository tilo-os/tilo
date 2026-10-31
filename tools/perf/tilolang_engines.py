#!/usr/bin/env python3
"""Compare the engines that run tilolang apps' wasm: tiered (what ships), wasmi, Cranelift, Winch.

Builds `tilolang bench` (the `bench` example of tilolang-tilo) and the image
filter library, runs the bench once per
engine over the apps in apps/tl with the compute and library workloads, and
prints one Markdown table of the p50s, the engines side by side
(docs/tilolang-engines.md). Benchmarks are timing-sensitive: run nothing
heavy beside it.

Usage:

    python3 tools/perf/tilolang_engines.py [--engines tiered,wasmi,cranelift,winch]
        [--apps notes,todo] [--sizes 1000,10000] [--samples 10]
        [--records out.jsonl] [--no-build]
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BENCH = ROOT / "target/release/examples/bench"
LIBRARY_DIR = ROOT / "crates/platform/tilolang-tilo/bench/imagefilter"
LIBRARY = ROOT / "target/wasm32-unknown-unknown/release/tilolang_bench_imagefilter.wasm"
APPS = ROOT / "apps/tl"


def build() -> None:
    """The bench in release, and the image filter library."""
    command = ["cargo", "build", "--release", "--locked", "-p", "tilolang-tilo", "--example", "bench"]
    subprocess.run(command, cwd=ROOT, check=True)
    subprocess.run([str(LIBRARY_DIR / "build.sh")], check=True)


def run(engine: str, apps: list[str], sizes: str, samples: int, sink) -> list[dict]:
    records = []
    for index, app in enumerate(apps):
        command = [
            str(BENCH), str(APPS / app), "--engine", engine, "--sizes", sizes,
            "--samples", str(samples), "--idle", "0",
        ]
        if index == 0:
            command += ["--compute", "--library", str(LIBRARY)]
        print(f"{engine}: {app}", file=sys.stderr)
        out = subprocess.run(command, cwd=ROOT, check=True, capture_output=True, text=True).stdout
        for line in out.splitlines():
            if line.startswith("{"):
                record = json.loads(line)
                records.append(record)
                if sink:
                    sink.write(line + "\n")
    return records


ROWS = [
    # (label, kpi prefix, unit shown)
    ("compile", "tilolang.compile.", "ms"),
    ("compile, peak memory", "tilolang.compile_peak.", "KiB"),
    ("start to first frame", "tilolang.start.", "ms"),
    ("start to first frame, compiled module kept", "tilolang.start_cached.", "ms"),
    ("open", "tilolang.open.", "ms"),
    ("keystroke", "tilolang.keystroke.", "ms"),
    ("press", "tilolang.press.", "ms"),
    ("peer edit", "tilolang.peer_edit.", "ms"),
    ("send in a thread", "tilolang.thread_send.", "ms"),
    ("compute: guest calls (2^20 indices)", "tilolang.compute_sweep", "ms"),
    ("compute: loops over a list (2^20 steps)", "tilolang.compute_loops", "ms"),
    ("library: 3x3 blur, 512x512, 4 passes", "tilolang.library_blur", "ms"),
]


def fmt(value: float) -> str:
    if value >= 100:
        return f"{value:.0f}"
    if value >= 1:
        return f"{value:.2f}"
    return f"{value:.3f}"


def table(by_engine: dict[str, list[dict]]) -> str:
    engines = list(by_engine)
    lines = [
        "| measure | " + " | ".join(engines) + " |",
        "|---|" + "---:|" * len(engines),
    ]
    for label, prefix, unit in ROWS:
        kpis: list[str] = []
        for records in by_engine.values():
            for record in records:
                if record["kpi"].startswith(prefix) and record["kpi"] not in kpis:
                    kpis.append(record["kpi"])
        for kpi in kpis:
            suffix = kpi[len(prefix):].strip(".")
            cells = []
            for engine in engines:
                found = [r for r in by_engine[engine] if r["kpi"] == kpi]
                cells.append(fmt(found[0]["p50"]) if found else "")
            name = f"{label} {suffix}".strip()
            lines.append(f"| {name} ({unit}) | " + " | ".join(cells) + " |")
    results = []
    for kpi, key in [("tilolang.compute_sweep", "result"), ("tilolang.compute_loops", "result"),
                     ("tilolang.library_blur", "checksum")]:
        values = {e: next((r["meta"].get(key) for r in rs if r["kpi"] == kpi), None)
                  for e, rs in by_engine.items()}
        same = len({json.dumps(v) for v in values.values()}) == 1
        results.append(f"{kpi.removeprefix('tilolang.')}: {'same on every engine' if same else values}")
    text = "\n".join(lines)
    return text + "\n\nResults: " + "; ".join(results) + "."


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--engines", default="tiered,wasmi,cranelift,winch")
    parser.add_argument("--apps", default=",".join(sorted(p.name for p in APPS.iterdir() if p.is_dir())))
    parser.add_argument("--sizes", default="1000,10000")
    parser.add_argument("--samples", type=int, default=10)
    parser.add_argument("--records", type=Path, help="also write every record here, as JSON lines")
    parser.add_argument("--no-build", action="store_true", help="use the bench and library already built")
    args = parser.parse_args()
    if not args.no_build:
        build()
    for path in (BENCH, LIBRARY):
        if not path.is_file():
            sys.exit(f"{path} is missing; run without --no-build")
    sink = args.records.open("w") if args.records else None
    by_engine = {
        engine: run(engine, args.apps.split(","), args.sizes, args.samples, sink)
        for engine in args.engines.split(",")
    }
    if sink:
        sink.close()
    print(table(by_engine))
    return 0


if __name__ == "__main__":
    sys.exit(main())
