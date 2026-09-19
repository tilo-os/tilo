#!/usr/bin/env python3
"""Bind native package ISA metadata to the release compiler policy and ELF arch."""
import argparse
import os
from pathlib import Path
import re
import sys
import tomllib

POLICY = {"x86_64": ("x86-64-v3", "v3", 62),
          "aarch64": ("generic", "armv8-a", 183)}


def build_flags(root, arch):
    cpu, _, _ = POLICY[arch]
    config = tomllib.loads((root / ".cargo/config.toml").read_text())
    target = f'cfg(all(target_arch = "{arch}", target_os = "linux"))'
    flags = config["target"][target]["rustflags"]
    if flags.count(f"target-cpu={cpu}") != 1 or any(
        "target-feature" in flag or ("target-cpu" in flag and flag != f"target-cpu={cpu}")
        for flag in flags
    ):
        raise ValueError(f"unaudited {arch} compiler ISA policy")
    return flags


def stage(manifest, binary, arch, output):
    _, level, machine = POLICY[arch]
    with binary.open("rb") as stream:
        header = stream.read(20)
    if (len(header) != 20 or header[:6] != b"\x7fELF\x02\x01"
            or int.from_bytes(header[18:20], "little") != machine):
        raise ValueError(f"{binary}: expected a 64-bit little-endian {arch} ELF")
    source = manifest.read_text()
    # Source manifests describe reusable app definitions. Each signed archive
    # contains exactly one native executable, so declare only that architecture.
    section = f'[package.isa]\n{arch} = "{level}"\n\n'
    source, count = re.subn(r"(?m)^\[package\.isa\][^\n]*\n[\s\S]*?(?=^\[|\Z)", section, source)
    if not count:
        source += "\n" + section
    if tomllib.loads(source)["package"]["isa"] != {arch: level}:
        raise ValueError("could not normalize package ISA metadata")
    output.write_text(source)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("flags", "stage"))
    parser.add_argument("--arch", choices=POLICY, required=True)
    parser.add_argument("--root", type=Path)
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.mode == "flags":
        for key in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_TARGET"):
            if os.environ.get(key):
                raise ValueError(f"{key} overrides the audited native package build policy")
        print("\x1f".join(build_flags(args.root, args.arch)))
    else:
        stage(args.manifest, args.binary, args.arch, args.output)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, OSError) as error:
        print(f"package ISA: {error}", file=sys.stderr)
        sys.exit(1)
