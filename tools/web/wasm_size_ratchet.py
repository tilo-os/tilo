#!/usr/bin/env python3
"""A shrink-only size budget for web bundles.

`tools/web/wasm-size-budget/` records, per bundle, the byte size its module
(`pkg/tilo_browser_runtime_bg.wasm`) may reach:

- `budget.toml` holds each bundle's base `bytes` and the `headroom` (a
  fraction); every `raises/<bundle>/*.toml` adds its own `bytes` to that
  bundle's budget. One file per raise, so changes that each grow a bundle
  never edit the same line;
- a budget is a measurement plus the headroom, rounded up to 64 KiB, so
  ordinary commits do not fail on a few bytes and real growth still does;
- `measure NAME BUNDLE_DIR` fails when the bundle is larger than its budget,
  naming the raise file to add, and prints the lower base budget to commit
  (with the raise files removed) when that is more than 1 % below the current
  budget;
- `check-budget --base REV` fails when a budget is larger than at REV, beyond
  what the raise files new in this change add. A raise records `bytes`, the
  app-sdk `run` that measured the bundle, the `measured` bytes and a `reason`.
"""

from __future__ import annotations

import argparse
import math
import pathlib
import subprocess
import sys
import tomllib

REPO = pathlib.Path(__file__).resolve().parents[2]
DIRECTORY = "tools/web/wasm-size-budget"
BUDGET = REPO / DIRECTORY / "budget.toml"
# The single-file layout this directory replaced, read at older revisions.
LEGACY = "tools/web/wasm-size-budget.toml"
MODULE = "pkg/tilo_browser_runtime_bg.wasm"
# A budget this much lower than the current one is worth committing.
SLACK = 0.01
ROUNDING = 64 * 1024


class Budget:
    def __init__(self, text: str, raises: dict[str, dict[str, str]] | None = None) -> None:
        self.text = text
        # bundle -> raise file name -> its TOML text
        self.raises = raises if raises is not None else {}

    @property
    def base(self) -> dict[str, int]:
        table = tomllib.loads(self.text).get("bundles", {})
        return {name: int(entry["bytes"]) for name, entry in table.items()}

    @property
    def headroom(self) -> float:
        return float(tomllib.loads(self.text).get("headroom", 0))

    def raise_records(self, bundle: str) -> dict[str, dict]:
        return {name: tomllib.loads(text) for name, text in self.raises.get(bundle, {}).items()}

    def totals(self) -> dict[str, int]:
        return {
            bundle: base + sum(int(record.get("bytes", 0)) for record in self.raise_records(bundle).values())
            for bundle, base in self.base.items()
        }


def load(root: pathlib.Path = REPO) -> Budget:
    directory = root / DIRECTORY
    raises: dict[str, dict[str, str]] = {}
    for path in sorted((directory / "raises").glob("*/*.toml")):
        raises.setdefault(path.parent.name, {})[path.name] = path.read_text()
    return Budget((directory / "budget.toml").read_text(), raises)


def load_at(revision: str, root: pathlib.Path = REPO) -> Budget | None:
    def git(*args: str) -> subprocess.CompletedProcess:
        return subprocess.run(["git", *args], cwd=root, capture_output=True, text=True, check=False)

    shown = git("show", f"{revision}:{DIRECTORY}/budget.toml")
    if shown.returncode != 0:
        legacy = git("show", f"{revision}:{LEGACY}")
        return Budget(legacy.stdout) if legacy.returncode == 0 else None
    budget = Budget(shown.stdout)
    listed = git("ls-tree", "-r", "--name-only", revision, f"{DIRECTORY}/raises/")
    for path in listed.stdout.split():
        parts = pathlib.PurePosixPath(path)
        if parts.suffix == ".toml":
            text = git("show", f"{revision}:{path}").stdout
            budget.raises.setdefault(parts.parent.name, {})[parts.name] = text
    return budget


def budget_for(size: int, headroom: float) -> int:
    return math.ceil(size * (1 + headroom) / ROUNDING) * ROUNDING


def measure(name: str, bundle: pathlib.Path, budget: Budget) -> tuple[int, str]:
    size = (bundle / MODULE).stat().st_size
    limits = budget.totals()
    proposed = budget_for(size, budget.headroom)
    if name not in limits:
        return 1, (f"{name}: {size} bytes; no budget yet, add [bundles.{name}] "
                   f"bytes = {proposed} to {DIRECTORY}/budget.toml")
    limit = limits[name]
    if size > limit:
        return 1, (
            f"{name}: {size} bytes is over its budget of {limit} (+{size - limit}); "
            f"a deliberate raise adds {DIRECTORY}/raises/{name}/<change>.toml with "
            f"bytes = {proposed - limit}, measured = {size}, run = <this run's number> "
            f"and a reason"
        )
    if proposed < limit * (1 - SLACK):
        return 0, (f"{name}: {size} bytes, under its budget of {limit}; lower it: "
                   f"[bundles.{name}] bytes = {proposed}, without its raise files")
    return 0, f"{name}: {size} bytes, within its budget of {limit}"


def raise_problems(bundle: str, name: str, record: dict, headroom: float) -> list[str]:
    valid = (
        isinstance(record.get("bytes"), int) and record["bytes"] > 0
        and isinstance(record.get("run"), int) and record["run"] > 0
        and isinstance(record.get("measured"), int) and record["measured"] > 0
        and isinstance(record.get("reason"), str) and len(record["reason"].strip()) >= 20
    )
    if not valid:
        return [f"{bundle}: raise {name} needs positive bytes, an app-sdk run number, "
                f"its measured bytes and a reason"]
    if record["bytes"] > budget_for(record["measured"], headroom):
        return [f"{bundle}: raise {name} adds more than its whole measurement"]
    return []


def increases(before: Budget, after: Budget) -> list[str]:
    old, new = before.totals(), after.totals()
    problems = []
    for bundle in sorted(new):
        added = {
            name: record for name, record in after.raise_records(bundle).items()
            if name not in before.raises.get(bundle, {})
        }
        for name, record in sorted(added.items()):
            problems += raise_problems(bundle, name, record, after.headroom)
        if bundle not in old or new[bundle] <= old[bundle]:
            continue
        allowance = sum(record["bytes"] for record in added.values() if isinstance(record.get("bytes"), int))
        if new[bundle] - old[bundle] > allowance:
            problems.append(f"{bundle}: budget raised from {old[bundle]} to {new[bundle]} "
                            f"beyond the {allowance} bytes its new raise files add")
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
    current = load()
    if args.command == "measure":
        status, message = measure(args.name, args.bundle, current)
        print(message, file=sys.stderr if status else sys.stdout)
        return status
    base = load_at(args.base)
    if base is None:
        print("no budget at the base revision; nothing to compare")
        return 0
    problems = increases(base, current)
    for problem in problems:
        print(f"error: {problem}; budgets may only decrease", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
