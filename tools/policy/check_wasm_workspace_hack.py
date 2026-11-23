#!/usr/bin/env python3
"""Keep the workspace-hack out of the browser's wasm32 builds.

``crates/workspace-hack`` unifies the workspace's *native* feature sets
(tokio's ``net``, sockets, D-Bus, ...), and hakari writes most of them into its
unconditional ``[dependencies]``. A crate that also builds for
``wasm32-unknown-unknown`` (the browser runtime's web core, the apps it links
as side modules, the registry verifier, an app's model module) therefore names the hack only under
``[target.'cfg(not(target_arch = "wasm32"))'.dependencies]``; otherwise its
wasm32 build pulls mio and fails. ``cargo hakari manage-deps`` adds the line
unconditionally to a new member, so this check names any member the browser
builds reach that picked it up.

The graph is resolved for the selection the page and browser families build:
the packages ``tools/ci/package_sets.py wasm`` names, cargo tree's
``--workspace --exclude`` form, for ``wasm32-unknown-unknown`` without proc
macros (they build for the host). Resolving one root alone misses what a
sibling member in the selection pulls in, which is how a native dependency
added through another member reached a page build while this check passed.
Resolving it needs no wasm32 target installed.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WASM = "wasm32-unknown-unknown"
HACK = "workspace-hack"
PACKAGE_SETS = Path(__file__).resolve().parents[1] / "ci/package_sets.py"


def run(root: Path, command: list[str]) -> str:
    result = subprocess.run(command, cwd=root, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        raise RuntimeError(f"{command[0]} failed:\n{result.stderr.strip()}")
    return result.stdout


def members_of(root: Path) -> dict[str, str]:
    """Each workspace member, by name, with its manifest's path from the root."""
    metadata = json.loads(run(
        root, ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"]))
    return {
        package["name"]: Path(package["manifest_path"]).resolve().relative_to(root.resolve()).as_posix()
        for package in metadata["packages"]
    }


def wasm_selection(root: Path) -> set[str]:
    """The packages the page and browser families build for wasm32."""
    printed = run(root, [sys.executable, str(PACKAGE_SETS), "wasm"])
    names = set()
    tokens = printed.split()
    for index, token in enumerate(tokens):
        if token in ("-p", "--package") and index + 1 < len(tokens):
            names.add(tokens[index + 1])
    if not names:
        raise RuntimeError(f"no wasm packages: {printed.strip()}")
    return names


def tree(root: Path, members: dict[str, str], selection: set[str], *,
         package: str | None = None, invert: str | None = None) -> list[str]:
    """What the selection (or one of its members) resolves for wasm32, as package names."""
    command = [
        "cargo", "tree", "--locked", "--target", WASM,
        "--edges", "normal,no-proc-macro", "--prefix", "none", "--format", "{p}",
    ]
    if package is None:
        command.append("--workspace")
        for name in sorted(set(members) - selection):
            command += ["--exclude", name]
    else:
        command += ["--package", package]
    if invert:
        command += ["--invert", invert, "--depth", "1"]
    return [line.split()[0] for line in run(root, command).splitlines() if line.strip()]


def main() -> int:
    try:
        members = members_of(ROOT)
        selection = wasm_selection(ROOT)
        reached = tree(ROOT, members, selection)
        if HACK not in reached:
            print(f"the page's {WASM} selection: no {HACK}", file=sys.stderr)
            return 0
    except RuntimeError as error:
        print(error, file=sys.stderr)
        return 1
    failed = False
    for member in sorted(selection & set(members)):
        try:
            if HACK not in tree(ROOT, members, selection, package=member):
                continue
            dependants = sorted(
                set(tree(ROOT, members, selection, package=member, invert=HACK)) - {HACK})
        except RuntimeError as error:
            print(error, file=sys.stderr)
            return 1
        failed = True
        through = ", ".join(dependants) if dependants else "a dependency it names"
        print(
            f"{members[member]}: {member} reaches {HACK} for {WASM} through {through}; name it "
            "under [target.'cfg(not(target_arch = \"wasm32\"))'.dependencies] in those manifests"
        )
    if not failed:
        # The selection reaches it, but no member resolves it alone: name what the union pulls.
        try:
            dependants = sorted(set(tree(ROOT, members, selection, invert=HACK)) - {HACK})
        except RuntimeError as error:
            print(error, file=sys.stderr)
            return 1
        print(
            f"the page's {WASM} selection reaches {HACK} through {', '.join(dependants)}; name it "
            "under [target.'cfg(not(target_arch = \"wasm32\"))'.dependencies] in those manifests",
            file=sys.stderr,
        )
    return 1


if __name__ == "__main__":
    sys.exit(main())
