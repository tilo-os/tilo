#!/usr/bin/env python3
"""Judge how fast an installed image's apps started, for gates outside run.py.

    ./os/dev/vm.sh ssh "$(python3 scripts/vm-acceptance/app_startup_check.py --probe)" \\
        | python3 scripts/vm-acceptance/app_startup_check.py [--scale F] [--report-only] [--require APP]

`--probe` prints the guest script (run it as the session's user after the apps were
launched); without it the script's output is read from stdin. Prints one row per app that
started (slowest first) and fails when any launch's first frame, or any History opening,
came later than its budget times `--scale`. `--report-only` prints the same and never
fails, for a machine too slow to judge (an emulated CPU). `--require APP` fails when APP
logged no startup at all. Exit 0 when every launch is within budget, 1 otherwise.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance import app_startup  # noqa: E402


def main(argv: list[str]) -> int:
    if argv[1:] == ["--probe"]:
        print(app_startup.PROBE)
        return 0
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--scale", type=float, default=1.0,
                        help="multiply every budget, for slower hardware")
    parser.add_argument("--report-only", action="store_true")
    parser.add_argument("--require", action="append", default=[], metavar="APP")
    args = parser.parse_args(argv[1:])
    log = sys.stdin.read()
    startups = app_startup.parse_startups(log)
    opens = app_startup.parse_history_opens(log)
    print("\n".join(app_startup.table(startups)))
    for launch in startups:
        print(launch["line"])
    for opened in opens:
        print(f"{opened['app']}: History opened in {opened['ms']} ms "
              f"({opened['listed']} changes listed, earlier ones {'paged' if opened['earlier'] else 'none'})")
    problems = app_startup.over_budget(startups, args.scale)
    problems += app_startup.history_over_budget(opens, args.scale)
    launched = {launch["app"] for launch in startups}
    problems += [f"{app} logged no APP-STARTUP line" for app in args.require if app not in launched]
    if not startups:
        problems.append("no app logged an APP-STARTUP line")
    if not problems:
        print(f"App startup: {len(startups)} launches of {len(launched)} apps within budget")
        return 0
    level = "notice" if args.report_only else "error"
    for problem in problems:
        print(f"::{level}::App startup: {problem}")
    return 0 if args.report_only else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
