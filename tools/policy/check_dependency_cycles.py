#!/usr/bin/env python3
"""Keep the workspace's package graph acyclic.

A package that depends on itself through other packages cannot be resolved:
``cargo metadata``, and with it every build, test and ``cargo tree``, fails
for the whole workspace. Such a cycle comes in quietly: a low crate gains a
dependency on a crate that already sits above it (a derive from the resource
layer, say, which reaches the filesystem client through the content store),
and nothing about the change itself looks wrong.

This check walks the package graph Cargo.lock records, without compiling or
resolving, and fails with the cycle spelled out. The lock has every package's
dependencies, but a workspace member's dev-dependencies among them; which of a
member's dependencies are dev-only comes from its manifest, read with
``cargo metadata --no-deps --offline``, as is every path dependency a member
declares, so a new edge between members counts before the lock has it.
Dev-dependencies may form cycles (a crate's tests may use a crate that depends
on it), so they are not followed.

Nothing here reads the registry cache: resolving the full graph
(``cargo metadata --locked --offline``) needed every locked crate's sources on
this machine, so every new dependency failed every commit on a machine that
hadn't fetched it yet.
"""

from __future__ import annotations

import json
import subprocess
import sys
import tomllib
from collections.abc import Callable, Mapping, Sequence
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
COMMAND = ("cargo", "metadata", "--no-deps", "--offline", "--format-version", "1")
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


def graph(members: Mapping, lock: Mapping) -> dict:
    """The package graph of `lock` (a parsed Cargo.lock) with the dev-only
    dependencies of the workspace `members` (``cargo metadata --no-deps``)
    marked as such, and every path dependency between members, in the shape
    `find_cycle` reads (``cargo metadata``'s packages and resolve)."""
    packages = lock.get("package", [])
    ids = {(p["name"], p["version"]): f"{p['name']} {p['version']}" for p in packages}
    by_name: dict[str, list[str]] = {}
    for (name, _), node in ids.items():
        by_name.setdefault(name, []).append(node)

    def resolve(entry: str) -> str | None:
        # `name`, `name version` or `name version (source)`, as the lock writes it.
        parts = entry.split(" ")
        if len(parts) == 1:
            found = by_name.get(parts[0], [])
            return found[0] if len(found) == 1 else None
        return ids.get((parts[0], parts[1]))

    nodes: dict[str, set[tuple[str, str | None]]] = {node: set() for node in ids.values()}
    for p in packages:
        for entry in p.get("dependencies", []):
            dep = resolve(entry)
            if dep is not None:
                nodes[f"{p['name']} {p['version']}"].add((dep, None))

    member_nodes = {}
    for m in members.get("packages", []):
        node = f"{m['name']} {m['version']}"
        member_nodes[m["name"]] = node
        nodes.setdefault(node, set())
    for m in members.get("packages", []):
        node = member_nodes[m["name"]]
        declared = m.get("dependencies", [])
        kept = {d["name"] for d in declared if d.get("kind") != "dev"}
        nodes[node] = {(dep, kind) for dep, kind in nodes[node] if dep.split(" ")[0] in kept}
        for d in declared:
            if d.get("path") and d.get("kind") != "dev" and d["name"] in member_nodes:
                nodes[node].add((member_nodes[d["name"]], d.get("kind")))

    return {
        "packages": [{"id": node, "name": node.split(" ")[0]} for node in nodes],
        "resolve": {
            "nodes": [
                {
                    "id": node,
                    "deps": [{"pkg": dep, "dep_kinds": [{"kind": kind}]} for dep, kind in sorted(deps, key=str)],
                }
                for node, deps in nodes.items()
            ]
        },
    }


def run_cargo(root: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        COMMAND, cwd=root, capture_output=True, text=True, check=False
    )


def audit(
    root: Path = ROOT,
    run: Callable[[Path], subprocess.CompletedProcess[str]] = run_cargo,
) -> list[str]:
    """The failure, as lines to print; empty when the graph is acyclic."""
    result = run(root)
    if result.returncode != 0:
        return [
            f"`{' '.join(COMMAND)}` failed, so the package graph could not be checked:",
            *(f"  {line}" for line in result.stderr.strip().splitlines()),
        ]
    try:
        lock = tomllib.loads((root / "Cargo.lock").read_text())
    except (OSError, tomllib.TOMLDecodeError) as e:
        return [f"Cargo.lock could not be read, so the package graph could not be checked: {e}"]
    cycle = find_cycle(graph(json.loads(result.stdout), lock))
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
    print("Dependency cycle check OK (the package graph is acyclic)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
