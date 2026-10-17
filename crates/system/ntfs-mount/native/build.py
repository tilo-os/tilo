#!/usr/bin/env python3
"""Build the library the NTFS mount service preloads into its confined ntfs-3g.

Creates one build artifact. See fuse_handoff.c for what it does and why.
"""

from __future__ import annotations

import argparse
import platform
import subprocess
from pathlib import Path


def build(output: Path) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    hardening = [
        "-fstack-protector-strong",
        "-D_FORTIFY_SOURCE=3",
        "-Wl,-z,relro,-z,now,-z,noexecstack",
    ]
    if platform.machine() in ("x86_64", "amd64"):
        hardening.append("-Wl,-z,shstk")
    subprocess.run(
        [
            "/usr/bin/cc",
            "-shared",
            "-fPIC",
            "-O2",
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            *hardening,
            str(Path(__file__).with_name("fuse_handoff.c")),
            "-o",
            str(output),
        ],
        check=True,
        timeout=30,
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    build(parser.parse_args().out)


if __name__ == "__main__":
    main()
