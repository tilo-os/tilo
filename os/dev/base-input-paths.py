#!/usr/bin/env python3
"""The paths a local production base build reads, one per line, relative to the repository.

os/dev/build-dev-image.sh rebuilds the base (os/dev/build-image.sh, TILO_IMAGE_ONLY=1) when
the files under these paths change, and skips it otherwise. They are:

  * what os/Containerfile takes from the build context: the sources of its COPY
    instructions without --from and of its RUN bind mounts without from=;
  * what the compile (os/image/compile.sh, run in the checkout) reads: the workspace, the
    repository files its crates include from outside it, and the scripts it runs;
  * the scripts and pins the build runs before podman does: os/dev, os/image, os/scripts,
    os/kernel, the os/*.env pins, and every os/Containerfile*, which the RPM package set is
    derived from (os/dev/derive-rpm-package-set.py);
  * .containerignore, which shapes the build context.

Everything else (docs, the VM acceptance harness, CI and tool scripts, the dev image's own
sources) cannot change the base, so a change there reuses it.

  base-input-paths.py [--repo DIR]     the paths, then the files within them to leave out,
                                      each of those prefixed with "!"
"""

from __future__ import annotations

import argparse
import re
import shlex
from pathlib import Path

FIXED = [
    ".containerignore",
    "os/dev",
    "os/image",
    "os/scripts",
    "os/kernel",
]
# What os/image/compile.sh reads of the checkout besides os/: the workspace, the files its
# crates include from outside it, and the tools its build steps run.
COMPILE = [
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    ".cargo",
    "keys/tpm2-pcr-signing.public.pem",
    "crates",
    "apps",
    "examples",
    "sdk/python/native",
    "sdk/typescript/native",
    "vendor",
    "dist",
    "docs/developers/tasks",
    "scripts/browser",
    "tools/apps",
    "tools/ci/app-models.sh",
    "tools/ci/measure.sh",
    "tools/ci/runtime-dylib-lazy-link.py",
    "tools/ci/rustc-thin-app-wrapper.sh",
    "tools/ci/thin-app",
    "tools/font-catalog.py",
    "tools/policy/app_capability_usage.toml",
    "tools/protocols/airdrop_sender.py",
    "tools/test-assets-build.rs",
    "tools/test-assets.py",
    "tools/web/wasm-opt-flags",
    "tools/web/wasm-split",
]
# Inputs of the dev image alone (os/Containerfile.dev and its build), which the base never reads.
DEV_ONLY = {
    "crates/dev/tilo-vm",
    "crates/dev/tilo-vmd/data",
    "os/Containerfile.dev",
    "os/dev/build-dev-image.sh",
    "os/dev/base-input-paths.py",
    "os/scripts/unseal-rootfs.py",
    "os/scripts/check-dev-kernel.sh",
    "os/scripts/finalize-dev-image.py",
}


def instructions(text: str) -> list[str]:
    """The Containerfile's instructions with continuation lines joined and comments dropped."""
    out: list[str] = []
    current = ""
    for line in text.splitlines():
        stripped = line.strip()
        if not current and (not stripped or stripped.startswith("#")):
            continue
        if current and stripped.startswith("#"):
            continue
        if stripped.endswith("\\"):
            current += stripped[:-1] + " "
            continue
        out.append(current + stripped)
        current = ""
    if current:
        out.append(current)
    return out


def context_sources(containerfile: Path) -> set[str]:
    sources: set[str] = set()
    for instruction in instructions(containerfile.read_text()):
        word, _, rest = instruction.partition(" ")
        word = word.upper()
        if word in {"COPY", "ADD"}:
            args = shlex.split(rest, posix=True)
            if any(a.startswith("--from") for a in args):
                continue
            paths = [a for a in args if not a.startswith("--")]
            sources.update(paths[:-1])
        elif word == "RUN":
            for mount in re.findall(r"--mount=(\S+)", rest):
                fields = dict(f.split("=", 1) for f in mount.split(",") if "=" in f)
                if fields.get("type") == "bind" and "from" not in fields and "source" in fields:
                    sources.add(fields["source"])
    out = set()
    for source in sources:
        # A path chosen by a build argument (os/hardware/firmware/${TILO_HARDWARE_PROFILE}): the
        # directory above the first expanded part, which holds every choice.
        if "$" in source:
            source = source[: source.index("$")].rsplit("/", 1)[0]
        out.add(source.strip("/").removeprefix("./") or ".")
    return out


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args()
    repo: Path = args.repo
    paths = set(FIXED) | set(COMPILE) | context_sources(repo / "os/Containerfile")
    paths |= {p.relative_to(repo).as_posix() for p in (repo / "os").glob("Containerfile*")}
    paths |= {p.relative_to(repo).as_posix() for p in (repo / "os").glob("*.env")}
    paths -= DEV_ONLY
    if "." in paths:
        raise SystemExit("os/Containerfile copies the whole build context; every file is a base input")
    for path in sorted(paths):
        print(path)
    # Within the paths above, but read by the dev image alone.
    for path in sorted(DEV_ONLY):
        print(f"!{path}")


if __name__ == "__main__":
    main()
