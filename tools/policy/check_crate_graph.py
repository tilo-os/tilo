#!/usr/bin/env python3
"""Keep each package's graph free of the stacks it does not own.

A boundary names a package, the crates it must never reach through any
dependency, and what the package is: a stack it reaches is compiled into it
and held while it runs, and no line of its manifest says so.

``tilo-desktop-model`` is shared by every host: the native compositor, a
headless peer, the browser runtime, and anything else that renders or serves
a desktop. It must not reach the compositor, the toolkit, a transport, an
async runtime or the host filesystem — directly or through any dependency.

``tilo-desktop`` is the platform-neutral desktop drawn with the toolkit. It
may reach the toolkit (``tilo-ui``) and the graphics stack, and it builds for
``wasm32-unknown-unknown`` inside the browser runtime. It must never reach the
compositor or a peer transport, and it must not itself depend on an async
runtime, host directories, settings storage or libc: those it may only see
through the toolkit and the other shared crates it uses, which own them.
(They are reached transitively — libc by every native allocator-level
crate, tokio by feature unification — so the rule is on its own edges.)

``tilo-cli`` is the ``tilo`` program: the machine's own commands, the
desktop's actions, and the bridge an agent reaches its person's workspace
through. It draws nothing and plays nothing, so it reaches neither the
toolkit nor the media stack; either one compiled back in is megabytes a
``tilo mcp`` holds for as long as an agent keeps it. A task's programs go to
the tilolang host's own program, which ``tilo`` starts and does not link.

``tilo-agent-session`` drives a workspace's apps on a server, and serves them
with the daemon a server runs. Its whole graph, an example's dev-dependencies
included, stays clear of the compositor, the keyboard stack and the media
stack: any of them compiled in is a device driver, a Wayland server or a codec
stack that no server has.

``tilo-runtime`` is the device daemon, on every device and every platform.
It reaches an app in the app's own process, through the action registry and
the catalogue, and links no crate under ``apps/``. The edges into ``apps/``
its graph still has are counted per package in ``daemon_apps.txt``; a
count only goes down, and one above what the graph has fails until lowered.

Each graph is resolved the way a build of the crate alone resolves it
(``cargo tree -p``), for both Linux architectures and for
``wasm32-unknown-unknown``, so a
default feature that pulls a host stack into a shared dependency is caught
too. The generated ``workspace-hack`` crate only unifies compilation features;
prune its artificial edges while retaining all real paths to forbidden stacks.
Each target is named rather than left to the host, so the result is the same
on any machine. Resolving a graph does not need the targets installed.
"""

from __future__ import annotations

import re
import subprocess
from collections import Counter
import sys
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
WASM = "wasm32-unknown-unknown"
NATIVE = ("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu")


@dataclass(frozen=True)
class Boundary:
    package: str
    # The package's manifest, relative to the root: where its findings point.
    manifest: str
    # Never reached, through any dependency.
    forbidden: frozenset[str]
    # What the package is, in the words its own finding ends with.
    why: str
    # Never a direct dependency of the package itself.
    not_direct: frozenset[str] = frozenset()
    targets: tuple[str, ...] = NATIVE
    # Which of the package's dependencies count: its own, and an example's
    # dev-dependencies, are one program's.
    edges: tuple[str, ...] = ("normal",)


BOUNDARIES = (
    Boundary(
        package="tilo-desktop-model",
        manifest="crates/desktop/desktop-model/Cargo.toml",
        forbidden=frozenset(
            {
                "dirs",
                "iroh",
                "smithay",
                "taffy",
                "tilo-taffy",
                "tilo-ui",
                "tilo-vello",
                "tilo-winit",
                "tilo-wgpu",
                "tokio",
                "vello",
                "wgpu",
                "winit",
            }
        ),
        why="it is shared by every host, and the hosts own those stacks",
        targets=(*NATIVE, WASM),
    ),
    Boundary(
        package="tilo-cli",
        manifest="crates/core/tilo-cli/Cargo.toml",
        forbidden=frozenset({"tilo-media", "tilo-native-media", "tilo-ui"}),
        why="it draws nothing and plays nothing: the toolkit and the media stack are the apps'",
        targets=NATIVE,
    ),
    Boundary(
        package="tilo-agent-session",
        manifest="crates/core/agent-session/Cargo.toml",
        forbidden=frozenset(
            {
                "gbm",
                "gstreamer",
                "gstreamer-app",
                "gstreamer-base",
                "gstreamer-video",
                "input",
                "libseat",
                "libspa",
                "miracast",
                "pipewire",
                "pixman",
                "smithay",
                "tilo-app-keeper",
                "udev",
                "xkbcommon",
            }
        ),
        why=(
            "it drives apps on a server, which has no desktop: the compositor, the "
            "keyboard stack and the media stack are a device's"
        ),
        edges=("normal", "dev"),
    ),
)


def cargo_tree(package: str, target: str, edges: tuple[str, ...], *options: str) -> str:
    # Every package reaches the hack: it unifies features for the whole workspace,
    # so its edges say nothing about a package's own stacks.
    command = ["cargo", "tree", "--locked", "--package", package, "--edges", ",".join(edges)]
    command += ["--format", "{p}", "--prune", "workspace-hack", "--target", target, *options]
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        raise RuntimeError(f"cargo tree failed:\n{result.stderr}")
    return result.stdout


def reached_packages(package: str, target: str, edges: tuple[str, ...], *options: str) -> set[str]:
    tree = cargo_tree(package, target, edges, "--prefix", "none", *options)
    return {line.split()[0] for line in tree.splitlines() if line.strip()}


DAEMON = "tilo-runtime"
DAEMON_APPS = Path("tools/policy/daemon_apps.txt")


def app_edges(tree: str, apps: str) -> set[tuple[str, str]]:
    """``(package, app)`` for each edge of a ``cargo tree --prefix depth`` from a
    package outside ``apps`` into a crate under it: where a graph enters the apps."""
    path: list[tuple[str, bool]] = []
    found = set()
    for line in tree.splitlines():
        depth = re.match(r"\d+", line)
        name, _, rest = line[depth.end() :].partition(" ")
        node = (name, f"{apps}/" in rest)
        del path[int(depth.group()) :]
        if path and node[1] and not path[-1][1]:
            found.add((path[-1][0], name))
        path.append(node)
    return found


def daemon_findings(edges: set[tuple[str, str]], listed: dict[str, int]) -> list[str]:
    reached = Counter(package for package, _ in edges)
    found = []
    for package in sorted(reached.keys() | listed.keys()):
        count, allowed = reached[package], listed.get(package, 0)
        if count > allowed:
            apps = ", ".join(sorted(app for user, app in edges if user == package))
            found.append(
                f"{package} reaches {count} app crates ({apps}), more than the {allowed} "
                f"{DAEMON_APPS} allows: the device daemon ({DAEMON}) links it, and reaches an "
                "app in its own process, through its actions and the catalogue"
            )
        elif count < allowed:
            found.append(f"{DAEMON_APPS}: {package} reaches {count} app crates; lower it to {count}")
    return found


def daemon_violations() -> list[str]:
    edges = set()
    for target in (*NATIVE, "aarch64-apple-darwin"):
        tree = cargo_tree(DAEMON, target, ("normal",), "--prefix", "depth")
        edges |= app_edges(tree, str(ROOT / "apps"))
    lines = (ROOT / DAEMON_APPS).read_text().splitlines()
    listed = {line.split()[0]: int(line.split()[1]) for line in lines if not line.startswith("#")}
    return daemon_findings(edges, listed)


def violations(boundary: Boundary, target: str) -> list[str]:
    packages = reached_packages(boundary.package, target, boundary.edges)
    if boundary.package not in packages:
        raise RuntimeError(f"cargo tree did not describe {boundary.package} for {target}")
    found = []
    reached = sorted(packages & boundary.forbidden)
    if reached:
        found.append(
            f"{boundary.manifest}: {boundary.package} reaches {', '.join(reached)} for {target}; "
            + boundary.why
        )
    if boundary.not_direct:
        direct = reached_packages(boundary.package, target, boundary.edges, "--depth", "1")
        reached = sorted(direct & boundary.not_direct)
        if reached:
            found.append(
                f"{boundary.manifest}: {boundary.package} depends on {', '.join(reached)} itself for {target}; "
                "only the crates that own those stacks may"
            )
    return found


def main() -> int:
    found = []
    try:
        for boundary in BOUNDARIES:
            for target in boundary.targets:
                found += violations(boundary, target)
        found += daemon_violations()
    except RuntimeError as error:
        print(error, file=sys.stderr)
        return 1
    for message in found:
        print(message)
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main())
