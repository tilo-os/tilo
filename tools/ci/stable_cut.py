#!/usr/bin/env python3
"""Cut stable from a green head of main, on t (docs/t.md, "Cutting stable").

    python3 tools/ci/stable_cut.py select            the candidate: main's newest head whose
                                                     default checks all pass
    python3 tools/ci/stable_cut.py checks            the checks a candidate is verified by
    python3 tools/ci/stable_cut.py verify <commit>   run them on t and wait for the verdict
    python3 tools/ci/stable_cut.py cut               all three, then the push that makes it stable

A candidate is verified in two sets:

- gating: the checks t.toml declares `heavy` (images, VM end-to-end, long suites), less those
  `t stable` excludes, and main's default checks that have no result for the candidate yet.
  Any failure holds the cut.
- reported: the performance checks (`REPORTED`, not `heavy`, run when stable moves). They run on
  the same candidate and their results print with the verdict, but they don't hold the cut:
  t's shared workers are loaded and run their VMs without a GPU, so a timing budget measured
  there says more about the worker than the commit. They gate again once they run on
  dedicated or GPU runners. vm-acceptance's own budget scenarios are reported the same way
  from inside the suite (`--budgets report`, scripts/vm-acceptance).

Nothing here pushes. `cut` prints the push for the stabiliser, the one who moves stable;
that push runs the publishing checks (`on = ["stable"]` in t.toml: sign-*, publish-*,
release-*, submit-*).
"""

from __future__ import annotations

import argparse
import fnmatch
import json
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))
import t_matrix  # noqa: E402

# Checks a candidate reports and does not gate on: performance measured on t's shared,
# GPU-less workers. Publishing steps among them (perf-publish) are left to the stable push.
REPORTED = ["perf-*", "vm-acceptance-perf", "weekly-latency"]


def t(*args: str) -> str:
    return subprocess.run(["t", *args, "--json"], cwd=ROOT, check=True, capture_output=True, text=True).stdout


def exclusions() -> set[str]:
    return {e["check"] for e in json.loads(t("stable")).get("exclusions", [])}


def excluded(name: str, excl: set[str]) -> bool:
    return any(name == e or fnmatch.fnmatchcase(name, e) for e in excl)


def split(excl: set[str], root: Path = ROOT) -> tuple[list[str], list[str]]:
    """(gating, reported): t.toml's heavy checks and its performance checks, matrices
    expanded, in declaration order, the excluded left out of both."""
    gating, reported = [], []
    for c in t_matrix.checks(root):
        name = c["name"]
        if excluded(name, excl):
            continue
        if c.get("heavy"):
            gating.append(name)
        elif (any(fnmatch.fnmatchcase(name, p) for p in REPORTED) and "stable" in c.get("on", [])
              and not c.get("secrets") and not c.get("id_token")):
            reported.append(name)
    return gating, reported


def select(commits: int, excl: set[str]) -> tuple[dict | None, list[dict]]:
    """The newest commit of main none of whose default checks fails (excluded checks aside),
    and each recent commit's standing, newest first. t runs main's default checks as it has
    room, so a commit has results for some of them only: those without one (`open`) are run
    with the candidate's verification."""
    board = json.loads(t("main", "--coverage", str(commits)))
    rows = []
    for c in board["commits"]:
        checks = {k: v for k, v in c["checks"].items() if not excluded(k, excl)}
        failing = sorted(k for k, v in checks.items() if v in ("fail", "error"))
        open_ = sorted(k for k, v in checks.items() if v in ("unknown", "running"))
        rows.append({"sha": c["sha"], "failing": failing, "open": open_})
    for row in rows:
        if not row["failing"]:
            return row, rows
    return None, rows


def follow(run: str) -> dict[str, dict]:
    """Wait for `run` to end; its results by check."""
    results: dict[str, dict] = {}
    while True:
        # `t follow` streams the run's results to its end; a client stopped early (a dropped
        # connection) loses nothing, so follow again until t says the verification ended.
        proc = subprocess.Popen(["t", "follow", run, "--json"], cwd=ROOT, stdout=subprocess.PIPE, text=True)
        assert proc.stdout
        for line in proc.stdout:
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if event.get("type") == "result":
                results[event["check"]] = event
        proc.wait()
        status = json.loads(t("stable"))
        if any(v["run"] == run and v.get("finished_ms") for v in status.get("verifications", [])):
            return results
        time.sleep(5)


def verify(sha: str, checks: list[str]) -> str:
    out = subprocess.run(["t", "stable", "verify", sha, *checks, "--human"], cwd=ROOT,
                         capture_output=True, text=True)
    line = (out.stdout + out.stderr).strip().splitlines()
    for word in " ".join(line).split():
        if word.startswith("r") and len(word) == 11 and all(ch in "0123456789abcdef" for ch in word[1:]):
            return word
    sys.exit(f"t stable verify did not start a run: {' '.join(line)}")


def verdict(results: dict[str, dict], checks: list[str]) -> tuple[list[str], list[str]]:
    failed = [c for c in checks if results.get(c, {}).get("outcome") not in ("pass", None)]
    missing = [c for c in checks if c not in results]
    return failed, missing


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("select")
    s.add_argument("--commits", type=int, default=50)
    sub.add_parser("checks")
    v = sub.add_parser("verify")
    v.add_argument("commit")
    c = sub.add_parser("cut")
    c.add_argument("--commits", type=int, default=50)
    c.add_argument("--commit", help="verify this commit instead of selecting one")
    args = ap.parse_args(argv)

    excl = exclusions()
    gating, reported = split(excl)
    if args.cmd == "checks":
        print("gating:   " + " ".join(gating))
        print("reported: " + " ".join(reported))
        return 0

    sha = getattr(args, "commit", None)
    open_defaults: list[str] = []
    if args.cmd in ("select", "cut") and not sha:
        row, rows = select(args.commits, excl)
        if args.cmd == "select" or not row:
            for r in rows[:10]:
                print(f"{r['sha'][:10]}  {len(r['failing'])} failing, {len(r['open'])} without a result"
                      + (f": {', '.join(r['failing'][:6])}" if r["failing"] else ""))
            print(f"candidate: {row['sha']}" if row else "no recent commit of main passes its default checks")
            return 0 if row else 1
        sha, open_defaults = row["sha"], row["open"]
    gating = gating + [c for c in open_defaults if c not in gating]

    started = time.monotonic()
    run = verify(sha, gating)
    print(f"verifying {sha[:10]}: {len(gating)} gating checks in run {run}", flush=True)
    results = follow(run)
    failed, missing = verdict(results, gating)
    report_run = verify(sha, reported) if reported else None
    reported_results = follow(report_run) if report_run else {}
    minutes = (time.monotonic() - started) / 60
    for name in reported:
        print(f"reported  {name}: {reported_results.get(name, {}).get('outcome', 'no result')}")
    if failed or missing:
        print(f"{sha[:10]} is not a stable candidate ({minutes:.0f} min): "
              f"failed {', '.join(failed) or 'none'}; no result {', '.join(missing) or 'none'}")
        return 1
    print(f"{sha[:10]} passed its gating checks in {minutes:.0f} min. To make it stable:")
    print(f"  git push origin {sha}:refs/heads/stable")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
