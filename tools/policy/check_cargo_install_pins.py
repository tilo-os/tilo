#!/usr/bin/env python3
"""Every `cargo install` in an image's Containerfile names its release.

An image builds its tools with the toolchain rust-toolchain.toml pins. An
unpinned `cargo install` takes whatever crates.io published last, which may
need a newer compiler: cargo-machete 0.10.1 did, and the build-environment
image stopped building with nothing changed in the tree. Each install names
one crate with `--version` and `--locked`, so a new release is taken on
purpose, as any other pinned input.
"""

from __future__ import annotations

import re
import shlex
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
# Flags whose value is the next word, so it is not read as a crate name.
VALUED = {"--version", "--root", "--git", "--branch", "--tag", "--rev", "--path", "--features",
          "--target", "--profile", "--index", "--registry", "-j", "--jobs", "--config", "-Z"}


def installs(text: str):
    """(line number, words after `cargo install`) for each install, continued lines joined."""
    joined, start = "", 0
    for number, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if line.startswith("#"):
            continue
        if not joined:
            start = number
        joined += " " + line.rstrip("\\")
        if line.endswith("\\"):
            continue
        for match in re.finditer(r"cargo install\s+([^;&|]*)", joined):
            yield start, shlex.split(match.group(1), comments=False)
        joined = ""


def main() -> int:
    failed = False
    for path in sorted(ROOT.glob("os/Containerfile*")):
        for number, words in installs(path.read_text(encoding="utf-8")):
            crates, skip = [], False
            for word in words:
                if skip:
                    skip = False
                elif word in VALUED:
                    skip = True
                elif not word.startswith("-"):
                    crates.append(word)
            pinned = "--version" in words and "--locked" in words and len(crates) == 1
            if not pinned:
                failed = True
                print(f"{path.relative_to(ROOT)}:{number}: `cargo install {' '.join(words)}` installs "
                      "what crates.io published last; install one crate per command with "
                      "--locked --version <release>")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
