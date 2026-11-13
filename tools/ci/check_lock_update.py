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
- every other entry is unchanged beyond its dependency references, which are
  held to the canonical form below.

A lock-only change with no declared update is admitted as a relock: what cargo
writes when manifests that are already committed need entries the lock lacks,
as after a rebase took main's side of a conflicting Cargo.lock
(.githooks/merge-generated) and `t fix lockfile` put back what the rebased
commit added. ``cargo metadata --locked`` must accept it, and it may add and
drop entries and dependency references but move no package's version.

Anything else stays refused, with the reason.

Canonical dependency references
-------------------------------
Several crates in the tree take a wide requirement (`windows-sys = ">=0.52,
<0.62"` and the like), so many versions already in the lock satisfy it, and the
resolver's answer for which one a package references depends on the order it
walks the graph: any re-resolve, ``cargo update -p <crate> --precise
<same version>`` included, may re-point those references at another version it
already resolved, and running it again points them back. Every assignment is
valid, so ``cargo metadata --locked`` accepts all of them.

The lock's canonical form is the one this file enforces: a reference points at
the newest version already in the lock that the referencing package allows.
It is what a fresh resolve picks, it is the same for everyone, and it leaves
only the crate a `cargo update` names in the diff. Use

    python3 tools/ci/check_lock_update.py --write

to write it (the pre-commit hook does this on a staged lock); CI runs
``--verify``.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import json
import os
import re
from collections.abc import Sequence
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


# --- canonical dependency references ---------------------------------------
#
# A requirement is a comma-separated list of comparators, matched the way
# cargo's semver does: `^1.2.3` is `>=1.2.3, <2.0.0`, `0.61.*` and `=0.61` are
# both `>=0.61.0, <0.62.0`, and a version's build metadata is ignored.


def _version_parts(text: str) -> tuple[tuple[int, int, int], str]:
    """A version's three numbers and its pre-release, without build metadata."""
    core = text.split("+", 1)[0]
    numbers, _, pre = core.partition("-")
    return _padded([int(part) for part in numbers.split(".") if part]), pre


def _padded(numbers: Sequence[int]) -> tuple[int, int, int]:
    filled = list(numbers)[:3] + [0, 0, 0]
    return (filled[0], filled[1], filled[2])


def _bumped(numbers: Sequence[int], index: int) -> tuple[int, int, int]:
    filled = list(_padded(numbers))
    filled[index] += 1
    for rest in range(index + 1, 3):
        filled[rest] = 0
    return (filled[0], filled[1], filled[2])


def _prerelease_key(pre: str) -> tuple:
    """Sort key placing a release above every pre-release of that version."""
    if not pre:
        return (1, ())
    return (0, tuple((0, int(part)) if part.isdigit() else (1, part) for part in pre.split(".")))


def _order(version: str) -> tuple:
    """What versions sort by: their numbers, then a release above its pre-releases."""
    return _order_of(*_version_parts(version))


def _order_of(numbers: Sequence[int], pre: str) -> tuple:
    return (_padded(numbers), _prerelease_key(pre))


def _parse_comparator(part: str) -> tuple[str, list[int], str, bool]:
    operator = "^"
    for candidate in ("^", "~", ">=", "<=", ">", "<", "="):
        if part.startswith(candidate):
            operator, part = candidate, part[len(candidate) :].strip()
            break
    core, _, pre = part.split("+", 1)[0].partition("-")
    numbers: list[int] = []
    wildcard = False
    for piece in core.split("."):
        piece = piece.strip()
        if piece in ("*", "x", "X"):
            wildcard = True
            break
        if piece:
            numbers.append(int(piece))
    return operator, numbers, pre, wildcard


def requirement_matches(requirement: str, version: str) -> bool:
    """Whether ``version`` satisfies ``requirement``, as cargo's semver does."""
    numbers, pre = _version_parts(version)
    current = _order_of(numbers, pre)
    prerelease_commons: set[tuple[int, int, int]] = set()
    for part in requirement.split(","):
        part = part.strip()
        if not part or part == "*":
            continue
        operator, wanted, wanted_pre, wildcard = _parse_comparator(part)
        floor = _order_of(wanted, wanted_pre)
        if wildcard:
            operator = "="
        if operator == "^":
            if not wanted:
                continue
            ceiling = next(
                (_bumped(wanted, index) for index, value in enumerate(_padded(wanted)) if value),
                (1, 0, 0),
            )
            inside = current >= floor and numbers < ceiling
        elif operator == "~":
            if not wanted:
                continue
            inside = (
                current >= floor
                and numbers < _bumped(wanted, 0 if len(wanted) == 1 else 1)
            )
        elif operator == "=":
            if not wanted:
                continue
            if len(wanted) == 3:
                inside = current == floor
            else:
                inside = (
                    current >= floor
                    and numbers < _bumped(wanted, len(wanted) - 1)
                )
        elif operator == ">=":
            inside = current >= floor if wanted else True
        elif operator == ">":
            if not wanted:
                continue
            inside = (
                current > floor
                if len(wanted) == 3
                else numbers >= _bumped(wanted, len(wanted) - 1)
            )
        elif operator == "<":
            inside = current < floor if wanted else False
        else:  # "<="
            if not wanted:
                continue
            inside = (
                current <= floor
                if len(wanted) == 3
                else numbers < _bumped(wanted, len(wanted) - 1)
            )
        if not inside:
            return False
        if wanted_pre:
            prerelease_commons.add(_padded(wanted))
    # A pre-release only satisfies a requirement that names a pre-release of the
    # same three numbers; cargo keeps pre-releases opt-in.
    if pre:
        return numbers in prerelease_commons
    return True


def _requirements(metadata: dict) -> dict[Key, dict[str, frozenset[str]]]:
    """Each package's requirements for the crates it names, as cargo resolved it."""
    collected: dict[Key, dict[str, set[str]]] = {}
    for package in metadata.get("packages", []):
        key = (package["name"], package["version"], package.get("source") or "")
        declared = collected.setdefault(key, {})
        for dependency in package.get("dependencies", []):
            declared.setdefault(dependency["name"], set()).add(dependency["req"])
    return {
        key: {name: frozenset(requirements) for name, requirements in declared.items()}
        for key, declared in collected.items()
    }


def _versions_by_name(lock: dict) -> dict[str, tuple[str, ...]]:
    versions: dict[str, list[str]] = {}
    for key in _entries(lock):
        versions.setdefault(key[0], []).append(key[1])
    return {name: tuple(sorted(found, key=_order)) for name, found in versions.items()}


def _canonical_version(
    allowed: frozenset[str], version: str, candidates: tuple[str, ...]
) -> str | None:
    """The reference ``version`` should be, or None when it already is.

    The one a fresh resolve picks: the newest version in the lock that stands in
    for everything this one does. A package may allow several versions it cannot
    share (two majors for two targets), and each reference then keeps to its own.
    """
    matched = frozenset(
        requirement for requirement in allowed if requirement_matches(requirement, version)
    )
    if not matched:
        return None  # cargo metadata --locked would have refused such a reference
    better = [
        candidate
        for candidate in candidates
        if _order(candidate) > _order(version)
        and matched
        <= frozenset(
            requirement
            for requirement in allowed
            if requirement_matches(requirement, candidate)
        )
    ]
    return better[-1] if better else None


def reference_problems(lock: dict, requirements: dict[Key, dict[str, frozenset[str]]]) -> list[str]:
    """Why a dependency reference is not the canonical one (see the docstring)."""
    versions = _versions_by_name(lock)
    problems: list[str] = []
    for key, entry in sorted(_entries(lock).items()):
        declared = requirements.get(key)
        if declared is None:
            continue
        for reference in entry.get("dependencies", []):
            name, _, version = reference.partition(" ")
            allowed = declared.get(name)
            if not version or not allowed:
                continue
            wanted = _canonical_version(allowed, version, versions[name])
            if wanted is not None:
                problems.append(
                    f"{key[0]} {key[1]} points at {name} {version}, but {wanted} is the newest "
                    f"version of it the lock holds that {key[0]} allows"
                )
    return problems


def canonical_lock_text(
    text: str, requirements: dict[Key, dict[str, frozenset[str]]]
) -> tuple[str, list[str]]:
    """``text`` with every dependency reference in canonical form, and what changed.

    A version whose last reference moved up is dropped with it: cargo refuses a
    lock that holds packages the manifests do not resolve.
    """
    lock = tomllib.loads(text)
    versions = _versions_by_name(lock)
    lines = text.splitlines(keepends=True)
    starts = [index for index, line in enumerate(lines) if line.strip() == "[[package]]"]
    preamble_text = "".join(lines[: starts[0]]) if starts else text
    bounds = list(zip(starts, starts[1:] + [len(lines)]))
    changes: list[str] = []
    blocks = [
        _canonical_block(lines[begin:end], requirements, versions, changes)
        for begin, end in bounds
    ]
    keys = [_entry_key(block) for block in blocks]
    rewritten = preamble_text + "".join(line for block in blocks for line in block)
    entries = _entries(tomllib.loads(rewritten))
    kept = _reached(entries, {key for key in entries if not key[2]})
    output = [preamble_text]
    for key, block in zip(keys, blocks):
        if key in kept:
            output.extend(block)
        else:
            changes.append(f"{key[0]} {key[1]}: dropped, nothing references it")
    return "".join(output), changes


def _canonical_block(
    block: list[str],
    requirements: dict[Key, dict[str, frozenset[str]]],
    versions: dict[str, tuple[str, ...]],
    changes: list[str],
) -> list[str]:
    """One ``[[package]]`` block with its references pointing at canonical versions."""
    key = _entry_key(block)
    declared = requirements.get(key, {})
    in_dependencies = False
    for index, line in enumerate(block):
        stripped = line.strip()
        if stripped == "dependencies = [":
            in_dependencies = True
            continue
        if not in_dependencies:
            continue
        if stripped == "]":
            in_dependencies = False
            continue
        reference = _REFERENCE.match(line)
        if reference is None:
            continue
        name, _, version = reference.group(2).partition(" ")
        allowed = declared.get(name)
        if not version or not allowed:
            continue
        wanted = _canonical_version(allowed, version, versions[name])
        if wanted is None:
            continue
        block[index] = f'{reference.group(1)}"{name} {wanted}",{reference.group(3)}'
        changes.append(f"{key[0]} {key[1]}: {name} {version} -> {wanted}")
    return block


def _entry_key(block: list[str]) -> Key:
    fields = {}
    for line in block:
        for field in ("name", "version", "source"):
            if line.strip().startswith(f"{field} = "):
                fields[field] = line.split("=", 1)[1].strip().strip('"')
    return (fields["name"], fields["version"], fields.get("source", ""))


_REFERENCE = re.compile(r'(\s*)"([^"]+)",(\r?\n?)$')


def _reached(entries: dict[Key, dict], roots: set[Key]) -> set[Key]:
    """The entries reachable from ``roots`` through their dependency references."""
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
        for reference in entries[key].get("dependencies", []):
            name, _, version = reference.partition(" ")
            pending.extend(
                candidate
                for candidate in by_name.get(name, ())
                if not version or candidate[1] == version
            )
    return seen


def _reachable(entries: dict[Key, dict], roots: set[Key]) -> set[str]:
    """Names reachable through dependency references from ``roots``."""
    return {key[0] for key in _reached(entries, roots)}


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


def _kept_entry_problems(old: dict[Key, dict], new: dict[Key, dict]) -> list[str]:
    """Entries on both sides may change their dependency references, nothing else."""
    problems: list[str] = []
    for key in sorted(set(old) & set(new)):
        before, after = old[key], new[key]
        if {k: v for k, v in before.items() if k != "dependencies"} != {
            k: v for k, v in after.items() if k != "dependencies"
        }:
            problems.append(f"{key[0]} {key[1]} changed beyond its dependencies")
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
    # A relock may point a kept entry at another package already in the lock, as
    # when a committed manifest swapped one dependency for another or cargo
    # re-resolved a wide requirement onto another version: its references may
    # change, held to the canonical form checked besides this, and nothing else of
    # it. `cargo metadata --locked` holds the references to the manifests.
    problems += _kept_entry_problems(old, new)
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

    problems += _kept_entry_problems(old, new)
    return problems


def _git(root: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args], cwd=root, text=True, capture_output=True, check=False
    )


def _metadata(root: Path) -> tuple[dict, str | None]:
    """The resolved workspace, or why cargo refuses the lock.

    A lock entry the default selection does not reach (an optional dependency no
    feature turns on here) keeps the references cargo wrote for it: this reads
    what one resolve covers, and downloads no more than `cargo metadata --locked`
    already did.
    """
    run = subprocess.run(
        [os.environ.get("CARGO", "cargo"), "metadata", "--locked", "--format-version", "1"],
        cwd=root,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if run.returncode != 0:
        return {}, "\n    ".join(run.stderr.strip().splitlines()[-6:])
    return json.loads(run.stdout), None


REFERENCE_HINT = (
    "a dependency reference is not in canonical form; "
    "`python3 tools/ci/check_lock_update.py --write` writes it, stage Cargo.lock"
)


def tree_reference_problems(root: Path) -> list[str]:
    """Why the working tree's Cargo.lock has a reference that is not canonical."""
    metadata, error = _metadata(root)
    if error is not None:
        return [f"cargo metadata --locked rejects the lock:\n    {error}"]
    lock = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))
    return reference_problems(lock, _requirements(metadata))


def write_canonical_references(root: Path) -> list[str]:
    """Rewrite the working tree's Cargo.lock into canonical form; what moved."""
    metadata, error = _metadata(root)
    if error is not None:
        raise SystemExit(f"cargo metadata --locked rejects the lock:\n    {error}")
    path = root / "Cargo.lock"
    before = path.read_text(encoding="utf-8")
    text, changes = canonical_lock_text(before, _requirements(metadata))
    if not changes:
        return []
    path.write_text(text, encoding="utf-8")
    _, error = _metadata(root)
    if error is not None:
        path.write_text(before, encoding="utf-8")
        raise SystemExit(f"the canonical references would not resolve:\n    {error}")
    return changes


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

    metadata, error = _metadata(root)
    if error is not None:
        return [f"cargo metadata --locked rejects the staged lock:\n    {error}"]

    problems = [
        f"{problem} ({REFERENCE_HINT})"
        for problem in reference_problems(tomllib.loads(new.stdout), _requirements(metadata))
    ]
    if not intent:
        relock = relock_violations(old.stdout, new.stdout)
        if relock:
            problems.append(
                "Cargo.lock changed with no Cargo.toml beside it, and more than a relock does "
                f"({'; '.join(relock)}). For a single-crate `cargo update -p <crate> --precise "
                f"<version>`, declare it: {INTENT_VARIABLE}='-p <crate> --precise <version>' git commit"
            )
        return problems
    return problems + [
        f"{problem} ({INTENT_VARIABLE}={intent!r})"
        for problem in lock_violations(old.stdout, new.stdout, updates)
    ]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument(
        "--write",
        action="store_true",
        help="write the canonical dependency references into the working tree's Cargo.lock",
    )
    parser.add_argument(
        "--verify",
        action="store_true",
        help="fail when the working tree's Cargo.lock is not in canonical form",
    )
    args = parser.parse_args(argv)
    if args.write:
        for change in write_canonical_references(args.root):
            print(f"  {change}", file=sys.stderr)
        return 0
    if args.verify:
        problems = tree_reference_problems(args.root)
        for problem in problems:
            print(f"  {problem}", file=sys.stderr)
        return 1 if problems else 0
    problems = check(args.root, os.environ.get(INTENT_VARIABLE))
    for problem in problems:
        print(f"  {problem}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
