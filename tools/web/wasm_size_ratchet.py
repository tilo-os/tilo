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

The shipped bundle (what the web entry at tilo.coffee serves) has a second
measurement, `measure-shipped NAME BUNDLE_DIR`: `[shipped.<name>]` budgets
`start` (the module the page loads first) and `brotli` (its sidecar), then
`chunks` (every other module the split made, total), `chunks_brotli` (their
sidecars), and `largest`/`largest_brotli` (the biggest of them), and each is
checked and ratcheted the same way. See budget.toml for which artifact is
which.
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
# The shipped bundle's measurements, in the order they are reported.
SHIPPED = ("start", "brotli", "chunks", "chunks_brotli", "largest", "largest_brotli")
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

    @property
    def shipped(self) -> dict[str, dict[str, int]]:
        table = tomllib.loads(self.text).get("shipped", {})
        return {
            name: {part: int(entry[part]) for part in SHIPPED if part in entry}
            for name, entry in table.items()
        }

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
        return subprocess.run(["git", "-C", str(root), *args], capture_output=True, text=True, check=False)

    resolved = git("rev-parse", "--verify", "--end-of-options", f"{revision}^{{commit}}")
    if resolved.returncode:
        raise RuntimeError(f"cannot resolve budget base {revision}: {resolved.stderr.strip()}")
    revision = resolved.stdout.strip()
    listed = git("ls-tree", "-r", "--name-only", revision, "--", DIRECTORY, LEGACY)
    if listed.returncode:
        raise RuntimeError(f"cannot list budget base: {listed.stderr.strip()}")
    paths = set(listed.stdout.splitlines())

    def read(path: str) -> str:
        result = git("show", f"{revision}:{path}")
        if result.returncode:
            raise RuntimeError(f"cannot read budget base {path}: {result.stderr.strip()}")
        return result.stdout

    shown_path = f"{DIRECTORY}/budget.toml"
    if shown_path not in paths:
        return Budget(read(LEGACY)) if LEGACY in paths else None
    budget = Budget(read(shown_path))
    for path in sorted(paths):
        if not path.startswith(f"{DIRECTORY}/raises/"):
            continue
        parts = pathlib.PurePosixPath(path)
        if parts.suffix == ".toml":
            text = read(path)
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


def measure_shipped(name: str, bundle: pathlib.Path, budget: Budget) -> tuple[int, str]:
    """The shipped bundle's parts against `[shipped.<name>]`, each on its own.

    A part over its budget fails; a part well under prints the lower budget to
    commit. Each module is weighed twice, as the Containerfile leaves it: raw,
    and with its brotli sidecar, which is what a browser accepting br downloads.
    """
    modules = sorted((bundle / "pkg").glob("*.wasm"))
    if not (bundle / MODULE).is_file():
        return 1, f"{name}: no {MODULE} in the bundle at {bundle}"
    chunks = [module for module in modules if module.name != pathlib.Path(MODULE).name]

    def sidecar(module: pathlib.Path) -> int:
        # The Containerfile compresses files over 1 KiB only; one left under it
        # has no sidecar and Caddy serves it as it is, so its brotli cost is
        # its own size.
        baked = module.parent / f"{module.name}.br"
        return baked.stat().st_size if baked.is_file() else module.stat().st_size
    brotli = [sidecar(module) for module in chunks]
    measured = {
        "start": (bundle / MODULE).stat().st_size,
        "brotli": sidecar(bundle / MODULE),
        "chunks": sum(module.stat().st_size for module in chunks),
        "chunks_brotli": sum(brotli),
        "largest": max((module.stat().st_size for module in chunks), default=0),
        "largest_brotli": max(brotli, default=0),
    }
    limits = budget.shipped.get(name)
    if limits is None:
        entry = "\n".join(f"{part} = {budget_for(size, budget.headroom)}" for part, size in measured.items())
        return 1, (f"{name}: no shipped budget yet, add [shipped.{name}] to "
                   f"{DIRECTORY}/budget.toml:\n{entry}")
    status, lines = 0, []
    for part, size in measured.items():
        limit = limits.get(part)
        proposed = budget_for(size, budget.headroom)
        if limit is None:
            status = 1
            lines.append(f"{name}.{part}: {size} bytes; no budget for it, add {part} = {proposed} "
                         f"to [shipped.{name}]")
        elif size > limit:
            status = 1
            lines.append(
                f"{name}.{part}: {size} bytes is over its budget of {limit} (+{size - limit}); "
                f"a deliberate raise adds {DIRECTORY}/raises/{name}/<change>.toml with "
                f"bytes = {proposed - limit}, measured = {size}, run = <this run's number> "
                f"and a reason")
        elif proposed < limit * (1 - SLACK):
            lines.append(f"{name}.{part}: {size} bytes, under its budget of {limit}; lower it: "
                         f"{part} = {proposed} in [shipped.{name}], without its raise files")
        else:
            lines.append(f"{name}.{part}: {size} bytes, within its budget of {limit}")
    return status, "\n".join(lines)


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


def shipped_increases(before: Budget, after: Budget) -> list[str]:
    old, new = before.shipped, after.shipped
    problems = []
    for bundle in sorted(new):
        added = sum(record.get("bytes", 0) for name, record in after.raise_records(bundle).items()
                    if name not in before.raises.get(bundle, {}) and isinstance(record.get("bytes"), int))
        for part, limit in sorted(new[bundle].items()):
            if bundle not in old or part not in old[bundle] or limit <= old[bundle][part]:
                continue
            if limit - old[bundle][part] > added:
                problems.append(f"shipped.{bundle}.{part}: budget raised from {old[bundle][part]} to "
                                f"{limit} beyond the {added} bytes its new raise files add")
        problems += [f"shipped.{bundle}.{part}: budget removed"
                     for part in sorted(set(old.get(bundle, {})) - set(new[bundle]))]
    problems += [f"shipped.{bundle}: budget removed" for bundle in sorted(set(old) - set(new))]
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    measure_parser = commands.add_parser("measure")
    measure_parser.add_argument("name")
    measure_parser.add_argument("bundle", type=pathlib.Path)
    shipped_parser = commands.add_parser("measure-shipped")
    shipped_parser.add_argument("name")
    shipped_parser.add_argument("bundle", type=pathlib.Path)
    check = commands.add_parser("check-budget")
    check.add_argument("--base", required=True)
    args = parser.parse_args()
    current = load()
    if args.command in ("measure", "measure-shipped"):
        measured = measure if args.command == "measure" else measure_shipped
        status, message = measured(args.name, args.bundle, current)
        print(message, file=sys.stderr if status else sys.stdout)
        return status
    try:
        base = load_at(args.base)
    except RuntimeError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    if base is None:
        print("no budget at the base revision; nothing to compare")
        return 0
    problems = increases(base, current) + shipped_increases(base, current)
    for problem in problems:
        print(f"error: {problem}; budgets may only decrease", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
