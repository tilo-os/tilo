#!/usr/bin/env python3
"""Keep OS internals and dead weight out of the shared app runtime.

Every thin app maps one Rust dylib, ``libruntime_dylib.so``
(crates/runtime/runtime-dylib), so each crate in it costs every app process
its load-time relocations and the memory they dirty, and every device
downloads it. Two rules hold for its dependency graph (TIL-353):

- **No OS internals.** The FUSE filesystem daemon, the sandbox launcher, the
  app host and the install CLI, the peer transport and the other daemons
  below are the OS's, never an app's. An app reaches them through a service;
  one that still calls into such a crate links it itself and gets only the
  code it calls.
- **No dead weight.** Every crate in the runtime is reached by at least one
  app of the default collection without going through the runtime.

The graph is resolved the way the release builds it
(os/scripts/build-thin-apps.sh through dist/apps/package-apps.sh): the
runtime and every default-collection app in one Cargo graph, each app with
its ``thin`` feature plus the collection's features, for both shipped
architectures. Feature unification is why this cannot be read from
``runtime-dylib``'s manifest alone: an app enabling a feature on a shared
crate can pull a daemon into the runtime through that crate.
"""

from __future__ import annotations

import collections
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
RUNTIME = "runtime-dylib"
TARGETS = ("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu")
# Crates the toolkit itself needs: if the resolved runtime lacks them, the
# check is not looking at the runtime and must fail rather than pass.
EXPECTED = frozenset({"tilo-ui", "tilo-app"})
# OS daemons and OS-internal crates. Each name is a Cargo package.
DENIED = {
    "tilo-fs": "the FUSE filesystem daemon",
    "fuser": "the FUSE filesystem daemon's kernel binding",
    "launcher": "the sandbox launcher",
    "app-host": "the app host daemon (apps use its client side)",
    "app-cli": "the install and registry CLI",
    "app-registry": "the registry protocol behind the app host",
    "install": "the package installer (tilo-install)",
    "iroh": "the peer transport, owned by the runtime daemon",
    "iroh-blobs": "the content store's transfer protocol",
    "iroh-gossip": "the filesystem's metadata gossip",
    "tilo-runtime": "the per-user runtime daemon",
    "tilo-relay": "the relay service",
    "shell": "the compositor",
    "ush": "the user shell daemon",
    "compat-service": "the Windows compatibility service",
}


def collection() -> tuple[list[str], list[str]]:
    """The default collection's packages and their release features."""
    result = subprocess.run(
        [sys.executable, "tools/apps/app_release.py", "--collection", "default"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"tools/apps/app_release.py failed:\n{result.stderr}")
    plan = json.loads(result.stdout)
    packages = list(plan["packages"])
    if not packages:
        raise RuntimeError("the default collection has no packages")
    return packages, [f"{package}/thin" for package in packages] + list(plan["features"])


def resolve(packages: list[str], features: list[str], target: str) -> str:
    command = ["cargo", "tree", "--locked", "--edges", "normal", "--target", target,
               "--prefix", "depth", "--format", "{p}", "--package", RUNTIME]
    for package in packages:
        command += ["--package", package]
    command += ["--features", ",".join(features)]
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        raise RuntimeError(f"cargo tree failed for {target}:\n{result.stderr}")
    return result.stdout


def parse(tree: str) -> tuple[list[str], dict[str, set[str]]]:
    """Roots and edges of a ``--prefix depth`` forest, keyed by package name.

    A deduplicated tree prints each package's dependencies once, under its
    first appearance, so the edges are complete even where a later
    appearance is marked ``(*)``.
    """
    roots: list[str] = []
    edges: dict[str, set[str]] = collections.defaultdict(set)
    stack: list[str] = []
    for line in tree.splitlines():
        match = re.match(r"^(\d+)(\S+)", line)
        if not match:
            continue
        depth, name = int(match.group(1)), match.group(2)
        del stack[depth:]
        if depth == 0:
            roots.append(name)
        elif stack:
            edges[stack[-1]].add(name)
        stack.append(name)
    return roots, edges


def reached(edges: dict[str, set[str]], start: str, through_runtime: bool = True) -> set[str]:
    seen: set[str] = set()
    pending = [start]
    while pending:
        name = pending.pop()
        if name in seen:
            continue
        seen.add(name)
        for dependency in edges.get(name, ()):
            if through_runtime or dependency != RUNTIME:
                pending.append(dependency)
    return seen


def violations(tree: str, apps: list[str], target: str) -> list[str]:
    roots, edges = parse(tree)
    if RUNTIME not in roots:
        return [f"cargo tree did not describe {RUNTIME} for {target}"]
    runtime = reached(edges, RUNTIME) - {RUNTIME}
    missing = sorted(EXPECTED - runtime)
    if missing:
        return [f"{RUNTIME} for {target} lacks {', '.join(missing)}; this is not the runtime graph"]
    found = []
    for name in sorted(runtime & DENIED.keys()):
        found.append(
            f"{RUNTIME} reaches {name}, {DENIED[name]}, for {target}: OS internals are not app "
            "runtime; an app reaches them through a service or links what it calls itself"
        )
    absent = sorted(set(apps) - set(roots))
    if absent:
        return found + [f"cargo tree did not describe {', '.join(absent)} for {target}"]
    used: set[str] = set()
    for app in apps:
        used |= reached(edges, app, through_runtime=False)
    for name in sorted(runtime - used):
        found.append(
            f"{RUNTIME} carries {name} for {target}, but no default-collection app reaches it: "
            "every app process would map it for nothing"
        )
    return found


def main() -> int:
    try:
        apps, features = collection()
        failed = False
        for target in TARGETS:
            found = violations(resolve(apps, features, target), apps, target)
            for message in found:
                print(message, file=sys.stderr)
            failed = failed or bool(found)
            if not found:
                print(f"{RUNTIME} ({target}): no OS internals, every crate used by an app")
    except RuntimeError as error:
        print(error, file=sys.stderr)
        return 1
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
