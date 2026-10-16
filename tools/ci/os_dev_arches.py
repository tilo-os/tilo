#!/usr/bin/env python3
"""Which architectures a Build / OS dev run builds.

Main pushes build in Build / OS dev (x86_64) and (aarch64) (os-dev-x86.yml,
os-dev-arm.yml), newest commit only; a push here is a manual run. A pull
request builds
x86_64, and aarch64 as well when its own diff (from its real merge base)
touches an architecture-specific input or it carries the `os-arm64` label.

    os_dev_arches.py --event push
    os_dev_arches.py --event pull_request --base SHA --head SHA [--label NAME ...]

Prints a JSON list and writes `arches=<list>` to $GITHUB_OUTPUT when set.
"""
import argparse
import fnmatch
import json
import os
import subprocess
import sys

# Inputs whose effect differs by architecture: the kernel and its
# configuration, board profiles and firmware, the per-architecture pins, the
# UKI assembly and the ARM-only Windows (FEX) provider.
ARCH_PATHS = (
    "os/kernel/*",
    "os/hardware/*",
    "os/*-snapshot.env",
    "os/dev/uki*",
    "os/dev/hardware_profile.py",
    "os/dev/prepare-hardware-profile.py",
    "os/Containerfile.board",
    "os/dev/*fex*",
    "os/external-assets/fex-*",
    "os/dev/windows_provider.py",
)
LABEL = "os-arm64"


def arch_specific(paths):
    return sorted(p for p in paths if any(fnmatch.fnmatchcase(p, pattern) for pattern in ARCH_PATHS))


def changed_paths(base, head):
    ancestor = subprocess.run(["git", "merge-base", base, head], check=True,
                              capture_output=True, text=True).stdout.strip()
    out = subprocess.run(["git", "diff", "--name-only", ancestor, head], check=True,
                         capture_output=True, text=True).stdout
    return [line for line in out.splitlines() if line]


def arches(event, labels=(), paths=()):
    if event == "pull_request" and (LABEL in labels or arch_specific(paths)):
        return ["x86_64", "aarch64"]
    return ["x86_64"]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--event", required=True)
    parser.add_argument("--base")
    parser.add_argument("--head")
    parser.add_argument("--label", action="append", default=[])
    args = parser.parse_args()
    paths = changed_paths(args.base, args.head) if args.event == "pull_request" else []
    result = arches(args.event, args.label, paths)
    print(json.dumps({"arches": result, "arch_specific": arch_specific(paths), "labels": args.label}))
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a") as stream:
            stream.write(f"arches={json.dumps(result)}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
