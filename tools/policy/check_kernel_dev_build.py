#!/usr/bin/env python3
"""Keep the dev kernel build (`tilo-vm swap kernel`) on the release build's path.

os/kernel/dev-build.sh builds the kernel incrementally for dev VMs. It must
build the same kernel as os/kernel/Containerfile, so this check fails when the
two could diverge:

  * the dev build's builder image is the Containerfile's `build-toolchain`
    stage, and that stage still ends before any Tilo input is applied;
  * both run the same os/kernel/build-support.sh commands (prepare-source,
    compile, stage-modules), in the same order;
  * every input the Containerfile COPYs to /work for those commands is
    mounted at the same /work path in the dev build;
  * the dev build's environment is the Containerfile's ENV block, plus only
    the documented LOCALVERSION suffix;
  * tilo-vmd builds a swapped kernel's initramfs with the dracut arguments
    os/scripts/build-initramfs.sh uses for the image's initramfs.

Run: python3 tools/policy/check_kernel_dev_build.py
"""

from __future__ import annotations

import re
import shlex
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
CONTAINERFILE = REPO / "os/kernel/Containerfile"
DEV_BUILD = REPO / "os/kernel/dev-build.sh"
IMAGE_INITRAMFS = REPO / "os/scripts/build-initramfs.sh"
VMD_BOOT = REPO / "crates/dev/tilo-vmd/src/boot.rs"

SHARED_COMMANDS = ("prepare-source", "compile", "stage-modules")
# Allowed only in the dev build, and why (see the header of dev-build.sh).
DEV_ONLY_ENV = {"LOCALVERSION": "+dev"}


def fail(errors: list[str], message: str) -> None:
    errors.append(message)


def helper_commands(text: str) -> list[str]:
    """build-support.sh commands in order of appearance, first use only."""
    seen: list[str] = []
    for match in re.finditer(r"kernel-build-support\.sh (\S+)", text):
        command = match.group(1)
        if command in SHARED_COMMANDS and command not in seen:
            seen.append(command)
    return seen


def containerfile_env(text: str) -> dict[str, str]:
    """The ENV block of the `build` stage, with SOURCE_DATE_EPOCH at its default."""
    match = re.search(r"^ENV (KBUILD_BUILD_TIMESTAMP=.*?)(?:\n\n|\n#|\nRUN)", text, re.M | re.S)
    if not match:
        return {}
    epoch = re.search(r"^ARG SOURCE_DATE_EPOCH=(\S+)", text, re.M)
    body = match.group(1).replace("\\\n", " ")
    body = body.replace("${SOURCE_DATE_EPOCH}", epoch.group(1) if epoch else "0")
    env = {}
    for word in shlex.split(body):
        key, _, value = word.partition("=")
        env[key] = value
    return env


def dev_env(text: str) -> dict[str, str]:
    env = {}
    for match in re.finditer(r"-e ([A-Z_]+)=(\S+)", text):
        env[match.group(1)] = match.group(2).strip("\"'")
    return env


def image_dracut_args(text: str) -> list[str]:
    """The image initramfs's dracut invocation (os/scripts/build-initramfs.sh), minus
    the per-call words (`--force -v --kver "$kver" <output>`)."""
    match = re.search(r"dracut --force (.*?)\"/usr/lib/modules/\$kver/initramfs\.img\"", text, re.S)
    if not match:
        return []
    words = shlex.split(match.group(1).replace("\\\n", " "))
    out: list[str] = []
    skip = False
    for word in words:
        if skip:
            skip = False
            continue
        if word == "--kver":
            skip = True
            continue
        if word == "-v":
            continue
        out.append(word)
    return out


def vmd_dracut_args(text: str) -> list[str]:
    match = re.search(r"pub const IMAGE_DRACUT_ARGS: &\[&str\] = &\[(.*?)\];", text, re.S)
    return re.findall(r'"([^"]*)"', match.group(1)) if match else []


def main() -> int:
    errors: list[str] = []
    containerfile = CONTAINERFILE.read_text()
    dev = DEV_BUILD.read_text()

    # The builder is the Containerfile's toolchain stage, untouched by Tilo inputs.
    if "--target build-toolchain" not in dev:
        fail(errors, "os/kernel/dev-build.sh no longer builds os/kernel/Containerfile's "
                     "`build-toolchain` stage as its builder image")
    # Both builds start from the same pinned, signed kernel buildroot.
    if ("os/kernel-buildroot-snapshot.env" not in dev
            or '--build-arg "KERNEL_BUILDROOT_REF=$KERNEL_BUILDROOT_REF"' not in dev):
        fail(errors, "os/kernel/dev-build.sh no longer builds on the pinned kernel buildroot "
                     "(os/kernel-buildroot-snapshot.env) the release build uses")
    stage = re.search(r"^FROM \$\{KERNEL_BUILDROOT_REF\} AS build-toolchain$(.*?)^FROM ",
                      containerfile, re.M | re.S)
    if not stage:
        fail(errors, "os/kernel/Containerfile has no `build-toolchain` stage followed by another stage")
    else:
        instructions = "\n".join(line for line in stage.group(1).splitlines()
                                 if not line.lstrip().startswith("#"))
        for tilo_input in ("config-fragments", "kernel-patches", "build-support.sh", "module-signing"):
            if tilo_input in instructions:
                fail(errors, f"os/kernel/Containerfile's build-toolchain stage uses {tilo_input}; "
                             "the dev build mounts Tilo inputs itself, so keep them in the `build` stage")

    # The same shared steps, in the same order.
    release_steps = helper_commands(containerfile)
    dev_steps = helper_commands(dev)
    if release_steps != list(SHARED_COMMANDS):
        fail(errors, f"os/kernel/Containerfile runs build-support.sh {release_steps}, expected "
                     f"{list(SHARED_COMMANDS)}")
    if dev_steps != release_steps:
        fail(errors, f"os/kernel/dev-build.sh runs build-support.sh {dev_steps}, but the release "
                     f"build runs {release_steps}")

    # Every /work input the release build copies in, the dev build mounts.
    for source, target in re.findall(r"^COPY (os/kernel/\S+) (/work/\S+)$", containerfile, re.M):
        if source.endswith("tilo-kernel-payload.spec") or source.endswith("project-crate.py"):
            continue
        mount = f'"$repo/{source.rstrip("/")}:{target.rstrip("/")}'
        if mount not in dev:
            fail(errors, f"os/kernel/dev-build.sh does not mount {source} at {target} "
                         "like os/kernel/Containerfile copies it")

    # The build environment.
    release_env = containerfile_env(containerfile)
    if not release_env:
        fail(errors, "os/kernel/Containerfile has no ENV block starting with KBUILD_BUILD_TIMESTAMP")
    development_env = dev_env(dev)
    for key, value in release_env.items():
        if development_env.get(key) != value:
            fail(errors, f"os/kernel/dev-build.sh sets {key}={development_env.get(key)!r}, "
                         f"the release build {value!r}")
    for key, value in development_env.items():
        if key in release_env:
            continue
        if DEV_ONLY_ENV.get(key) != value:
            fail(errors, f"os/kernel/dev-build.sh sets {key}={value!r}, which the release build "
                         "does not; add it to both or document it in DEV_ONLY_ENV here")

    # The swapped kernel's initramfs.
    image_args = image_dracut_args(IMAGE_INITRAMFS.read_text())
    vmd_args = vmd_dracut_args(VMD_BOOT.read_text())
    if not image_args:
        fail(errors, "found no image initramfs dracut call in os/scripts/build-initramfs.sh")
    elif image_args != vmd_args:
        fail(errors, "tilo-vmd's IMAGE_DRACUT_ARGS (crates/dev/tilo-vmd/src/boot.rs) differ from "
                     f"os/scripts/build-initramfs.sh's dracut arguments:\n    image:    {image_args}\n"
                     f"    tilo-vmd: {vmd_args}")

    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        print("\nThe dev kernel build must stay on the release build's path: see the header of "
              "os/kernel/dev-build.sh and `tilo-vm guide kernel`.", file=sys.stderr)
        return 1
    print("os/kernel/dev-build.sh and tilo-vmd follow the release kernel and initramfs builds")
    return 0


if __name__ == "__main__":
    sys.exit(main())
