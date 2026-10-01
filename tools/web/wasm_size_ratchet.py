#!/usr/bin/env python3
"""A shrink-only size budget for web bundles.

`tools/web/wasm-size-budget.toml` records, per bundle, the byte size of its
module (`pkg/tilo_browser_runtime_bg.wasm`):

- a budget is a measurement plus the file's `headroom` (a fraction), rounded
  up to 64 KiB, so ordinary commits do not fail on a few bytes and real growth
  still does;
- `measure NAME BUNDLE_DIR` fails when the bundle is larger than its budget,
  and prints the lower budget to commit when that is more than 1 % below the
  current one;
- `check-budget --base REV` fails when a budget in the working tree is larger
  than at REV, so budgets can only go down. The one exception is a re-baseline:
  the raised entry records `rebaseline = { run = <app-sdk run number>,
  measured = <bytes that run measured>, reason = "..." }`, new in this change,
  and its bytes are at most the budget for that measurement.
"""

from __future__ import annotations

import argparse
import math
import pathlib
import subprocess
import sys
import tomllib

REPO = pathlib.Path(__file__).resolve().parents[2]
BUDGET = REPO / "tools/web/wasm-size-budget.toml"
MODULE = "pkg/tilo_browser_runtime_bg.wasm"
# A budget this much lower than the current one is worth committing.
SLACK = 0.01
ROUNDING = 64 * 1024


def budgets(text: str) -> dict[str, int]:
    table = tomllib.loads(text).get("bundles", {})
    return {name: int(entry["bytes"]) for name, entry in table.items()}


def headroom(text: str) -> float:
    return float(tomllib.loads(text).get("headroom", 0))


def budget_for(size: int, text: str) -> int:
    return math.ceil(size * (1 + headroom(text)) / ROUNDING) * ROUNDING


def measure(name: str, bundle: pathlib.Path, text: str) -> tuple[int, str]:
    size = (bundle / MODULE).stat().st_size
    limits = budgets(text)
    proposed = budget_for(size, text)
    if name not in limits:
        return 1, f"{name}: {size} bytes; no budget yet, add [bundles.{name}] bytes = {proposed}"
    limit = limits[name]
    if size > limit:
        return 1, (
            f"{name}: {size} bytes is over its budget of {limit} (+{size - limit}); "
            f"a deliberate raise records a rebaseline with measured = {size} "
            f"and bytes = {proposed}"
        )
    if proposed < limit * (1 - SLACK):
        return 0, f"{name}: {size} bytes, under its budget of {limit}; lower it to {proposed}"
    return 0, f"{name}: {size} bytes, within its budget of {limit}"


def rebaselines(text: str) -> dict[str, dict]:
    table = tomllib.loads(text).get("bundles", {})
    return {name: entry["rebaseline"] for name, entry in table.items() if "rebaseline" in entry}


def valid_rebaseline(record: object) -> bool:
    return (
        isinstance(record, dict)
        and isinstance(record.get("run"), int)
        and record["run"] > 0
        and isinstance(record.get("measured"), int)
        and record["measured"] > 0
        and isinstance(record.get("reason"), str)
        and len(record["reason"].strip()) >= 20
    )


def increases(before: str, after: str) -> list[str]:
    old, new = budgets(before), budgets(after)
    old_records, new_records = rebaselines(before), rebaselines(after)
    problems = []
    for name in sorted(old):
        if name not in new or new[name] <= old[name]:
            continue
        record = new_records.get(name)
        if (valid_rebaseline(record) and record != old_records.get(name)
                and new[name] <= budget_for(record["measured"], after)):
            continue
        problems.append(f"{name}: budget raised from {old[name]} to {new[name]}")
    for name, record in sorted(new_records.items()):
        if not valid_rebaseline(record):
            problems.append(
                f"{name}: rebaseline needs an app-sdk run number, its measured bytes and a reason")
    return problems + [f"{name}: budget removed" for name in sorted(set(old) - set(new))]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    measure_parser = commands.add_parser("measure")
    measure_parser.add_argument("name")
    measure_parser.add_argument("bundle", type=pathlib.Path)
    check = commands.add_parser("check-budget")
    check.add_argument("--base", required=True)
    args = parser.parse_args()
    current = BUDGET.read_text()
    if args.command == "measure":
        status, message = measure(args.name, args.bundle, current)
        print(message, file=sys.stderr if status else sys.stdout)
        return status
    shown = subprocess.run(
        ["git", "show", f"{args.base}:{BUDGET.relative_to(REPO).as_posix()}"],
        cwd=REPO,
        capture_output=True,
        text=True,
        check=False,
    )
    if shown.returncode != 0:
        print("no budget at the base revision; nothing to compare")
        return 0
    problems = increases(shown.stdout, current)
    for problem in problems:
        print(f"error: {problem}; budgets may only decrease", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
