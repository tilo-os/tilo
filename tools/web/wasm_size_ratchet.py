#!/usr/bin/env python3
"""A shrink-only size budget for web bundles.

`tools/web/wasm-size-budget.toml` records, per bundle, the byte size of its
module (`pkg/tilo_browser_runtime_bg.wasm`):

- `measure NAME BUNDLE_DIR` fails when the bundle is larger than its budget,
  and prints the lower budget to commit when it shrank by more than 1 %;
- `check-budget --base REV` fails when a budget in the working tree is larger
  than at REV, so budgets can only go down.
"""

from __future__ import annotations

import argparse
import pathlib
import subprocess
import sys
import tomllib

REPO = pathlib.Path(__file__).resolve().parents[2]
BUDGET = REPO / "tools/web/wasm-size-budget.toml"
MODULE = "pkg/tilo_browser_runtime_bg.wasm"
# Shrinking by more than this asks for a lower budget.
SLACK = 0.01


def budgets(text: str) -> dict[str, int]:
    table = tomllib.loads(text).get("bundles", {})
    return {name: int(entry["bytes"]) for name, entry in table.items()}


def measure(name: str, bundle: pathlib.Path, text: str) -> tuple[int, str]:
    size = (bundle / MODULE).stat().st_size
    limits = budgets(text)
    if name not in limits:
        return 1, f"{name}: {size} bytes; no budget yet, add [bundles.{name}] bytes = {size}"
    limit = limits[name]
    if size > limit:
        return 1, f"{name}: {size} bytes is over its budget of {limit} (+{size - limit})"
    if size < limit * (1 - SLACK):
        return 0, f"{name}: {size} bytes, under its budget of {limit}; lower it to {size}"
    return 0, f"{name}: {size} bytes, within its budget of {limit}"


def increases(before: str, after: str) -> list[str]:
    old, new = budgets(before), budgets(after)
    return [
        f"{name}: budget raised from {old[name]} to {new[name]}"
        for name in sorted(old)
        if name in new and new[name] > old[name]
    ] + [f"{name}: budget removed" for name in sorted(set(old) - set(new))]


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
