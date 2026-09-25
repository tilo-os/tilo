#!/usr/bin/env python3
"""Keep the canonical desktop model free of host stacks.

``tilo-desktop-model`` is shared by every host: the native compositor, a
headless peer, and anything else that renders or serves a desktop. It must
therefore not reach the compositor, the toolkit, a transport, an async
runtime or the host filesystem — directly or through any dependency. The
graph is resolved the way a build of the crate alone resolves it (``cargo
tree -p``), so a default feature that pulls a host stack into a shared
dependency is caught too.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
PACKAGE = "tilo-desktop-model"
FORBIDDEN = frozenset(
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
)


def reached_packages(root: Path = ROOT) -> set[str]:
    result = subprocess.run(
        [
            "cargo",
            "tree",
            "--locked",
            "--package",
            PACKAGE,
            "--edges",
            "normal",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"cargo tree failed:\n{result.stderr}")
    return {line.split()[0] for line in result.stdout.splitlines() if line.strip()}


def main() -> int:
    try:
        packages = reached_packages()
    except RuntimeError as error:
        print(error, file=sys.stderr)
        return 1
    if PACKAGE not in packages:
        print(f"cargo tree did not describe {PACKAGE}", file=sys.stderr)
        return 1
    reached = sorted(packages & FORBIDDEN)
    if reached:
        print(
            f"{PACKAGE} reaches {', '.join(reached)}; hosts own those stacks, "
            "the shared desktop model must not",
            file=sys.stderr,
        )
        return 1
    print(f"{PACKAGE}: {len(packages)} packages, no host stacks")
    return 0


if __name__ == "__main__":
    sys.exit(main())
