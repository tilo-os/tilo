#!/usr/bin/env python3
"""Print the direct RPM specs consumed from qOS's frozen repository."""

from __future__ import annotations

import pathlib
import re
import shlex
import sys


REPO = pathlib.Path(__file__).resolve().parents[2]
SOURCES = (
    "os/Containerfile",
    "os/Containerfile.host",
    "os/dev/composefs/Containerfile",
    "os/containers/qos-relay/Containerfile",
    "os/kernel/Containerfile",
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


def packages(path: pathlib.Path):
    text = path.read_text()
    for marker in re.finditer(
        r"^\s*#\s*qos-rpm-snapshot-package:\s*(\S+)\s*$", text, re.MULTILINE
    ):
        spec = marker.group(1)
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+()=*-]*", spec):
            raise SystemExit(f"{path}: invalid snapshot-only package {spec!r}")
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
    if sys.argv[1:] == ["--sources"]:
        print("\n".join(SOURCES))
        return 0
    if sys.argv[1:]:
        raise SystemExit("usage: derive-rpm-package-set.py [--sources]")
    print("\n".join(sorted({spec for source in SOURCES for spec in packages(REPO / source)})))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
