#!/usr/bin/env python3
"""Keep the shared desktop crates free of host stacks.

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

Each graph is resolved the way a build of the crate alone resolves it
(``cargo tree -p``), for the host and for ``wasm32-unknown-unknown``, so a
default feature that pulls a host stack into a shared dependency is caught
too. The generated ``workspace-hack`` crate only unifies compilation features;
prune its artificial edges while retaining all real paths to forbidden stacks.
Resolving a graph does not need the wasm32 target installed.
"""

from __future__ import annotations

import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
WASM = "wasm32-unknown-unknown"


@dataclass(frozen=True)
class Boundary:
    package: str
    # Never reached, through any dependency.
    forbidden: frozenset[str]
    # Never a direct dependency of the package itself.
    not_direct: frozenset[str] = frozenset()
    targets: tuple[str | None, ...] = (None,)


BOUNDARIES = (
    Boundary(
        package="tilo-desktop-model",
        forbidden=frozenset(
            {
                "dirs",
                "iroh",
                "smithay",
                "taffy",
                "tilo-ui",
                "tokio",
                "vello",
                "wgpu",
                "winit",
            }
        ),
        targets=(None, WASM),
    ),
    Boundary(
        package="tilo-desktop",
        forbidden=frozenset({"iroh", "smithay"}),
        not_direct=frozenset({"dirs", "libc", "settings-store", "tokio"}),
        targets=(None, WASM),
    ),
)


def reached_packages(
    package: str,
    target: str | None = None,
    depth: int | None = None,
    root: Path = ROOT,
) -> set[str]:
    command = [
        "cargo",
        "tree",
        "--locked",
        "--package",
        package,
        "--edges",
        "normal",
        "--prefix",
        "none",
        "--format",
        "{p}",
    ]
    # desktop-model excludes the hack entirely; cargo tree rejects pruning a
    # package absent from its graph. Only the drawn desktop needs this filter.
    if package == "tilo-desktop":
        command += ["--prune", "workspace-hack"]
    if target:
        command += ["--target", target]
    if depth is not None:
        command += ["--depth", str(depth)]
    result = subprocess.run(command, cwd=root, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        raise RuntimeError(f"cargo tree failed:\n{result.stderr}")
    return {line.split()[0] for line in result.stdout.splitlines() if line.strip()}


def violations(boundary: Boundary, target: str | None) -> list[str]:
    where = target or "the host"
    packages = reached_packages(boundary.package, target)
    if boundary.package not in packages:
        return [f"cargo tree did not describe {boundary.package} for {where}"]
    found = []
    reached = sorted(packages & boundary.forbidden)
    if reached:
        found.append(
            f"{boundary.package} reaches {', '.join(reached)} for {where}; "
            "hosts own those stacks, the shared desktop must not"
        )
    if boundary.not_direct:
        direct = reached_packages(boundary.package, target, depth=1)
        reached = sorted(direct & boundary.not_direct)
        if reached:
            found.append(
                f"{boundary.package} depends on {', '.join(reached)} itself for {where}; "
                "only the crates that own those stacks may"
            )
    return found


def main() -> int:
    failed = False
    for boundary in BOUNDARIES:
        for target in boundary.targets:
            try:
                found = violations(boundary, target)
            except RuntimeError as error:
                print(error, file=sys.stderr)
                return 1
            for message in found:
                print(message, file=sys.stderr)
                failed = True
            if not found:
                print(f"{boundary.package} ({target or 'host'}): no host stacks")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
