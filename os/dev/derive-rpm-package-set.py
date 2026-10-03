#!/usr/bin/env python3
"""Print the direct RPM specs consumed from Tilo's frozen repository."""

from __future__ import annotations

import argparse
import pathlib
import platform
import re
import shlex


REPO = pathlib.Path(__file__).resolve().parents[2]
SOURCES = (
    "os/Containerfile",
    "os/Containerfile.dev",
    "os/Containerfile.host",
    "os/dev/composefs/Containerfile",
    "os/containers/tilo-relay/Containerfile",
    "os/Containerfile.env-runtime",
)


def logical_lines(text: str):
    pending = ""
    for physical in text.splitlines():
        pending += physical
        if re.search(r"\\\s*$", pending):
            pending = re.sub(r"\\\s*$", " ", pending)
            continue
        yield pending
        pending = ""
    if pending:
        yield pending


def packages(path: pathlib.Path, arch: str | None = None):
    text = path.read_text()
    for marker in re.finditer(
        r"^\s*#\s*tilo-rpm-snapshot-package(?:\[([^\]]+)\])?:\s*(\S+)\s*$", text, re.MULTILINE
    ):
        target, spec = marker.groups()
        if target is not None and target not in ("x86_64", "aarch64"):
            raise SystemExit(f"{path}: unsupported snapshot package architecture {target!r}")
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+()=*-]*", spec):
            raise SystemExit(f"{path}: invalid snapshot-only package {spec!r}")
        if target is None or arch is None or target == arch:
            yield spec

    for line in logical_lines(text):
        for command in line.split("&&"):
            if not re.search(r"\b(?:dnf|microdnf)\s+install\b", command):
                continue
            try:
                words = shlex.split(command, comments=True)
            except ValueError as error:
                raise SystemExit(f"{path}: cannot parse shell command {command!r}: {error}")
            for index, word in enumerate(words):
                if word not in ("dnf", "microdnf"):
                    continue
                try:
                    install = words.index("install", index + 1)
                except ValueError:
                    continue
                for spec in words[install + 1 :]:
                    if spec.startswith("-") or spec in ("RUN",):
                        continue
                    if "$" in spec or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+()=*-]*", spec):
                        continue
                    yield spec
                break


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sources", action="store_true")
    parser.add_argument("--arch", choices=("x86_64", "aarch64"),
                        help="target RPM architecture (default: native host)")
    args = parser.parse_args()
    if args.sources:
        print("\n".join(SOURCES))
        return 0
    native = platform.machine().lower()
    arch = args.arch or {"amd64": "x86_64", "arm64": "aarch64"}.get(native, native)
    if arch not in ("x86_64", "aarch64"):
        parser.error(f"unsupported native architecture {native!r}; specify --arch")
    print("\n".join(sorted({spec for source in SOURCES for spec in packages(REPO / source, arch)})))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
