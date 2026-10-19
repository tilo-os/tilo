#!/usr/bin/env python3
"""Keep the workspace's package graph acyclic.

A package that depends on itself through other packages cannot be resolved:
``cargo metadata``, and with it every build, test and ``cargo tree``, fails
for the whole workspace. Such a cycle comes in quietly: a low crate gains a
dependency on a crate that already sits above it (a derive from the resource
layer, say, which reaches the filesystem client through the content store),
and nothing about the change itself looks wrong.

This check resolves the workspace as ``cargo metadata --locked --offline``
does, without compiling, and fails with the cycle spelled out. Cargo rejects
cycles over normal and build dependencies while resolving; the resolved graph
is walked again here over those edges, so the check does not depend on the
wording of Cargo's message. Dev-dependencies may form cycles (a crate's tests
may use a crate that depends on it), so they are not followed.
"""

from __future__ import annotations

import json
import subprocess
import sys
from collections.abc import Callable, Mapping, Sequence
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
COMMAND = ("cargo", "metadata", "--locked", "--offline", "--format-version", "1")
CARGO_CYCLE = "cyclic package dependency"
HINT = (
    "A crate cannot depend on a crate that already depends on it. Move what the "
    "lower crate needs below both (split it into a crate of its own), or invert "
    "the edge."
)


def package_name(node_id: str, names: Mapping[str, str]) -> str:
    return names.get(node_id, node_id)


def find_cycle(metadata: Mapping) -> list[str] | None:
    """The first cycle over normal and build edges, as package names, closed
    (its first name repeated last); `None` when the graph is acyclic."""
    names = {package["id"]: package["name"] for package in metadata.get("packages", [])}
    resolve = metadata.get("resolve") or {}
    edges: dict[str, list[str]] = {}
    for node in resolve.get("nodes", []):
        targets = []
        for dep in node.get("deps", []):
            kinds = {kind.get("kind") for kind in dep.get("dep_kinds", [])}
            if kinds - {"dev"}:
                targets.append(dep["pkg"])
        edges[node["id"]] = sorted(targets)

    # Iterative depth-first search; `path` holds the stack of the current walk.
    done: set[str] = set()
    for start in sorted(edges):
        if start in done:
            continue
        path: list[str] = [start]
        on_path = {start}
        cursors = [iter(edges.get(start, []))]
        while cursors:
            nxt = next(cursors[-1], None)
            if nxt is None:
                cursors.pop()
                finished = path.pop()
                on_path.discard(finished)
                done.add(finished)
                continue
            if nxt in on_path:
                cycle = path[path.index(nxt) :] + [nxt]
                return [package_name(node, names) for node in cycle]
            if nxt in done:
                continue
            path.append(nxt)
            on_path.add(nxt)
            cursors.append(iter(edges.get(nxt, [])))
    return None


def run_cargo(root: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        COMMAND, cwd=root, capture_output=True, text=True, check=False
    )


def audit(
    root: Path = ROOT,
    run: Callable[[Path], subprocess.CompletedProcess[str]] = run_cargo,
) -> list[str]:
    """The failure, as lines to print; empty when the graph resolves acyclic."""
    result = run(root)
    if result.returncode != 0:
        error = result.stderr.strip()
        if CARGO_CYCLE in error:
            return [
                "the workspace's package graph has a cycle; Cargo cannot resolve it:",
                *(f"  {line}" for line in error.splitlines()),
                HINT,
            ]
        return [
            f"`{' '.join(COMMAND)}` failed, so the package graph could not be checked:",
            *(f"  {line}" for line in error.splitlines()),
        ]
    cycle = find_cycle(json.loads(result.stdout))
    if cycle:
        return [
            "the workspace's package graph has a cycle:",
            "  " + " -> ".join(cycle),
            HINT,
        ]
    return []


def main(argv: Sequence[str] | None = None) -> int:
    del argv
    failure = audit()
    if failure:
        print("Dependency cycle check FAILED", file=sys.stderr)
        for line in failure:
            print(line, file=sys.stderr)
        return 1
    print("Dependency cycle check OK (the package graph resolves and is acyclic)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
