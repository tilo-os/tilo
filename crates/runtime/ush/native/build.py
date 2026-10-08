#!/usr/bin/env python3
"""Build the native producer against the builder's qualified Bash/Readline ABI.

Creates one build artifact. Does not install, enable or start the producer.
"""

from __future__ import annotations

import argparse
import platform
import re
import subprocess
from pathlib import Path


def build(output: Path, headers: Path) -> None:
    version = subprocess.run(
        ["/usr/bin/bash", "--noprofile", "--norc", "--version"],
        env={"PATH": "/usr/bin:/bin", "LANG": "C"},
        check=True,
        capture_output=True,
        text=True,
        timeout=5,
    ).stdout
    match = re.match(r"GNU bash, version (\d+\.\d+)\.(\d+)\(", version)
    if match is None:
        raise RuntimeError("cannot qualify the build's Bash version")
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
            "-Wall",
            "-Wextra",
            "-Werror",
            *hardening,
            "-isystem",
            str(headers / "bash"),
            "-isystem",
            str(headers / "bash/include"),
            "-isystem",
            str(headers),
            f'-DTILO_BASH_VERSION="{match[1]}"',
            f"-DTILO_BASH_PATCH={match[2]}",
            str(Path(__file__).with_name("bash_editor.c")),
            "-o",
            str(output),
        ],
        check=True,
        timeout=30,
    )
    output.chmod(0o755)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--headers", type=Path, default=Path("/usr/include"))
    args = parser.parse_args()
    build(args.out, args.headers)


if __name__ == "__main__":
    main()
