#!/usr/bin/env python3
"""What the pre-push hook (.githooks/pre-push) runs on the commit being pushed.

Main is where changes integrate and it is allowed to be red for a while; `stable` is what
continuous deployment ships, and it moves only on green (docs/t.md, "How changes reach a
release"). So a push runs only what is cheap to catch and annoying to break, in seconds:

  * `fmt-changed`: rustfmt of the Rust files the change touches;
  * the `policy/*` steps whose inputs the change touches (the policy suite's own tests,
    `policy/tests/*`, only when the change touches tools/policy/);
  * the generated-file and manifest checks whose inputs it touches (lockfile, deny, hakari,
    t-toml, codemap, sdk-policy, workspace-layout, workspace-unify-x86_64);
  * `changes`, the diff gates (allow reasons, commit policy, interfaces);
  * clippy of the packages whose files it changes, not of their dependents;
  * the aarch64 size check of each app whose files it changes, when t has that commit's
    shared runtime built (about a minute); skipped, said and recorded when it has not.

No tests, VM checks, images, suites or dependents, and no size check of an app the change
does not touch: main's CI runs those on every commit or its newest head and tells the commit
that broke one, and `t test <check>` runs any of them on request.

A check red on main runs all the same: t compares what fails with the same check at the commit
the push is based on, and only what fails there too passes. A lint (`clippy/*`) and a policy
step's problem (`policy/*`, tools/policy/findings.py) are compared one finding at a time, so a
push that adds one is refused however red main is. A check that fails as a whole (a compile
error, a generated file out of date) can only be compared as a whole: it passes when main fails
it too, and the hook says which passed that way and why. One that runs tests, or whose failure
on main named its findings, is refused instead: what fails can't be told from main's.

  pre_push.py select --scope SCOPE.json --changed CHANGED.txt
      SCOPE.json is `t scope --rev <commit> --json`, CHANGED.txt the paths the push changes, one
      per line. Prints a JSON object: `run` (the checks to name to `t test`), `left` (in
      scope, but not run before a push) and `sizes` (the size checks of the apps it touches).
  pre_push.py sizes --top DIR --rev COMMIT --record FILE CHECK...
      Of those size checks, the ones to run: all when t has the commit's shared runtime built,
      else none, the skip said on stderr and appended to FILE.
  pre_push.py opt-in --suggest SUGGEST.json
      The opt-in checks `t suggest --json` says the change touches, space-separated.
  pre_push.py unpassed --run RUN.jsonl CHECK...
      Every check of the gate (CHECK...) that the run (`t test --json`) did not pass, one per
      line with why, and t's own errors: what a refused push names, and what a push passed
      although it fails, since main fails it too.
"""
from __future__ import annotations

import argparse
import datetime
import json
from pathlib import Path
import subprocess
import sys
import threading
import tomllib

# The repository this script belongs to: where the changed paths are relative to.
ROOT = Path(__file__).resolve().parents[2]

# Always run: neither is in `t scope` by its inputs alone. fmt-changed is not a default check
# (main runs the whole tree's `fmt`), and changes compares the commit with its base.
ALWAYS = ("fmt-changed", "changes")
# Whole-tree checks of generated files, manifests and repository policy, each seconds long and
# keyed by narrow inputs, so `t scope` names them only when the change touches what they read.
GATES = frozenset({
    "lockfile", "deny", "hakari", "t-toml", "codemap", "sdk-policy", "workspace-layout",
    "workspace-unify-x86_64",
})
# Replaced before a push by fmt-changed.
REPLACED = frozenset({"fmt"})
POLICY = "policy/"
POLICY_TESTS = "policy/tests/"
POLICY_SOURCES = "tools/policy/"
CLIPPY = "clippy/"


def package_of(path: str, root: Path) -> str | None:
    """The Cargo package a changed path belongs to: the nearest directory above it with a
    `Cargo.toml` that declares a `[package]`. None outside every package (the workspace manifest
    alone, docs, tools)."""
    directory = (root / path).parent
    while directory != root and root in directory.parents:
        manifest = directory / "Cargo.toml"
        if manifest.is_file():
            try:
                name = tomllib.loads(manifest.read_text(encoding="utf-8")).get("package", {}).get("name")
            except (OSError, tomllib.TOMLDecodeError):
                return None
            return name if isinstance(name, str) else None
        directory = directory.parent
    return None


def wanted(check: str, changed: list[str], packages: set[str]) -> bool:
    """Whether a check `t scope` names runs before a push. `t scope` names the lint of every
    package whose key the change moves, its dependents' too; only those of the packages whose
    files it changes (`packages`) run."""
    if check in GATES or check in ALWAYS:
        return True
    if check.startswith(POLICY_TESTS):
        return any(path.startswith(POLICY_SOURCES) for path in changed)
    if check.startswith(CLIPPY):
        return check.removeprefix(CLIPPY) in packages
    return check.startswith(POLICY)


def select(scope: dict, changed: list[str], root: Path = ROOT) -> dict[str, list[str]]:
    named = list(dict.fromkeys(scope.get("checks", [])))
    packages = {package for path in changed if (package := package_of(path, root))}
    run, left = [], []
    for check in [*ALWAYS, *named]:
        if check in run or check in left:
            continue
        if check in REPLACED or not wanted(check, changed, packages):
            left.append(check)
        else:
            run.append(check)
    return {"run": run, "left": left, "sizes": app_sizes(changed, root)}


# An app's own change is held to its size budget before it lands: `app-size/<app>-aarch64` of
# each app whose files the change touches (aarch64 is where budgets have been crossed). Its
# check compiles only the app, on the shared runtime `thin-runtime-aarch64` builds, which main
# keeps built for its newest head (`idle`); with the runtime built it takes about a minute.
# A change the runtime is built from (the toolkit, the SDK, the manifests) needs that runtime
# built afresh, 5 minutes and more, and a change to more than SIZE_APPS apps is a shared one:
# the check is skipped then, said so, and main's whole-collection gate (`app-sizes-<arch>`,
# `idle`) holds the change to the budgets. Each push's decision, checked or skipped, is
# recorded in the clone (.git/t-gate-sizes) to be counted.
SIZE_ARCH = "aarch64"
SIZE_RUNTIME = f"thin-runtime-{SIZE_ARCH}"
# A change to more apps than this is a shared one (a toolkit sweep), left to main's
# whole-collection gate like a change to the toolkit itself.
SIZE_APPS = 3
# How long the hook waits for t to say whether the runtime is built.
SIZE_PROBE_SECONDS = 60


def app_sizes(changed: list[str], root: Path = ROOT) -> list[str]:
    """The size checks of the apps whose files the change touches: `apps/<app>/` with a
    `tilo-app.toml`, as t.toml's `app-size/<app>-<arch>` matrix names them."""
    apps = {parts[1] for path in changed
            if len(parts := path.split("/")) > 2 and parts[0] == "apps"
            and (root / "apps" / parts[1] / "tilo-app.toml").is_file()}
    return [f"app-size/{app}-{SIZE_ARCH}" for app in sorted(apps)]


def all_cached(planned: dict | None) -> bool:
    """Whether a run's `planned` event says every check it names is cached."""
    if not planned:
        return False
    return bool(planned.get("checks")) and planned.get("cached") == len(planned["checks"])


def runtime_built(top: str, rev: str) -> bool:
    """Whether t has the shared runtime of `rev` built: a run of it is cached at once. One that
    is not is cancelled; a run t does not plan within SIZE_PROBE_SECONDS counts as not built."""
    started = subprocess.run(["t", "test", "-C", top, "--rev", rev, "--priority", "high", "--detach",
                              "--json", SIZE_RUNTIME], capture_output=True, text=True, check=False,
                             timeout=SIZE_PROBE_SECONDS)
    try:
        run = json.loads(started.stdout).get("run")
    except ValueError:
        return False
    if not run:
        return False
    follow = subprocess.Popen(["t", "follow", "-C", top, "--json", run], stdout=subprocess.PIPE,
                              stderr=subprocess.DEVNULL, text=True)
    timer = threading.Timer(SIZE_PROBE_SECONDS, follow.kill)
    timer.start()
    planned = None
    try:
        for line in follow.stdout:
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if isinstance(event, dict) and event.get("type") == "planned":
                planned = event
                break
    finally:
        timer.cancel()
        follow.kill()
        follow.wait()
    if all_cached(planned):
        return True
    subprocess.run(["t", "cancel", run], capture_output=True, check=False, timeout=SIZE_PROBE_SECONDS)
    return False


def sizes(top: str, rev: str, checks: list[str], record: Path) -> list[str]:
    """The size checks to run before the push, each push's decision appended to `record`, one
    line each, so how many pushes are checked and how many skip can be counted: all of them
    when the change touches at most SIZE_APPS apps and t has its shared runtime built, else
    none, the skip said on stderr."""
    if not checks:
        return []
    if len(checks) > SIZE_APPS:
        outcome = "shared-change"
        why = (f"it touches {len(checks)} apps, a shared change like one to the toolkit, which main's "
               f"whole-collection size gate (app-sizes-{SIZE_ARCH}) holds to the budgets")
    elif runtime_built(top, rev):
        outcome, why = "checked", ""
    else:
        outcome = "runtime-not-built"
        why = (f"the shared runtime of this commit is not built yet ({SIZE_RUNTIME}), so main's "
               f"whole-collection size gate (app-sizes-{SIZE_ARCH}) holds it to the budgets")
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    with record.open("a", encoding="utf-8") as handle:
        handle.write(f"{stamp} {rev[:10]} {outcome} {' '.join(checks)}\n")
    if outcome == "checked":
        return checks
    print(f"pre-push: skipped {' '.join(checks)}: {why}", file=sys.stderr)
    return []


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


def excerpt(result: dict) -> str:
    """The first line t gives of why a check did not pass."""
    for failure in result.get("failures") or []:
        for line in str(failure.get("excerpt") or "").splitlines():
            if line.strip():
                return line.strip()
    return ""


def blame_note(result: dict) -> str:
    """Whose a failure is, as t blamed it, and how it was compared with main's."""
    blame = (result.get("blame") or {}).get("kind")
    if blame == "flaky":
        return " (flaky)"
    if blame == "uncompared":
        return (" (it failed as a whole, so what fails can't be compared with main's one by one: its tests did "
                "not run, or main's failure named findings this one doesn't; main failing too doesn't make it main's)")
    failures = result.get("failures") or []
    findings = [f for f in failures if f.get("test")]
    if blame == "inherited":
        if not findings or len(findings) != len(failures):
            return " (main fails it too, as a whole: it found nothing it names, so it can't be compared finding by finding)"
        if any((f.get("blame") or {}).get("kind") != "inherited" for f in findings):
            return (f" (main fails it too, as a whole: main's result names no findings to compare its "
                    f"{len(findings)} with one by one)")
        return f" (main fails it too: each of its {len(findings)} findings, compared one by one)"
    def named(kind: str) -> str:
        names = [f["test"] for f in findings if (f.get("blame") or {}).get("kind") == kind]
        more = f", and {len(names) - 3} more" if len(names) > 3 else ""
        return "; ".join(names[:3]) + more
    if named("new"):
        return f" (not on main: {named('new')})"
    if named("unknown"):
        return f" (main has no result at the base naming its findings, so these may be the change's: {named('unknown')})"
    return ""


def unpassed(events: list[dict], asked: list[str]) -> list[str]:
    """Each check of `asked` the run did not pass, with why, and each error t reported. A refusal
    names every one of these: an outcome nobody prints is a refusal nobody can act on."""
    planned: list[str] = []
    results: dict[str, dict] = {}
    lines: list[str] = []
    for event in events:
        kind = event.get("type")
        if kind == "planned":
            planned = list(event.get("checks") or [])
        elif kind == "result" and event.get("check"):
            results[event["check"]] = event
        elif kind == "error":
            hint = f" ({event['hint']})" if event.get("hint") else ""
            lines.append(f"t failed, not a check: {event.get('message', '')}{hint}")
    for check in dict.fromkeys([*asked, *planned, *results]):
        result = results.get(check)
        if result is None:
            lines.append(f"{check}: no result; the run ended before t reported one, a failure of t, not of the change")
            continue
        outcome = result.get("outcome")
        why = excerpt(result)
        if outcome == "pass":
            continue
        if outcome == "fail":
            text = f"{check}: fails{blame_note(result)}"
        elif outcome == "error":
            text = f"{check}: could not run, a failure of t's infrastructure, not of the change"
        elif outcome == "cancelled":
            text = f"{check}: cancelled before it finished"
        elif outcome == "skipped":
            text = f"{check}: not run, since a check it is gated on failed"
        else:
            text = f"{check}: {outcome or 'no outcome'}, which this hook can't tell apart from a failure"
        lines.append(f"{text}: {why}" if why else text)
    return lines


def load_events(path: Path) -> list[dict]:
    events = []
    text = path.read_text(encoding="utf-8", errors="replace") if path.is_file() else ""
    for line in text.splitlines():
        try:
            event = json.loads(line)
        except ValueError:
            continue
        if isinstance(event, dict):
            events.append(event)
    return events


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
    chosen.add_argument("--changed", type=Path, required=True)
    suggested = sub.add_parser("opt-in")
    suggested.add_argument("--suggest", type=Path, required=True)
    verdict = sub.add_parser("unpassed")
    verdict.add_argument("--run", type=Path, required=True)
    verdict.add_argument("checks", nargs="*")
    sized = sub.add_parser("sizes")
    sized.add_argument("--top", required=True)
    sized.add_argument("--rev", required=True)
    sized.add_argument("--record", type=Path, required=True)
    sized.add_argument("checks", nargs="*")
    args = parser.parse_args(argv)
    if args.command == "unpassed":
        for line in unpassed(load_events(args.run), args.checks):
            print(line)
        return 0
    if args.command == "sizes":
        print(" ".join(sizes(args.top, args.rev, args.checks, args.record)))
        return 0
    if args.command == "opt-in":
        print(opt_in_line(opt_in(load(args.suggest))))
        return 0
    changed = [line for line in args.changed.read_text(encoding="utf-8").splitlines() if line]
    print(json.dumps(select(load(args.scope), changed)))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
