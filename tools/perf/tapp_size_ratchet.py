#!/usr/bin/env python3
"""Per-app size budgets for the shipped tapps, and the fleet gate over them.

"Apps are ~2 MB" is a marketed claim. The KPI behind it ("Performance and cost
KPIs", Linear af384e878293, "App package size") is the bytes of the signed
`.tapp` a person downloads, per architecture: median <= 2 MB, p90 <= 4 MB and
max <= 6 MB, ratcheted per app. Two gates hold it:

- **Per app, when the tapp is packaged.** `measure DIR` reads what
  dist/apps/package-apps.sh wrote (`index.json` and `tapp/`): Release / apps
  runs it on the tapps it has just built and signed, which are the affected
  apps only. It fails when an app is over its budget or has none.
- **The fleet, nightly.** `fleet ENTRIES_JSON` reads the public registry's
  `entries` index (Perf / scheduled, footprint group). Each budgeted app's
  newest tapp is held to its budget, and the median, p90 and max of those
  apps are held to the fleet ceilings.

`check-budget --base REV` runs in CI / fast quality without building
anything. It fails when a budget is larger than at REV beyond what the raise
files new in the change add. It also fails when the budgeted apps are not
exactly the default collection, so a new app brings its budget in the same
pull request.

`tools/perf/tapp-size-budget/budget.toml` holds the budgets. Each budget is a
measurement plus `headroom`, rounded up to 64 KiB. `raises/<change>.toml`
adds bytes to apps or fleet ceilings, one file per change, so concurrent
changes never edit the same line. When an app shrinks, `measure` and `fleet`
print the lower budget to commit. DEVELOPMENT.md ("App package size") says how
to raise a budget.

The population is the default collection's apps: what Release / apps builds
and publishes as apps. The runtime package (`so.tilo.runtime`, its own KPI)
and the runtime and provider stubs are not apps. Packages the registry still
lists without a source in this repository are not counted either. `fleet`
names every package it leaves out.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import math
import os
import pathlib
import subprocess
import sys
import tomllib
from typing import Any

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parents[1]
sys.path.insert(0, str(HERE))

import record  # noqa: E402

DIRECTORY = "tools/perf/tapp-size-budget"
STATS = {"p50": 50.0, "p90": 90.0, "max": 100.0}
# A budget this much lower than the current one is worth committing.
SLACK = 0.01
ROUNDING = 64 * 1024
MIN_REASON = 20


def ints(table: Any) -> dict[str, int]:
    if not isinstance(table, dict):
        return {}
    return {key: value for key, value in table.items() if isinstance(value, int)}


class Budget:
    def __init__(self, text: str, raises: dict[str, str] | None = None) -> None:
        self.data = tomllib.loads(text)
        # raise file name -> its parsed TOML
        self.raises = {name: tomllib.loads(body) for name, body in (raises or {}).items()}

    @property
    def headroom(self) -> float:
        return float(self.data.get("headroom", 0))

    @property
    def architectures(self) -> list[str]:
        return [arch for arch in self.data.get("architectures", []) if isinstance(arch, str)]

    @property
    def targets(self) -> dict[str, int]:
        return {stat: value for stat, value in ints(self.data.get("fleet", {})).items() if stat in STATS}

    @property
    def interim(self) -> dict[str, dict[str, int]]:
        table = self.data.get("fleet", {}).get("interim", {})
        return {arch: ints(stats) for arch, stats in table.items()} if isinstance(table, dict) else {}

    def apps(self) -> set[str]:
        return set(self.data.get("apps", {}))

    def added(self, kind: str, names: set[str] | None = None) -> dict[tuple[str, str], int]:
        """Bytes the raise files (or only `names`) add, by (app or arch, arch or stat)."""
        sums: dict[tuple[str, str], int] = {}
        for name, raised in self.raises.items():
            if names is not None and name not in names:
                continue
            table = raised.get(kind, {})
            for outer, inner in (table.items() if isinstance(table, dict) else ()):
                for key, value in ints(inner).items():
                    sums[(outer, key)] = sums.get((outer, key), 0) + value
        return sums

    def app_totals(self) -> dict[tuple[str, str], int]:
        """Each (app, architecture) budget, its raises included."""
        totals = {(app, arch): value for app, table in self.data.get("apps", {}).items()
                  for arch, value in ints(table).items()}
        for key, value in self.added("apps").items():
            if key in totals:
                totals[key] += value
        return totals

    def fleet_ceilings(self) -> dict[tuple[str, str], int]:
        """Each (architecture, statistic) ceiling: the interim one where set, else the KPI target."""
        targets, interim = self.targets, self.interim
        ceilings = {(arch, stat): interim.get(arch, {}).get(stat, targets[stat])
                    for arch in self.architectures for stat in STATS if stat in targets}
        for key, value in self.added("fleet").items():
            if key in ceilings:
                ceilings[key] += value
        return ceilings


def git(root: pathlib.Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(["git", *args], cwd=root, capture_output=True, text=True, check=False)


def load(root: pathlib.Path = REPO) -> Budget:
    directory = root / DIRECTORY
    raises = {path.name: path.read_text() for path in sorted((directory / "raises").glob("*.toml"))}
    return Budget((directory / "budget.toml").read_text(), raises)


def load_at(revision: str, root: pathlib.Path = REPO) -> Budget | None:
    """The budget at `revision`, or None when the revision has none. An unknown revision fails."""
    if git(root, "rev-parse", "--verify", "--quiet", f"{revision}^{{commit}}").returncode != 0:
        raise ValueError(f"cannot read revision {revision}: fetch it before comparing budgets")
    shown = git(root, "show", f"{revision}:{DIRECTORY}/budget.toml")
    if shown.returncode != 0:
        return None
    raises = {}
    listed = git(root, "ls-tree", "--name-only", revision, f"{DIRECTORY}/raises/")
    for path in listed.stdout.split():
        if path.endswith(".toml"):
            raises[pathlib.PurePosixPath(path).name] = git(root, "show", f"{revision}:{path}").stdout
    return Budget(shown.stdout, raises)


def default_collection(root: pathlib.Path = REPO) -> set[str]:
    """Package ids of the default collection: what Release / apps publishes as apps."""
    spec = importlib.util.spec_from_file_location("app_release", root / "tools/apps/app_release.py")
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    catalog = module.app_catalog(root)
    names = module.resolve_collection("default", catalog, module.config(root / "dist/apps/app-collections.toml"))
    return {catalog[name]["id"] for name in names}


def budget_for(size: int, headroom: float) -> int:
    return math.ceil(size * (1 + headroom) / ROUNDING) * ROUNDING


def run_label() -> str:
    number = os.environ.get("GITHUB_RUN_NUMBER", "")
    return number if number.isdigit() else "<the Release / apps run number>"


def raise_hint(over: dict[str, int], arch: str) -> str:
    lines = [f"A deliberate growth adds one raise file for the change, {DIRECTORY}/raises/<change>.toml",
             "(the other architecture's job prints its own lines; DEVELOPMENT.md, \"App package size\"):",
             '    reason = "<why these apps grew, at least 20 characters>"',
             f"    run = {run_label()}"]
    for app, added in sorted(over.items()):
        lines += [f'    [apps."{app}"]', f"    {arch} = {added}"]
    return "\n".join(lines)


def judge_apps(sizes: dict[str, int], arch: str, budget: Budget,
               verbose: bool = True) -> tuple[int, list[str]]:
    """Each app's bytes against its budget on `arch`."""
    limits = budget.app_totals()
    lines: list[str] = []
    over: dict[str, int] = {}
    missing = False
    for app, size in sorted(sizes.items()):
        proposed = budget_for(size, budget.headroom)
        limit = limits.get((app, arch))
        if limit is None:
            missing = True
            lines.append(f'error: {app} {arch}: {size} bytes and no budget; add [apps."{app}"] '
                         f"{arch} = {proposed} to {DIRECTORY}/budget.toml")
        elif size > limit:
            over[app] = proposed - limit
            lines.append(f"error: {app} {arch}: {size} bytes is over its budget of {limit} (+{size - limit})")
        elif proposed < limit * (1 - SLACK):
            lines.append(f"{app} {arch}: {size} bytes, under its budget of {limit}; lower it: "
                         f'[apps."{app}"] {arch} = {proposed}, and drop the app from its raise files')
        elif verbose:
            lines.append(f"{app} {arch}: {size} bytes, within its budget of {limit}")
    if over:
        lines.append(raise_hint(over, arch))
    return (1 if over or missing else 0), lines


def measure(directory: pathlib.Path, budget: Budget, arch: str | None = None) -> tuple[int, list[str]]:
    """The tapps package-apps.sh wrote to `directory`, each against its budget."""
    index = json.loads((directory / "index.json").read_text(encoding="utf-8"))
    built = index.get("architecture", "")
    if arch is not None and built != arch:
        return 1, [f"error: {directory}/index.json is for {built!r}, not {arch}"]
    if built not in budget.architectures:
        return 1, [f"error: no budgets for architecture {built!r}"]
    apps = index.get("apps", [])
    if not apps:
        return 1, [f"error: {directory}/index.json lists no app, so nothing was measured"]
    sizes = {app["id"]: (directory / "tapp" / app["file"]).stat().st_size for app in apps}
    return judge_apps(sizes, built, budget)


def newest_tapps(entries: dict[str, Any]) -> dict[tuple[str, str], int]:
    """Bytes of the newest tilo-os tapp per (package, architecture) in a registry index."""
    newest: dict[tuple[str, str], dict[str, Any]] = {}
    for entry in entries.get("entries", []):
        for version in entry.get("versions", []):
            target = version.get("target", {})
            if target.get("platform") != "tilo-os":
                continue
            key = (entry["package_id"], target["architecture"])
            if key not in newest or version["released_at_unix"] > newest[key]["released_at_unix"]:
                newest[key] = version
    sizes = {}
    for key, version in newest.items():
        size = version["tapp_size_bytes"]
        if not isinstance(size, int) or size <= 0:
            raise ValueError(f"invalid tapp_size_bytes {size!r}")
        sizes[key] = size
    if not sizes:
        raise ValueError("the registry index lists no tilo-os tapp")
    return sizes


def judge_stat(arch: str, stat: str, value: int, budget: Budget) -> tuple[int, str]:
    ceiling = budget.fleet_ceilings()[(arch, stat)]
    target = budget.targets[stat]
    interim = budget.interim.get(arch, {}).get(stat)
    head = f"fleet {arch} {stat}: {value} bytes"
    if value > ceiling:
        return 1, (f"error: {head} is over its ceiling of {ceiling} (KPI target {target}); shrink the "
                   f"apps that grew, or a raise file adds [fleet.{arch}] {stat} = "
                   f"{budget_for(value, budget.headroom) - ceiling}")
    if value > target:
        lower = budget_for(value, budget.headroom)
        hint = (f"; lower it: [fleet.interim.{arch}] {stat} = {lower}, without its raise files"
                if lower < ceiling * (1 - SLACK) else "")
        return 0, f"{head}, KPI target {target} not met yet; held to {ceiling}{hint}"
    if interim is not None:
        return 0, f"{head} meets the KPI target {target}; remove [fleet.interim.{arch}] {stat}"
    return 0, f"{head}, within the KPI target {target}"


def fleet(entries: dict[str, Any], budget: Budget) -> tuple[int, list[str]]:
    """The published apps against their budgets, and the fleet against its ceilings."""
    published = newest_tapps(entries)
    population = budget.apps()
    status = 0
    lines = [f"registry sequence {entries.get('sequence', '?')}; {len(population)} budgeted apps"]
    outside = sorted({package for package, _ in published if package not in population})
    if outside:
        lines.append("not apps of the default collection, not counted: " + ", ".join(outside))
    for arch in budget.architectures:
        sizes = {package: size for (package, built), size in published.items()
                 if built == arch and package in population}
        if not sizes:
            status = 1
            lines.append(f"error: {arch}: the registry lists none of the budgeted apps")
            continue
        unpublished = sorted(population - set(sizes))
        if unpublished:
            lines.append(f"{arch}: not published yet, not counted: " + ", ".join(unpublished))
        app_status, app_lines = judge_apps(sizes, arch, budget, verbose=False)
        status |= app_status
        lines += app_lines
        ordered = sorted(float(size) for size in sizes.values())
        lines.append(f"fleet {arch}: {len(ordered)} apps, {int(sum(ordered))} bytes in all")
        for stat, percentile in STATS.items():
            stat_status, line = judge_stat(arch, stat, int(record.nearest_rank(ordered, percentile)), budget)
            status |= stat_status
            lines.append(line)
    return status, lines


def shape_problems(budget: Budget) -> list[str]:
    problems = []
    arches = budget.architectures
    if not arches:
        problems.append("budget.toml names no architectures")
    if set(budget.targets) != set(STATS) or any(value <= 0 for value in budget.targets.values()):
        problems.append(f"[fleet] needs positive {', '.join(STATS)} KPI targets in bytes")
    for app, table in sorted(budget.data.get("apps", {}).items()):
        values = ints(table)
        if set(table) != set(arches) or len(values) != len(table) or any(v <= 0 for v in values.values()):
            problems.append(f'[apps."{app}"] needs a positive byte budget for each of {", ".join(arches)}')
    for arch, stats in sorted(budget.data.get("fleet", {}).get("interim", {}).items()):
        if arch not in arches or not set(stats) <= set(STATS) or len(ints(stats)) != len(stats):
            problems.append(f"[fleet.interim.{arch}] names an unknown architecture or statistic")
        for stat, value in ints(stats).items():
            if stat in budget.targets and value <= budget.targets[stat]:
                problems.append(f"[fleet.interim.{arch}] {stat} is not above the KPI target; remove it")
    return problems


def raise_problems(name: str, raised: dict[str, Any], budget: Budget) -> list[str]:
    problems = []
    reason, run = raised.get("reason"), raised.get("run")
    if not (isinstance(reason, str) and len(reason.strip()) >= MIN_REASON
            and isinstance(run, int) and not isinstance(run, bool) and run > 0):
        problems.append(f"raise {name} needs a reason of at least {MIN_REASON} characters "
                        "and the run number that measured the growth")
    unknown = set(raised) - {"reason", "run", "apps", "fleet"}
    if unknown:
        problems.append(f"raise {name} has unknown keys {sorted(unknown)}")
    totals, targets = budget.app_totals(), budget.targets
    adds = 0
    for kind, valid in (("apps", lambda outer, key: (outer, key) in totals),
                        ("fleet", lambda outer, key: outer in budget.architectures and key in STATS)):
        table = raised.get(kind, {})
        for outer, inner in (table.items() if isinstance(table, dict) else ()):
            for key, value in (inner.items() if isinstance(inner, dict) else ()):
                adds += 1
                if not valid(outer, key):
                    problems.append(f"raise {name}: [{kind}.\"{outer}\"] {key} names nothing with a budget")
                elif not isinstance(value, int) or value <= 0:
                    problems.append(f"raise {name}: [{kind}.\"{outer}\"] {key} must add a positive byte count")
                else:
                    base = (budget.data["apps"][outer][key] if kind == "apps" else targets.get(key, 0))
                    if value > base:
                        problems.append(f"raise {name}: [{kind}.\"{outer}\"] {key} more than doubles its budget")
    if adds == 0:
        problems.append(f"raise {name} adds nothing")
    return problems


def increases(before: Budget, after: Budget, new_raises: set[str]) -> list[str]:
    problems = []
    if after.headroom > before.headroom:
        problems.append("headroom raised")
    for stat, value in after.targets.items():
        if value > before.targets.get(stat, value):
            problems.append(f"KPI target {stat} raised; the KPI document sets it")
    for kind, old, new in (("apps", before.app_totals(), after.app_totals()),
                           ("fleet", before.fleet_ceilings(), after.fleet_ceilings())):
        allowance = after.added(kind, new_raises)
        for key, total in sorted(new.items()):
            label = f'[apps."{key[0]}"] {key[1]}' if kind == "apps" else f"fleet {key[0]} {key[1]}"
            if key in old:
                if total - old[key] > allowance.get(key, 0):
                    problems.append(f"{label}: raised from {old[key]} to {total} beyond the "
                                    f"{allowance.get(key, 0)} bytes its new raise files add")
            elif kind == "apps" and total > after.targets.get("max", 0):
                problems.append(f"{label}: a new app starts at most at the KPI max target, "
                                f"{after.targets.get('max', 0)} bytes")
    return problems


def check(before: Budget | None, after: Budget, collection: set[str] | None) -> list[str]:
    problems = shape_problems(after)
    new_raises = set(after.raises) - set(before.raises if before is not None else {})
    for name in sorted(new_raises):
        problems += raise_problems(name, after.raises[name], after)
    if collection is not None:
        apps = after.apps()
        for app in sorted(collection - apps):
            problems.append(f'{app} is in the default collection without a budget; add [apps."{app}"] '
                            f"(a new app may start at the KPI max target, "
                            f"{after.targets.get('max', 0)} bytes, until it is measured)")
        for app in sorted(apps - collection):
            problems.append(f"{app} has a budget but is not in the default collection; remove it")
    if before is not None:
        problems += increases(before, after, new_raises)
    return problems


def seed(entries: dict[str, Any], budget: Budget, collection: set[str]) -> list[str]:
    """[apps] tables for the default collection from a registry index, today's sizes plus headroom."""
    published = newest_tapps(entries)
    lines = [f"# Seeded from registry sequence {entries.get('sequence', '?')}."]
    for app in sorted(collection):
        lines += ["", f'[apps."{app}"]']
        for arch in budget.architectures:
            size = published.get((app, arch))
            value = budget_for(size, budget.headroom) if size else budget.targets.get("max", 0)
            note = "" if size else "  # not published yet: the KPI max target"
            lines.append(f"{arch} = {value}{note}")
    return lines


def emit(status: int, lines: list[str], summary: pathlib.Path | None) -> int:
    for line in lines:
        print(line, file=sys.stderr if line.startswith("error:") else sys.stdout)
    if summary is not None:
        with summary.open("a", encoding="utf-8") as handle:
            handle.write("```\n" + "\n".join(lines) + "\n```\n")
    return status


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    measured = commands.add_parser("measure", help="tapps package-apps.sh wrote, against their budgets")
    measured.add_argument("directory", type=pathlib.Path)
    measured.add_argument("--arch")
    measured.add_argument("--summary", type=pathlib.Path)
    fleet_parser = commands.add_parser("fleet", help="the registry index against the budgets and fleet ceilings")
    fleet_parser.add_argument("entries", type=pathlib.Path)
    fleet_parser.add_argument("--summary", type=pathlib.Path)
    checked = commands.add_parser("check-budget", help="budgets only decrease without a raise file")
    checked.add_argument("--base", required=True)
    seeded = commands.add_parser("seed", help="print [apps] tables from a registry index")
    seeded.add_argument("entries", type=pathlib.Path)
    args = parser.parse_args(argv)
    try:
        current = load()
        if args.command == "measure":
            return emit(*measure(args.directory, current, args.arch), args.summary)
        if args.command == "fleet":
            return emit(*fleet(json.loads(args.entries.read_text(encoding="utf-8")), current), args.summary)
        if args.command == "seed":
            entries = json.loads(args.entries.read_text(encoding="utf-8"))
            return emit(0, seed(entries, current, default_collection()), None)
        problems = check(load_at(args.base), current, default_collection())
    except (ValueError, KeyError, OSError, tomllib.TOMLDecodeError) as error:
        print(f"tapp_size_ratchet.py: {error}", file=sys.stderr)
        return 1
    for problem in problems:
        print(f"error: {problem}", file=sys.stderr)
    if not problems:
        print(f"{len(current.apps())} app budgets checked against {args.base}")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
