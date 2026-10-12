#!/usr/bin/env python3
"""The runner labels CI jobs ask for, and what each one means.

Workflows name these labels in `runs-on`, and CI helpers emit them into job
matrices through the constants below. tools/ci/README.md documents the same
table; tools/tests/test_runner_labels.py keeps both, and every label a
workflow uses, in step.

A fork keeps the names and registers runners that carry them (a Forgejo or
GitHub self-hosted runner accepts any label list), or replaces them with the
`hosted` equivalent. Run this file to print the table.
"""
from __future__ import annotations

from typing import NamedTuple


class Label(NamedTuple):
    arch: str
    purpose: str
    hosted: str


LABELS = {
    "runner": Label(
        "x86_64", "General jobs and native Rust builds: 4-5 CPUs, 10 GiB memory, 50 GiB disk, Docker",
        "ubuntu-24.04"),
    "runner-arm64": Label(
        "aarch64", "`runner` on native ARM64", "ubuntu-24.04-arm"),
    "runner-native-portable": Label(
        "x86_64/aarch64", "Host-independent native SDK and WASM qualification: at least 22 GiB memory, Docker",
        "self-hosted on either native architecture"),
    "runner-quality": Label(
        "x86_64", "Lint, change selection and result aggregation: 2 CPUs, 4 GiB memory",
        "ubuntu-24.04"),
    "runner-large": Label(
        "x86_64", "Whole-collection compiles: 12 CPUs, 32 GiB memory, 100 GiB disk",
        "a 16-core larger runner"),
    "runner-large-arm64": Label(
        "aarch64", "`runner-large` on native ARM64", "a 16-core ARM64 larger runner"),
    "runner-disk-100g": Label(
        "x86_64", "`runner` with 100 GiB of disk for image and archive exports",
        "ubuntu-24.04 with extra disk"),
    "runner-disk-100g-arm64": Label(
        "aarch64", "`runner-disk-100g` on native ARM64", "ubuntu-24.04-arm with extra disk"),
    "runner-release": Label(
        "x86_64", "OS image publication: large class, FUSE and loop devices, one slot per architecture",
        "self-hosted"),
    "runner-release-arm64": Label(
        "aarch64", "`runner-release` on native ARM64", "self-hosted"),
    "runner-native-storage": Label(
        "x86_64", "Filesystem tests on a host kernel: Linux 6.14+ with pre-content fanotify, Btrfs, loop devices",
        "self-hosted"),
    "imager-linux-x86_64": Label(
        "x86_64", "Imager release builds: /dev/kvm and a DRM render node", "self-hosted"),
    "imager-linux-aarch64": Label(
        "aarch64", "`imager-linux-x86_64` on native ARM64", "self-hosted"),
    "imager-windows-x86_64": Label(
        "x86_64", "Imager release builds on Windows with signing tools", "self-hosted"),
    "imager-windows-aarch64": Label(
        "aarch64", "`imager-windows-x86_64` on native ARM64", "self-hosted"),
    "imager-macos-x86_64": Label(
        "x86_64", "Imager and runtime builds on Intel macOS with Xcode", "macos-15-intel"),
    "imager-macos-aarch64": Label(
        "aarch64", "Imager and runtime builds on Apple silicon with Xcode", "macos-15"),
}

NATIVE = {"x86_64": "runner", "aarch64": "runner-arm64"}
LARGE = {"x86_64": "runner-large", "aarch64": "runner-large-arm64"}


def table() -> str:
    rows = ["| Label | Architecture | Runs | Hosted equivalent |", "|---|---|---|---|"]
    rows += [f"| `{name}` | {label.arch} | {label.purpose} | {label.hosted} |"
             for name, label in LABELS.items()]
    return "\n".join(rows) + "\n"


if __name__ == "__main__":
    print(table(), end="")
