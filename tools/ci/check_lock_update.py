#!/usr/bin/env python3
"""Admit a staged lock-only change only when it is the declared `cargo update`.

Cargo.lock is committed and never regenerated (AGENTS.md). A change to it with
no Cargo.toml beside it is refused, except for the one sanctioned way to move a
dependency on its own: `cargo update -p <crate> [--precise <version>]`. The
committer declares that command in ``TILO_CARGO_UPDATE`` for the commit:

    TILO_CARGO_UPDATE='-p rustls --precise 0.23.31' git commit
    TILO_CARGO_UPDATE='cargo update -p rustls --precise 0.23.31; -p ring' git commit

Several updates are separated by ``;``. The staged lock is admitted when

- the working tree's Cargo.lock and manifests equal the staged ones, and
  ``cargo metadata --locked`` accepts them (the lock is consistent with every
  manifest, so nothing was hand-edited out of shape);
- every package whose version moved is one the intent names, at the version
  ``--precise`` names;
- every package entry that appeared or disappeared is reachable from a declared
  package's new or old entry, so the update may add or drop the dependencies
  the new version needs, but nothing else;
- every other entry is unchanged, apart from dependency references to the
  packages above.

A lock-only change with no declared update is admitted as a relock: what cargo
writes when manifests that are already committed need entries the lock lacks,
as after a rebase took main's side of a conflicting Cargo.lock
(.githooks/merge-generated) and `t fix lockfile` put back what the rebased
commit added. ``cargo metadata --locked`` must accept it, and it may add and
drop entries and dependency references but move no package's version.

Anything else stays refused, with the reason.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tomllib

INTENT_VARIABLE = "TILO_CARGO_UPDATE"


class IntentError(ValueError):
    """The declared intent is not a `cargo update` this check admits."""


@dataclass(frozen=True)
class Update:
    """One declared `cargo update -p <spec>... [--precise <version>]`."""

    packages: tuple[str, ...]
    precise: str | None


def parse_intent(text: str) -> list[Update]:
    """Parse ``TILO_CARGO_UPDATE`` into the updates it declares."""
    updates: list[Update] = []
    for clause in text.split(";"):
        words = shlex.split(clause)
        if words[:2] == ["cargo", "update"]:
            words = words[2:]
        elif words[:1] == ["update"]:
            words = words[1:]
        if not words:
            continue
        packages: list[str] = []
        precise: str | None = None
        index = 0
        while index < len(words):
            word = words[index]
            name, _, inline = word.partition("=")
            if name in ("-p", "--package", "--precise"):
                if inline:
                    value = inline
                elif index + 1 < len(words):
                    index += 1
                    value = words[index]
                else:
                    raise IntentError(f"{name} needs a value in {clause.strip()!r}")
                if name == "--precise":
                    precise = value
                else:
                    # A `name@version` spec selects which copy moves; the lock
                    # comparison below is by name.
                    packages.append(value.split("@", 1)[0])
            elif word in ("--locked", "--offline", "--frozen"):
                pass
            else:
                raise IntentError(
                    f"{word!r} is not admitted in {clause.strip()!r}; declare "
                    "`-p <crate> [--precise <version>]` updates only"
                )
            index += 1
        if not packages:
            raise IntentError(f"{clause.strip()!r} names no package (-p <crate>)")
        if precise is not None and len(packages) != 1:
            raise IntentError(f"--precise applies to exactly one package in {clause.strip()!r}")
        updates.append(Update(tuple(packages), precise))
    if not updates:
        raise IntentError("the intent declares no update")
    return updates


Key = tuple[str, str, str]  # name, version, source


def _entries(lock: dict) -> dict[Key, dict]:
    return {
        (package["name"], package["version"], package.get("source", "")): package
        for package in lock.get("package", [])
    }


def _dependency_names(entry: dict) -> set[str]:
    return {dependency.split(" ", 1)[0] for dependency in entry.get("dependencies", [])}


def _reachable(entries: dict[Key, dict], roots: set[Key]) -> set[str]:
    """Names reachable through dependency references from ``roots``."""
    by_name: dict[str, list[Key]] = {}
    for key in entries:
        by_name.setdefault(key[0], []).append(key)
    seen: set[Key] = set()
    pending = list(roots)
    while pending:
        key = pending.pop()
        if key in seen:
            continue
        seen.add(key)
        for name in _dependency_names(entries[key]):
            pending.extend(by_name.get(name, []))
    return {key[0] for key in seen}


def _shape_problems(old_lock: dict, new_lock: dict) -> list[str]:
    problems: list[str] = []
    if old_lock.get("version") != new_lock.get("version"):
        problems.append(
            f"the lock format changed ({old_lock.get('version')} -> {new_lock.get('version')})"
        )
    for table in set(old_lock) | set(new_lock):
        if table not in ("version", "package") and old_lock.get(table) != new_lock.get(table):
            problems.append(f"the lock's [{table}] table changed")
    return problems


def _kept_entry_problems(
    old: dict[Key, dict], new: dict[Key, dict], changed_names: set[str], why: str
) -> list[str]:
    """Entries on both sides may change only their references to ``changed_names``."""
    problems: list[str] = []
    for key in sorted(set(old) & set(new)):
        before, after = old[key], new[key]
        if {k: v for k, v in before.items() if k != "dependencies"} != {
            k: v for k, v in after.items() if k != "dependencies"
        }:
            problems.append(f"{key[0]} {key[1]} changed beyond its dependencies")
            continue
        stable_before = {
            dep for dep in before.get("dependencies", []) if dep.split(" ", 1)[0] not in changed_names
        }
        stable_after = {
            dep for dep in after.get("dependencies", []) if dep.split(" ", 1)[0] not in changed_names
        }
        if stable_before != stable_after:
            problems.append(f"{key[0]} {key[1]} changed dependencies {why}")
    return problems


def relock_violations(old_text: str, new_text: str) -> list[str]:
    """Why the change from ``old_text`` to ``new_text`` is more than a relock."""
    old_lock = tomllib.loads(old_text)
    new_lock = tomllib.loads(new_text)
    problems = _shape_problems(old_lock, new_lock)
    old = _entries(old_lock)
    new = _entries(new_lock)
    removed = set(old) - set(new)
    added = set(new) - set(old)
    removed_names = {key[0] for key in removed}
    added_names = {key[0] for key in added}
    for name in sorted(removed_names & added_names):
        before = sorted(key[1] for key in removed if key[0] == name)
        after = sorted(key[1] for key in added if key[0] == name)
        problems.append(f"{name} moved {', '.join(before)} -> {', '.join(after)}")
    # A relock may point a kept entry at another package already in the lock,
    # as when a committed manifest swapped one dependency for another: its
    # references may change, nothing else of it. `cargo metadata --locked`
    # holds the references to the manifests.
    every_name = {key[0] for key in set(old) | set(new)}
    problems += _kept_entry_problems(old, new, every_name, "")
    return problems


def lock_violations(old_text: str, new_text: str, updates: list[Update]) -> list[str]:
    """Why the change from ``old_text`` to ``new_text`` is not the declared update."""
    old_lock = tomllib.loads(old_text)
    new_lock = tomllib.loads(new_text)
    problems = _shape_problems(old_lock, new_lock)

    old = _entries(old_lock)
    new = _entries(new_lock)
    removed = set(old) - set(new)
    added = set(new) - set(old)
    removed_names = {key[0] for key in removed}
    added_names = {key[0] for key in added}
    moved = removed_names & added_names

    declared: dict[str, str | None] = {}
    for update in updates:
        for name in update.packages:
            declared[name] = update.precise

    for name, precise in sorted(declared.items()):
        if name not in moved and name not in added_names and name not in removed_names:
            problems.append(f"{name} is declared but its lock entry did not change")
        elif precise is not None and not any(
            key[0] == name and key[1] == precise for key in added
        ):
            versions = sorted(key[1] for key in added if key[0] == name)
            problems.append(
                f"{name} is declared at {precise} but moved to {', '.join(versions) or 'nothing'}"
            )

    for name in sorted(moved - set(declared)):
        before = sorted(key[1] for key in removed if key[0] == name)
        after = sorted(key[1] for key in added if key[0] == name)
        problems.append(
            f"{name} moved {', '.join(before)} -> {', '.join(after)} but is not declared"
        )

    new_reach = _reachable(new, {key for key in added if key[0] in declared})
    old_reach = _reachable(old, {key for key in removed if key[0] in declared})
    for key in sorted(added):
        if key[0] not in moved and key[0] not in declared and key[0] not in new_reach:
            problems.append(f"{key[0]} {key[1]} was added but no declared update needs it")
    for key in sorted(removed):
        if key[0] not in moved and key[0] not in declared and key[0] not in old_reach:
            problems.append(f"{key[0]} {key[1]} was removed but no declared update dropped it")

    problems += _kept_entry_problems(
        old, new, removed_names | added_names, "no declared update touches"
    )
    return problems


def _git(root: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args], cwd=root, text=True, capture_output=True, check=False
    )


def check(root: Path, intent: str | None) -> list[str]:
    """Problems with the staged lock-only change under ``root``."""
    updates: list[Update] = []
    if intent:
        try:
            updates = parse_intent(intent)
        except IntentError as error:
            return [f"{INTENT_VARIABLE}: {error}"]

    unstaged = _git(root, "diff", "--name-only", "--", "Cargo.lock", ":(glob)**/Cargo.toml")
    if unstaged.returncode != 0:
        return [f"git diff failed: {unstaged.stderr.strip()}"]
    if unstaged.stdout.strip():
        return [
            "cargo metadata checks the working tree, which differs from the staged "
            f"lock or manifests ({', '.join(unstaged.stdout.split())}); stage or revert those first"
        ]

    old = _git(root, "show", "HEAD:Cargo.lock")
    if old.returncode != 0:
        return [f"no committed Cargo.lock to compare with: {old.stderr.strip()}"]
    new = _git(root, "show", ":Cargo.lock")
    if new.returncode != 0:
        return [f"no staged Cargo.lock: {new.stderr.strip()}"]

    metadata = subprocess.run(
        [os.environ.get("CARGO", "cargo"), "metadata", "--locked", "--format-version", "1"],
        cwd=root,
        text=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        check=False,
    )
    if metadata.returncode != 0:
        tail = "\n    ".join(metadata.stderr.strip().splitlines()[-6:])
        return [f"cargo metadata --locked rejects the staged lock:\n    {tail}"]

    if not intent:
        relock = relock_violations(old.stdout, new.stdout)
        if not relock:
            return []
        return [
            "Cargo.lock changed with no Cargo.toml beside it, and more than a relock does "
            f"({'; '.join(relock)}). For a single-crate `cargo update -p <crate> --precise "
            f"<version>`, declare it: {INTENT_VARIABLE}='-p <crate> --precise <version>' git commit"
        ]
    return [
        f"{problem} ({INTENT_VARIABLE}={intent!r})"
        for problem in lock_violations(old.stdout, new.stdout, updates)
    ]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args(argv)
    problems = check(args.root, os.environ.get(INTENT_VARIABLE))
    for problem in problems:
        print(f"  {problem}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
