#!/usr/bin/env python3
"""Compare this run's performance records with the store and with budgets.

For every record of the run:

- **Baseline.** The last N (default 5) stored records with the same KPI and
  environment key (profile, renderer, virtualization, runner), excluding
  records that were themselves regressions, and starting no earlier than the
  newest record a person accepted. Records from another environment are never
  compared. A baseline is compared with once it holds the KPI's
  `baseline_min_runs` records (default 1), or starts at an accepted one; until
  then the run is `new` and joins the baseline. A deterministic KPI (bytes) is
  judged by one run; a noisy one needs more, or its first quiet night becomes
  the bar every later night is held to.
- **Relative regression.** p50 above `p50_tolerance` (1.10) or p99 above
  `p99_tolerance` (1.20) times the baseline median of that percentile, and by
  more than the KPI's `min_delta`. A first regression is `regression-pending`
  and passes; a regression right after a pending or failing one is
  `regression` and fails. The state lives in the store: each published record
  carries its verdict, so the next run reads it.
- **Absolute budgets.** `tools/perf/kpi-budgets.toml` ceilings on p50, p95,
  p99 or max. An `enforce = true` miss fails at once; otherwise the miss is
  reported as a target not yet met.
- **Missing results.** A KPI with a budget in a selected group that produced
  no record fails, and so does a selected group with no record at all: a
  benchmark that stopped reporting is not a pass.

`--accept KPI[,KPI]` (or `all`) records the run's values as the new baseline
start for an intended change; budgets still apply. `--synthetic SERIES
--synthetic-factor F` multiplies the run's values by F and tags them, which
proves the gate end to end: synthetic records never enter a real baseline and
only see the previous record of their own series.

    python3 tools/perf/compare.py --results results/*.jsonl \
        --store https://code.tilo.so --groups fs,footprint \
        --publish-dir out --run "$RUN_ID-$ATTEMPT" --summary summary.md

The exit status is 1 when any record fails, after the annotated records and
the summary are written, so the store keeps the state of a failing night.
"""

from __future__ import annotations

import argparse
import dataclasses
import datetime
import statistics
import sys
import tomllib
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

import record  # noqa: E402
import store  # noqa: E402

BUDGETS = Path(__file__).resolve().parent / "kpi-budgets.toml"
CEILINGS = ("p50", "p95", "p99", "max")
REGRESSED = frozenset({"regression-pending", "regression"})
FAILING = frozenset({"regression", "unit-mismatch"})
DEFAULTS = {"p50_tolerance": 1.10, "p99_tolerance": 1.20, "baseline_runs": 5,
            "baseline_min_runs": 1, "min_delta": 0.0}


@dataclasses.dataclass
class Budgets:
    defaults: dict[str, float]
    kpis: dict[str, dict[str, Any]]

    def setting(self, kpi: str, name: str) -> float:
        return float(self.kpis.get(kpi, {}).get(name, self.defaults[name]))


def load_budgets(path: Path) -> Budgets:
    data = tomllib.loads(path.read_text(encoding="utf-8"))
    defaults = {**DEFAULTS, **data.get("defaults", {})}
    kpis = data.get("kpi", {})
    allowed = {"unit", "enforce", "source", *CEILINGS, *DEFAULTS}
    for kpi, entry in kpis.items():
        if not record.KPI.fullmatch(kpi):
            raise ValueError(f"{path}: budget KPI {kpi!r} is not a KPI id")
        unknown = set(entry) - allowed
        if unknown:
            raise ValueError(f"{path}: {kpi} has unknown keys {sorted(unknown)}")
        if entry.get("unit") not in record.UNITS:
            raise ValueError(f"{path}: {kpi} needs a unit from {sorted(record.UNITS)}")
        if not any(name in entry for name in CEILINGS):
            raise ValueError(f"{path}: {kpi} names no ceiling ({', '.join(CEILINGS)})")
    return Budgets(defaults, kpis)


@dataclasses.dataclass
class Row:
    record: record.Record | None
    kpi: str
    status: str
    failing: bool
    note: str = ""


def series(item: record.Record) -> str | None:
    return item.meta.get("synthetic_series")


def baseline(history: list[record.Record], runs: int) -> list[record.Record]:
    real = [item for item in history if series(item) is None]
    accepted = [index for index, item in enumerate(real) if (item.verdict or {}).get("status") == "accepted"]
    if accepted:
        real = real[accepted[-1]:]
    return [item for item in real if (item.verdict or {}).get("status") not in REGRESSED][-runs:]


def comparable(base: list[record.Record], min_runs: int) -> bool:
    """Enough runs for a median, or a baseline a person started on purpose."""
    accepted = any((item.verdict or {}).get("status") == "accepted" for item in base)
    return len(base) >= min_runs or accepted


def number(value: float | None) -> str:
    if value is None:
        return ""
    return f"{value:,.0f}" if abs(value) >= 10_000 else f"{value:.4g}"


def budget_misses(item: record.Record, entry: dict[str, Any]) -> list[str]:
    return [f"{name} {number(getattr(item, name))} > {number(float(entry[name]))} {item.unit}"
            for name in CEILINGS if name in entry and getattr(item, name) > float(entry[name])]


def judge(item: record.Record, history: list[record.Record], budgets: Budgets, accept: bool) -> Row:
    """Annotate one record with its verdict and say whether it fails the run."""
    matching = sorted((h for h in history if h.kpi == item.kpi and h.env_key == item.env_key),
                      key=lambda h: h.started_at)
    verdict: dict[str, Any] = {}
    item.verdict = verdict
    if any(h.unit != item.unit for h in matching):
        verdict["status"] = "unit-mismatch"
        return Row(item, item.kpi, "unit-mismatch", True, "the store holds this KPI in another unit")
    base = baseline(matching, int(budgets.setting(item.kpi, "baseline_runs")))
    own = [h for h in matching if series(h) == series(item)]
    previous = (own[-1].verdict or {}).get("status") if own else None
    status = "new"
    notes = []
    if series(item) is not None:
        factor = item.meta.get("synthetic_factor", "?")
        notes.append(f"synthetic series {series(item)}: values x{factor}, never a real baseline")
    min_runs = int(budgets.setting(item.kpi, "baseline_min_runs"))
    if base and not comparable(base, min_runs):
        verdict.update(baseline_n=len(base))
        notes.append(f"baseline {len(base)} of {min_runs} runs; not compared yet")
    elif base:
        base50 = statistics.median(h.p50 for h in base)
        base99 = statistics.median(h.p99 for h in base)
        min_delta = budgets.setting(item.kpi, "min_delta")
        verdict.update(baseline_n=len(base), baseline_p50=base50, baseline_p99=base99)
        regressed = (
            (item.p50 > base50 * budgets.setting(item.kpi, "p50_tolerance") and item.p50 - base50 > min_delta)
            or (item.p99 > base99 * budgets.setting(item.kpi, "p99_tolerance") and item.p99 - base99 > min_delta))
        status = "pass"
        if regressed:
            status = "regression" if previous in REGRESSED else "regression-pending"
    if accept:
        status = "accepted"
    verdict["status"] = status
    entry = budgets.kpis.get(item.kpi)
    failing = status in FAILING
    if entry is not None:
        if entry["unit"] != item.unit:
            verdict["status"] = "unit-mismatch"
            return Row(item, item.kpi, "unit-mismatch", True, f"budget unit is {entry['unit']}")
        misses = budget_misses(item, entry)
        if misses:
            enforced = bool(entry.get("enforce", False))
            verdict["budget_misses"] = misses
            verdict["budget_enforced"] = enforced
            failing = failing or enforced
            notes.append(("over budget: " if enforced else "over target: ") + "; ".join(misses))
    if status == "regression-pending":
        notes.append("first regression; fails if the next run regresses too")
    return Row(item, item.kpi, status, failing, "; ".join(notes))


def compare(current: list[record.Record], history: list[record.Record], budgets: Budgets,
            groups: set[str], accept: set[str]) -> list[Row]:
    seen = {item.kpi for item in current}
    if len(seen) != len(current):
        raise ValueError("the run holds two records for one KPI")
    rows = [judge(item, history, budgets, "all" in accept or item.kpi in accept) for item in current]
    produced = {item.group for item in current}
    for group in sorted(groups - produced):
        rows.append(Row(None, f"{group}.*", "missing", True, f"group {group} produced no record"))
    for kpi in sorted(budgets.kpis):
        if kpi.split(".", 1)[0] in groups & produced and kpi not in seen:
            rows.append(Row(None, kpi, "missing", True, "a budgeted KPI produced no record"))
    return rows


def inject(current: list[record.Record], name: str, factor: float) -> None:
    if factor <= 0:
        raise ValueError("--synthetic-factor must be positive")
    for item in current:
        item.p50, item.p95, item.p99, item.max = (value * factor for value in
                                                  (item.p50, item.p95, item.p99, item.max))
        item.meta = {**item.meta, "synthetic_series": name, "synthetic_factor": f"{factor:g}"}


def summary(rows: list[Row]) -> str:
    lines = ["| KPI | environment | n | p50 | p99 | baseline p50 | baseline p99 | status | note |",
             "|---|---|---|---|---|---|---|---|---|"]
    for row in rows:
        item = row.record
        if item is None:
            lines.append(f"| `{row.kpi}` | | | | | | | **{row.status}** | {row.note} |")
            continue
        verdict = item.verdict or {}
        status = f"**{row.status}**" if row.failing else row.status
        lines.append(
            f"| `{item.kpi}` | {'/'.join(item.env_key)} | {item.n} | {number(item.p50)} {item.unit} "
            f"| {number(item.p99)} {item.unit} | {number(verdict.get('baseline_p50'))} "
            f"| {number(verdict.get('baseline_p99'))} | {status} | {row.note} |")
    failing = sum(row.failing for row in rows)
    lines.append("")
    lines.append(f"{len(rows)} results, {failing} failing.")
    return "\n".join(lines) + "\n"


def publish(current: list[record.Record], directory: Path, run: str) -> Path:
    started = min(item.started_at for item in current)
    month = started[:7]
    path = directory / month / store.file_name(started, run)
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        raise ValueError(f"{path} already exists; store files are immutable")
    path.write_text("".join(item.to_json() + "\n" for item in current), encoding="utf-8")
    return path


def main(argv: list[str] | None = None, now: datetime.datetime | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--results", nargs="+", type=Path, required=True)
    parser.add_argument("--store", required=True, help="a forge URL or a local store directory")
    parser.add_argument("--owner", default="tilo")
    parser.add_argument("--budgets", type=Path, default=BUDGETS)
    parser.add_argument("--groups", default="", help="comma-separated KPI groups this run was meant to produce")
    parser.add_argument("--accept", default="", help="comma-separated KPI ids (or all) to accept as the new baseline")
    parser.add_argument("--synthetic", default="", help="tag the run as this synthetic series")
    parser.add_argument("--synthetic-factor", type=float, default=1.0)
    parser.add_argument("--publish-dir", type=Path)
    parser.add_argument("--run", default="local")
    parser.add_argument("--summary", type=Path)
    args = parser.parse_args(argv)
    try:
        current = [item for path in args.results for item in record.read(path)]
        if args.synthetic_factor != 1.0 and not args.synthetic:
            raise ValueError("--synthetic-factor needs --synthetic SERIES")
        if args.synthetic:
            inject(current, args.synthetic, args.synthetic_factor)
        months = store.months_back(now or datetime.datetime.now(datetime.timezone.utc))
        history = store.history(store.open_store(args.store, args.owner), months)
        groups = {group for group in args.groups.split(",") if group}
        accept = {kpi for kpi in args.accept.split(",") if kpi}
        rows = compare(current, history, load_budgets(args.budgets), groups, accept)
    except ValueError as error:
        print(f"compare.py: {error}", file=sys.stderr)
        return 1
    text = summary(rows)
    print(text, end="")
    if args.summary:
        args.summary.write_text(text, encoding="utf-8")
    if args.publish_dir and current:
        print(f"published {publish(current, args.publish_dir, args.run)}")
    return 1 if any(row.failing for row in rows) else 0


if __name__ == "__main__":
    sys.exit(main())
