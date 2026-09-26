#!/usr/bin/env python3
"""Record per-package test execution time from nextest JUnit reports.

affected_packages.py balances test shards with these numbers, so a shard that
holds the slow suites (shell, tilo-ui, ...) does not also receive half of the
remaining packages. The values only move packages between shards; they never
change which packages are selected or tested. Stale or missing entries are
harmless: an unknown package counts as zero seconds.

    python3 tools/ci/recorded_test_time.py --output tools/ci/recorded_test_time.json JUNIT...

Feed it the x86_64 nightly report (artifact nextest-junit-nightly-x86_64,
workspace.xml), which runs every workspace test.
"""
from __future__ import annotations

import argparse
from collections import Counter
import json
from pathlib import Path
import xml.etree.ElementTree as ET


def package_seconds(reports: list[Path]) -> dict[str, int]:
    totals: Counter[str] = Counter()
    for report in reports:
        for suite in ET.parse(report).getroot().iter("testsuite"):
            # nextest names suites `package` or `package::binary`.
            package = (suite.get("name") or "").split("::", 1)[0]
            if not package:
                raise ValueError(f"{report}: test suite without a name")
            for case in suite.iter("testcase"):
                totals[package] += float(case.get("time") or 0)
    return {name: round(seconds) for name, seconds in sorted(totals.items())
            if round(seconds) > 0}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0])
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("reports", type=Path, nargs="+")
    args = parser.parse_args()
    seconds = package_seconds(args.reports)
    args.output.write_text(json.dumps(seconds, indent=1, sort_keys=True) + "\n", encoding="utf-8")
    print(f"recorded test time for {len(seconds)} packages ({sum(seconds.values())} s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
