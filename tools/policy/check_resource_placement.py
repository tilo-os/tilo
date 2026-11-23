#!/usr/bin/env python3
"""Where an app keeps a resource kind moves only with a conversion.

Where an app keeps a kind is written down, not derived from bytes. An app's
``surface.toml`` describes each resource kind it keeps under ``[[resources]]``.
Moving a kind out of an app moves what a person kept without converting it: a
window naming the kind opens an empty one, and the data is still on disk where
the new app does not look.

``against BASE`` fails when, between ``BASE`` and this tree, a kind is gone
from an app's ``surface.toml`` ``[[resources]]`` and no other app declares it
(``removed``), or another app declares it now (``moved``, named by the app it
left).

Each such change passes when a ``[[change]]`` entry in
``tools/policy/record_conversions.toml`` names it by ``kind``, ``app`` and
``change`` (several kinds changed alike by ``kinds = [...]``), and says either where the data now goes (``conversion =
"<path>:<fn>"``, a function that exists in this tree and moves what the old
place held, or a tilolang package's ``migration action`` of that name in a
``.tl`` file) or what a person loses (``lost = "<what and why>"``). An entry
whose conversion names no function, and one that says neither, fails.

``check`` reads the recorded entries alone: each names a conversion that
exists, or a loss, and no entry is empty.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tomllib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from check_format_migrations import item_definition  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
CONVERSIONS = Path("tools/policy/record_conversions.toml")
CHANGES = ("moved", "removed")


def surface_kinds(text: str) -> set[str]:
    """The resource kinds an app's ``surface.toml`` declares."""
    return {
        resource["kind"]
        for resource in tomllib.loads(text).get("resources", [])
        if "kind" in resource
    }


def read_tree(root: Path) -> dict[str, set[str]]:
    """Each app's surface kinds, in the working tree."""
    return {
        file.parent.name: surface_kinds(file.read_text(errors="replace"))
        for file in sorted(root.glob("apps/*/surface.toml"))
    }


def read_revision(root: Path, revision: str) -> dict[str, set[str]]:
    """Each app's surface kinds, at `revision`."""
    listed = subprocess.run(
        ["git", "ls-tree", "-r", "--name-only", revision, "--", "apps"],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    surfaces: dict[str, set[str]] = {}
    for path in listed.splitlines():
        parts = path.split("/")
        if len(parts) != 3 or parts[2] != "surface.toml":
            continue
        shown = subprocess.run(
            ["git", "show", f"{revision}:{path}"], cwd=root, capture_output=True, text=True
        )
        if shown.returncode == 0:
            surfaces[parts[1]] = surface_kinds(shown.stdout)
    return surfaces


def changes(
    then: dict[str, set[str]], now: dict[str, set[str]]
) -> list[tuple[str, str, str]]:
    """(kind, app, change) for each placement change between `then` and `now`."""
    found = []
    for app, kinds in sorted(then.items()):
        for kind in sorted(kinds - now.get(app, set())):
            elsewhere = any(kind in other for name, other in now.items() if name != app)
            found.append((kind, app, "moved" if elsewhere else "removed"))
    return found


def recorded(root: Path) -> list[dict]:
    """The ``[[change]]`` entries, one per kind: an entry naming several (``kinds``) says the
    same of each."""
    file = root / CONVERSIONS
    if not file.is_file():
        return []
    entries = tomllib.loads(file.read_text()).get("change", [])
    return [dict(entry, kind=kind) for entry in entries
            for kind in entry.get("kinds", [entry.get("kind")])]


def conversion_problem(root: Path, entry: dict) -> str | None:
    """What is wrong with an entry's conversion or loss, or `None`."""
    entry_change = str(entry.get("change", ""))
    if entry_change not in CHANGES:
        return (
            f"{CONVERSIONS}: {entry.get('kind')!r} in {entry.get('app')!r} names change "
            f"{entry_change!r}; write one of {', '.join(CHANGES)}"
        )
    conversion = entry.get("conversion")
    lost = entry.get("lost")
    if conversion is not None:
        path, _, name = str(conversion).partition(":")
        text = root / path
        if not path or not name or not text.is_file():
            return (
                f"{CONVERSIONS}: {entry.get('kind')!r} names conversion {conversion!r}, "
                "which is no `<path>:<fn>` of a file in this tree"
            )
        source = text.read_text(errors="replace")
        if path.endswith(".tl"):
            # A tilolang package carries what a native app of its id kept in a migration.
            if re.search(rf"^\s*migration\s+action\s+{re.escape(name)}\s*\(", source, re.M):
                return None
            return (
                f"{CONVERSIONS}: {entry.get('kind')!r} names conversion {conversion!r}, "
                "which that package does not declare as a `migration action`"
            )
        definition = item_definition(source, name)
        if definition is None or not re.search(rf"\bfn\s+{re.escape(name)}\b", definition):
            return (
                f"{CONVERSIONS}: {entry.get('kind')!r} names conversion {conversion!r}, "
                "which that file does not define as a function"
            )
        return None
    if not lost or not str(lost).strip():
        return (
            f"{CONVERSIONS}: {entry.get('kind')!r} in {entry.get('app')!r} says neither "
            "where the data goes (`conversion = \"<path>:<fn>\"`) nor what a person "
            "loses (`lost = \"...\"`)"
        )
    return None


def check(root: Path = ROOT) -> list[str]:
    """What is wrong with the recorded conversions on their own."""
    problems = []
    for entry in recorded(root):
        if not entry.get("kind") or not entry.get("app"):
            problems.append(
                f"{CONVERSIONS}: an entry names no kind or no app; a change is a kind in an "
                "app"
            )
            continue
        problem = conversion_problem(root, entry)
        if problem is not None:
            problems.append(problem)
    return problems


def against(base: str, root: Path = ROOT) -> list[str]:
    """Each placement change between `base` and this tree that no entry names."""
    entries = recorded(root)
    named = {(entry.get("kind"), entry.get("app"), entry.get("change")) for entry in entries}
    problems = []
    for kind, app, change in changes(read_revision(root, base), read_tree(root)):
        if (kind, app, change) not in named:
            problems.append(
                f"apps/{app}: {kind!r} is {change.replace('-', ' ')} here, and no entry in "
                f"{CONVERSIONS} names it. What a person kept is moved with the function "
                "that converts it (`conversion = \"<path>:<fn>\"`), or the entry says what "
                "a person loses (`lost = \"...\"`)"
            )
    return problems + check(root)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("check")
    base = sub.add_parser("against")
    base.add_argument("base")
    args = parser.parse_args(argv)
    if args.command == "check":
        problems = check()
        for problem in problems:
            print(problem)
    else:
        problems = against(args.base)
        for problem in problems:
            print(f"::error::{problem}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
