#!/usr/bin/env python3
"""What the pre-push hook (.githooks/pre-push) runs on the commit being pushed.

Main is where changes integrate and it is allowed to be red for a while; `stable` is what
continuous deployment ships, and it moves only on green (docs/t.md, "How changes reach a
release"). So a push runs only what is cheap to catch and annoying to break, in seconds:

  * `fmt-changed`: rustfmt of the Rust files the change touches;
  * the `policy/*` steps whose inputs the change touches (the policy suite's own tests,
    `policy/tests/*`, only when the change touches tools/policy/);
  * the generated-file and manifest checks whose inputs it touches (lockfile, deny, hakari,
    t-toml, codemap, sdk-policy, workspace-layout);
  * `changes`, the diff gates (allow reasons, commit policy, user-requirement tests,
    interfaces);
  * clippy of the packages whose files it changes, not of their dependents.

No tests, VM checks, images, suites or dependents: main's CI runs those on every commit and
tells the commit that broke one, and `t test <check>` runs any of them on request. A check that
is red on main at the commit the push is based on is skipped, since the push can't be what broke
it; the hook says which.

  pre_push.py select --scope SCOPE.json --main MAIN.json --changed CHANGED.txt
      SCOPE.json is `t scope --rev <commit> --json`, MAIN.json `t main --json` (or empty when t
      couldn't say), CHANGED.txt the paths the push changes, one per line. Prints a JSON object:
      `run` (the checks to name to `t test`), `red` (in scope, but red on main at the base) and
      `left` (in scope, but not run before a push).
  pre_push.py opt-in --suggest SUGGEST.json
      The opt-in checks `t suggest --json` says the change touches, space-separated.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import sys

# Always run: neither is in `t scope` by its inputs alone. fmt-changed is not a default check
# (main runs the whole tree's `fmt`), and changes compares the commit with its base.
ALWAYS = ("fmt-changed", "changes")
# Whole-tree checks of generated files, manifests and repository policy, each seconds long and
# keyed by narrow inputs, so `t scope` names them only when the change touches what they read.
GATES = frozenset({"lockfile", "deny", "hakari", "t-toml", "codemap", "sdk-policy", "workspace-layout"})
# Replaced before a push by fmt-changed.
REPLACED = frozenset({"fmt"})
POLICY = "policy/"
POLICY_TESTS = "policy/tests/"
POLICY_SOURCES = "tools/policy/"
CLIPPY = "clippy/"


def wanted(check: str, changed: list[str]) -> bool:
    """Whether a check `t scope` names runs before a push."""
    if check in GATES or check in ALWAYS:
        return True
    if check.startswith(POLICY_TESTS):
        return any(path.startswith(POLICY_SOURCES) for path in changed)
    return check.startswith(POLICY) or check.startswith(CLIPPY)


def red_at(board: dict, base: str, is_ancestor) -> set[str]:
    """The checks `t main` has red as a whole (not one test of them) since a commit that `base`
    contains: they fail at the base already, so the push can't be what breaks them."""
    red: set[str] = set()
    for item in board.get("items", []):
        if item.get("test") or item.get("excluded") or not item.get("first_bad"):
            continue
        if is_ancestor(item["first_bad"], base):
            red.update(item.get("checks") or [item["check"]])
    return red


def select(scope: dict, board: dict, changed: list[str], is_ancestor) -> dict[str, list[str]]:
    named = list(dict.fromkeys(scope.get("checks", [])))
    red = red_at(board, scope.get("base", ""), is_ancestor) if scope.get("base") else set()
    run, skipped, left = [], [], []
    for check in [*ALWAYS, *named]:
        if check in run or check in skipped or check in left:
            continue
        if check in REPLACED or not wanted(check, changed):
            left.append(check)
        elif check in red:
            skipped.append(check)
        else:
            run.append(check)
    return {"run": run, "red": skipped, "left": left}


def git_is_ancestor(commit: str, base: str) -> bool:
    """Whether `commit` is `base` or one of its ancestors, by the local repository."""
    return subprocess.run(["git", "merge-base", "--is-ancestor", commit, base],
                          capture_output=True).returncode == 0


def opt_in(suggest: dict) -> list[str]:
    """The opt-in checks `t suggest` says the change touches (its reason starts "opt-in")."""
    return [entry["check"] for entry in suggest.get("checks", [])
            if str(entry.get("reason", "")).startswith("opt-in") and entry["check"] not in ALWAYS]


# The opt-in checks the hook names on its one line; `t suggest` lists the rest.
SHOWN = 8


def opt_in_line(names: list[str]) -> str:
    """The hook's line about them: what to run, and how many more there are."""
    if not names:
        return ""
    more = f" (and {len(names) - SHOWN} more: t suggest)" if len(names) > SHOWN else ""
    return f"not run before a push; main runs them, or run them yourself: t test {' '.join(names[:SHOWN])}{more}"


def load(path: Path) -> dict:
    text = path.read_text(encoding="utf-8").strip() if path.is_file() else ""
    try:
        return json.loads(text) if text else {}
    except ValueError:
        return {}


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    chosen = sub.add_parser("select")
    chosen.add_argument("--scope", type=Path, required=True)
    chosen.add_argument("--main", type=Path, required=True)
    chosen.add_argument("--changed", type=Path, required=True)
    suggested = sub.add_parser("opt-in")
    suggested.add_argument("--suggest", type=Path, required=True)
    args = parser.parse_args(argv)
    if args.command == "opt-in":
        print(opt_in_line(opt_in(load(args.suggest))))
        return 0
    changed = [line for line in args.changed.read_text(encoding="utf-8").splitlines() if line]
    print(json.dumps(select(load(args.scope), load(args.main), changed, git_is_ancestor)))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
