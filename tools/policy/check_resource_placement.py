#!/usr/bin/env python3
"""Where an app keeps a resource kind moves only with a conversion.

Where an app keeps a kind is written down, not derived from bytes. An app's
``surface.toml`` describes each resource kind it keeps under ``[[resources]]``,
and ``tilo-app.toml`` lists under ``[resources] own`` the kinds each member
keeps as their own, which reach no one else's devices. Moving a kind out of an
app, or out of that list, moves what a person kept without converting it: a
window naming the kind opens an empty one, and the data is still on disk where
the new app does not look.

``against BASE`` fails when, between ``BASE`` and this tree:

- a kind is gone from an app's ``surface.toml`` ``[[resources]]`` and no other
  app declares it (``removed``), or another app declares it now (``moved``,
  named by the app it left);
- a kind joins (``joined-own``) or leaves (``left-own``) an app's
  ``tilo-app.toml`` ``[resources] own`` list.

Each such change passes when a ``[[change]]`` entry in
``tools/policy/record_conversions.toml`` names it by ``kind``, ``app`` and
``change``, and says either where the data now goes (``conversion =
"<path>:<fn>"``, a function that exists in this tree and moves what the old
place held) or what a person loses (``lost = "<what and why>"``). An entry
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
CHANGES = ("moved", "removed", "joined-own", "left-own")


def surface_kinds(text: str) -> set[str]:
    """The resource kinds an app's ``surface.toml`` declares."""
    return {
        resource["kind"]
        for resource in tomllib.loads(text).get("resources", [])
        if "kind" in resource
    }


def own_kinds(text: str) -> set[str]:
    """The kinds an app's ``tilo-app.toml`` has each member keep as their own."""
    return set(tomllib.loads(text).get("resources", {}).get("own", []))


def read_tree(root: Path) -> tuple[dict[str, set[str]], dict[str, set[str]]]:
    """Each app's surface kinds and own kinds, in the working tree."""
    surfaces: dict[str, set[str]] = {}
    own: dict[str, set[str]] = {}
    for file in sorted(root.glob("apps/*/surface.toml")):
        surfaces[file.parent.name] = surface_kinds(file.read_text(errors="replace"))
    for file in sorted(root.glob("apps/*/tilo-app.toml")):
        own[file.parent.name] = own_kinds(file.read_text(errors="replace"))
    return surfaces, own


def read_revision(root: Path, revision: str) -> tuple[dict[str, set[str]], dict[str, set[str]]]:
    """Each app's surface kinds and own kinds, at `revision`."""
    listed = subprocess.run(
        ["git", "ls-tree", "-r", "--name-only", revision, "--", "apps"],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    surfaces: dict[str, set[str]] = {}
    own: dict[str, set[str]] = {}
    for path in listed.splitlines():
        parts = path.split("/")
        if len(parts) != 3:
            continue
        app, name = parts[1], parts[2]
        if name not in ("surface.toml", "tilo-app.toml"):
            continue
        shown = subprocess.run(
            ["git", "show", f"{revision}:{path}"], cwd=root, capture_output=True, text=True
        )
        if shown.returncode != 0:
            continue
        if name == "surface.toml":
            surfaces[app] = surface_kinds(shown.stdout)
        else:
            own[app] = own_kinds(shown.stdout)
    return surfaces, own


def changes(
    then: tuple[dict[str, set[str]], dict[str, set[str]]],
    now: tuple[dict[str, set[str]], dict[str, set[str]]],
) -> list[tuple[str, str, str]]:
    """(kind, app, change) for each placement change between `then` and `now`."""
    then_surfaces, then_own = then
    now_surfaces, now_own = now
    found = []
    for app, kinds in sorted(then_surfaces.items()):
        for kind in sorted(kinds - now_surfaces.get(app, set())):
            elsewhere = any(
                kind in other for name, other in now_surfaces.items() if name != app
            )
            found.append((kind, app, "moved" if elsewhere else "removed"))
    for app in sorted(set(then_own) | set(now_own)):
        before, after = then_own.get(app, set()), now_own.get(app, set())
        for kind in sorted(after - before):
            found.append((kind, app, "joined-own"))
        for kind in sorted(before - after):
            found.append((kind, app, "left-own"))
    return found


def recorded(root: Path) -> list[dict]:
    """The ``[[change]]`` entries."""
    file = root / CONVERSIONS
    if not file.is_file():
        return []
    return tomllib.loads(file.read_text()).get("change", [])


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
        definition = item_definition(text.read_text(errors="replace"), name)
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
