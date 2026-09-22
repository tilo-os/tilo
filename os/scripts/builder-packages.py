#!/usr/bin/env python3
"""Print the RPM packages the production image builder installs, one per line.

    builder-packages.py [--containerfile os/Containerfile] [--stage builder]

The dev image (os/Containerfile.dev) compiles its binaries with, and ships for
in-VM builds, the same toolchain as the production image's `builder` stage:
the same compiler, -devel libraries and sccache, from the same frozen RPM
snapshot. Reading the list out of that stage at build time, rather than
copying it, is what keeps the two from drifting. Parsing is shared with
os/dev/derive-rpm-package-set.py, which decides what the snapshot contains.
"""

from __future__ import annotations

import argparse
import importlib.util
import platform
import re
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]


def derive_module():
    path = REPO / "os/dev/derive-rpm-package-set.py"
    spec = importlib.util.spec_from_file_location("derive_rpm_package_set", path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


def stage_text(text: str, stage: str) -> str:
    """The instructions of `FROM ... AS <stage>`, up to the next FROM."""
    lines = text.splitlines(keepends=True)
    start = next((i for i, line in enumerate(lines)
                  if re.match(rf"^FROM\s+\S+\s+AS\s+{re.escape(stage)}\s*$", line, re.I)), None)
    if start is None:
        raise SystemExit(f"builder-packages: no `FROM ... AS {stage}` stage in the Containerfile")
    end = next((i for i in range(start + 1, len(lines)) if re.match(r"^FROM\s", lines[i], re.I)), len(lines))
    return "".join(lines[start:end])


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--containerfile", type=Path, default=REPO / "os/Containerfile")
    parser.add_argument("--stage", default="builder")
    args = parser.parse_args()
    arch = platform.machine().lower()
    arch = {"amd64": "x86_64", "arm64": "aarch64"}.get(arch, arch)
    derive = derive_module()
    with tempfile.NamedTemporaryFile("w", suffix=".Containerfile") as handle:
        handle.write(stage_text(args.containerfile.read_text(encoding="utf-8"), args.stage))
        handle.flush()
        specs = sorted(set(derive.packages(Path(handle.name), arch)))
    if not specs:
        raise SystemExit(f"builder-packages: stage {args.stage!r} installs no packages")
    print("\n".join(specs))
    return 0


if __name__ == "__main__":
    sys.exit(main())
