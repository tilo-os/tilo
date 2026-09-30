#!/usr/bin/env python3
"""Record per-package test execution time from nextest JUnit reports.

affected_packages.py balances test shards with these numbers, so a shard that
holds the slow suites (shell, tilo-ui, ...) does not also receive half of the
remaining packages. The values only move packages between shards; they never
change which packages are selected or tested. Stale entries are harmless and a
package without an entry gets affected_packages.UNRECORDED_TEST_SECONDS.

    python3 tools/ci/recorded_test_time.py --output tools/ci/recorded_test_time.json RUN...

Each RUN is one CI run: a JUnit file, or a directory whose *.xml reports (all
shards of a check run, or the nightly workspace/visual-goldens/ignored reports)
are added up. Download the artifacts with `fgj run artifacts N -d NAME` from
recent main runs of check.yml (nextest-junit-x86_64-*) and ci-nightly.yml
(nextest-junit-nightly-x86_64), one directory per run. A package's time is the
median over the runs that executed its whole suite; a scoped run that picked
only a few of its tests (under 90% of the most test cases seen for it) is not
a sample.
"""
from __future__ import annotations

import argparse
from collections import Counter
import json
from pathlib import Path
from statistics import median
import xml.etree.ElementTree as ET

# A run that executed fewer of a package's test cases than this share of the
# largest count seen for it selected only part of the suite.
FULL_SUITE_SHARE = 0.9


def run_reports(run: Path) -> list[Path]:
    if run.is_dir():
        reports = sorted(run.rglob("*.xml"))
        if not reports:
            raise ValueError(f"{run}: no JUnit reports")
        return reports
    return [run]


def run_totals(run: Path) -> dict[str, tuple[float, int]]:
    """Seconds and test-case count per package, summed over one run's reports."""
    seconds: Counter[str] = Counter()
    cases: Counter[str] = Counter()
    for report in run_reports(run):
        for suite in ET.parse(report).getroot().iter("testsuite"):
            # nextest names suites `package` or `package::binary`.
            package = (suite.get("name") or "").split("::", 1)[0]
            if not package:
                raise ValueError(f"{report}: test suite without a name")
            for case in suite.iter("testcase"):
                seconds[package] += float(case.get("time") or 0)
                cases[package] += 1
    return {name: (seconds[name], cases[name]) for name in cases}


def package_seconds(runs: list[Path]) -> dict[str, int]:
    samples: dict[str, list[tuple[float, int]]] = {}
    for run in runs:
        for name, sample in run_totals(run).items():
            samples.setdefault(name, []).append(sample)
    result = {}
    for name, values in sorted(samples.items()):
        most = max(count for _, count in values)
        full = [seconds for seconds, count in values if count >= FULL_SUITE_SHARE * most]
        seconds = round(median(full))
        if seconds > 0:
            result[name] = seconds
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0])
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("runs", type=Path, nargs="+",
                        help="one JUnit file or directory of reports per CI run")
    args = parser.parse_args()
    seconds = package_seconds(args.runs)
    args.output.write_text(json.dumps(seconds, indent=1, sort_keys=True) + "\n", encoding="utf-8")
    print(f"recorded test time for {len(seconds)} packages ({sum(seconds.values())} s) "
          f"from {len(args.runs)} runs")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
