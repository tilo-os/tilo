#!/usr/bin/env python3
"""Verify that checked-in vendor sources reproduce the standalone histories.

The winit crates.io source archive contains Cargo publication bookkeeping
that Tilo deliberately omits from that vendored tree. Tilo also adds one local
``cargo-machete`` annotation to winit's normalized manifest. The forks are
published under their own names (tilo-winit, tilo-vello, tilo-vello-encoding),
so their manifests carry a publication identity -- package name, description,
readme, packaged files, renamed dependencies -- and Tilo's own records,
``TILO-FORK.md`` and ``PROVENANCE.md``; those lines and files are set aside
before comparing. Everything else, including Vello's retained package files,
must match the recorded standalone tips byte-for-byte.
"""

from __future__ import annotations

import argparse
from pathlib import Path
import subprocess
import sys


PUBLICATION_FILES = {
    ".cargo-ok",
    ".cargo_vcs_info.json",
    "Cargo.lock",
    "Cargo.toml.orig",
}

WINIT_MACHETE_BLOCK = b'''[package.metadata.cargo-machete]
# Build-script, target-cfg, and feature-forwarding dependencies which source
# scanning cannot see. Keep this metadata-only delta separate from fork code.
ignored = [
    "cfg_aliases",
    "ndk",
    "wayland-backend",
    "wayland-protocols",
]

'''


FORK_FILES = {"TILO-FORK.md", "PROVENANCE.md"}

# Manifest lines that give a fork its crates.io identity.
IDENTITY_PREFIXES = (
    b"name = ",
    b"description = ",
    b"readme = ",
    b"exclude = ",
    b"# Cargo publication bookkeeping",
    b'    "/TILO-FORK.md",',
    b'package = "tilo-',
    b'path = "../',
)


def identity_neutral(manifest: bytes) -> bytes:
    """`manifest` without the lines that give a fork its identity (the
    `[lib]`/`[[bin]]`/`[[example]]` `name`s go too, on both sides)."""
    return b"\n".join(
        line for line in manifest.split(b"\n") if not line.startswith(IDENTITY_PREFIXES)
    )


def git(root: Path, *args: str) -> bytes:
    return subprocess.check_output(("git", "-C", str(root), *args))


def ref_files(
    root: Path,
    ref: str,
    prefixes: tuple[str, ...],
    exclude_publication_files: bool,
) -> set[str]:
    paths = git(root, "ls-tree", "-r", "--name-only", ref).decode().splitlines()
    return {
        path
        for path in paths
        if path.startswith(prefixes)
        and (not exclude_publication_files or Path(path).name not in PUBLICATION_FILES)
    }


def tracked_files(root: Path, prefixes: tuple[str, ...]) -> set[str]:
    paths = git(root, "ls-files", "--", *prefixes).decode().splitlines()
    return set(paths)


def compare_tree(
    root: Path,
    ref: str,
    ref_prefixes: tuple[str, ...],
    vendor_prefix: str,
    overlay_manifest: str | None = None,
    exclude_publication_files: bool = False,
) -> list[str]:
    expected = ref_files(root, ref, ref_prefixes, exclude_publication_files)
    actual = tracked_files(
        root,
        tuple(f"{vendor_prefix}/{prefix}" if prefix else vendor_prefix for prefix in ref_prefixes),
    )
    mapped_actual = {path.removeprefix(f"{vendor_prefix}/") for path in actual}
    errors: list[str] = []

    for path in sorted(expected - mapped_actual):
        errors.append(f"missing vendored file: {vendor_prefix}/{path}")
    for path in sorted(mapped_actual - expected):
        if Path(path).name in FORK_FILES:
            continue
        errors.append(f"unexpected vendored file: {vendor_prefix}/{path}")

    for path in sorted(expected & mapped_actual):
        wanted = git(root, "show", f"{ref}:{path}")
        vendored_path = root / vendor_prefix / path
        found = vendored_path.read_bytes()
        if path == overlay_manifest:
            count = found.count(WINIT_MACHETE_BLOCK)
            if count != 1:
                errors.append(
                    f"{vendored_path.relative_to(root)} has {count} copies of the documented "
                    "cargo-machete overlay (expected one)"
                )
                continue
            found = found.replace(WINIT_MACHETE_BLOCK, b"", 1)
        if Path(path).name == "Cargo.toml":
            found, wanted = identity_neutral(found), identity_neutral(wanted)
        if found != wanted:
            errors.append(f"vendored content differs: {vendored_path.relative_to(root)}")

    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--vello-ref", required=True)
    parser.add_argument("--winit-ref", required=True)
    args = parser.parse_args()
    root = Path(git(Path.cwd(), "rev-parse", "--show-toplevel").decode().strip())

    errors = compare_tree(
        root,
        args.vello_ref,
        ("vello/", "vello_encoding/"),
        "vendor",
    )
    errors.extend(
        compare_tree(
            root,
            args.winit_ref,
            ("",),
            "vendor/winit",
            overlay_manifest="Cargo.toml",
            exclude_publication_files=True,
        )
    )
    if errors:
        print("vendor tree verification failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1
    print("vendored sources match standalone histories and documented Tilo overlays")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
