#!/usr/bin/env python3
"""Stage verified, local Lenovo firmware and its image-owned boot profile.

Run inside the native ARM64 image builder before dracut. All inputs and output
paths are validated before any writes. Generic images need no local firmware.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import stat
import struct
from pathlib import Path


FIRMWARE_NAMES = (
    "qcdxkmsuc8380.mbn",
    "qcvss8380.mbn",
    "qcadsp8380.mbn",
    "adsp_dtbs.elf",
    "qccdsp8380.mbn",
    "cdsp_dtbs.elf",
)
FIRMWARE_PATH = Path("usr/lib/firmware/qcom/x1e80100/LENOVO/83ED")
PROFILE_PATH = Path("usr/lib/qos/hardware-profile.json")
DRACUT_PATH = Path("etc/dracut.conf.d/41-qos-hardware.conf")


def checked_path(root: Path, relative: Path, *, required: bool) -> Path:
    """Reject redirects and nonregular files within a trusted directory."""
    if relative.is_absolute() or ".." in relative.parts:
        raise ValueError("hardware profile path must stay within its root")
    current = root
    for index, component in enumerate(("", *relative.parts)):
        if component:
            current = current / component
        try:
            info = current.lstat()
        except FileNotFoundError:
            if required or index == 0:
                raise ValueError(f"hardware profile input is missing: {current}") from None
            continue
        if stat.S_ISLNK(info.st_mode):
            raise ValueError(f"hardware profile path contains a symlink: {current}")
        if index == len(relative.parts):
            if not stat.S_ISREG(info.st_mode):
                raise ValueError(f"hardware profile path is not a regular file: {current}")
        elif not stat.S_ISDIR(info.st_mode):
            raise ValueError(f"hardware profile path is not a directory: {current}")
    return current


def prepare_profile(profile: str, root: Path, kernel_version: str, firmware_dir: Path) -> None:
    if profile == "generic":
        return
    if profile != "lenovo-83ed":
        raise ValueError(f"unsupported hardware profile: {profile}")
    if platform.machine() != "aarch64":
        raise ValueError("Lenovo 83ED hardware profile requires a native aarch64 builder")
    if os.geteuid() != 0:
        raise ValueError("hardware profile preparation must run as root inside the image builder")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]*", kernel_version):
        raise ValueError("invalid kernel version for hardware profile")

    kernel = Path(f"usr/lib/modules/{kernel_version}")
    config = checked_path(root, kernel / "config", required=True)
    if "CONFIG_ARM64=y" not in config.read_text(encoding="utf-8").splitlines():
        raise ValueError("hardware profile requires an ARM64 kernel configuration")
    dtb_relative = kernel / "dtb/qcom/x1e80100-lenovo-yoga-slim7x.dtb"
    dtb = checked_path(root, dtb_relative, required=True).read_bytes()
    if len(dtb) < 40 or struct.unpack_from(">II", dtb) != (0xD00DFEED, len(dtb)):
        raise ValueError("hardware profile device tree has an invalid flattened-device-tree header")

    manifest_path = checked_path(firmware_dir, Path("manifest.json"), required=True)
    if manifest_path.stat().st_size > 16384:
        raise ValueError("firmware manifest is too large")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if not isinstance(manifest, dict) or set(manifest) != {"schema_version", "profile", "files"}:
        raise ValueError("firmware manifest has missing or unknown fields")
    if type(manifest["schema_version"]) is not int or manifest["schema_version"] != 1:
        raise ValueError("unsupported firmware manifest schema")
    if manifest["profile"] != profile:
        raise ValueError("firmware manifest belongs to another hardware profile")
    hashes = manifest["files"]
    if not isinstance(hashes, dict) or set(hashes) != set(FIRMWARE_NAMES):
        raise ValueError("firmware manifest must name exactly the six Lenovo 83ED firmware files")

    outputs: dict[Path, bytes] = {}
    for name in FIRMWARE_NAMES:
        digest = hashes[name]
        if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise ValueError(f"firmware manifest has no valid SHA-256 for {name}")
        payload = checked_path(firmware_dir, Path(name), required=True).read_bytes()
        if not payload or hashlib.sha256(payload).hexdigest() != digest:
            raise ValueError(f"firmware does not match its pinned SHA-256: {name}")
        outputs[FIRMWARE_PATH / name] = payload

    document = {
        "schema_version": 1,
        "id": profile,
        "architecture": "aarch64",
        "devicetree": f"/{dtb_relative.as_posix()}",
        "devicetree_sha256": hashlib.sha256(dtb).hexdigest(),
    }
    outputs[PROFILE_PATH] = (json.dumps(document, indent=2) + "\n").encode("utf-8")
    firmware_paths = " ".join(f"/{(FIRMWARE_PATH / name).as_posix()}" for name in FIRMWARE_NAMES)
    outputs[DRACUT_PATH] = (
        "# Lenovo 83ED firmware needed before the encrypted root is unlocked.\n"
        f'install_items+=" {firmware_paths} "\n'
    ).encode("utf-8")
    # Check every destination before creating directories or installing bytes.
    # This includes existing output files, including dangling symlinks.
    destinations = {
        checked_path(root, relative, required=False): payload
        for relative, payload in outputs.items()
    }
    for destination, payload in destinations.items():
        destination.parent.mkdir(parents=True, exist_ok=True, mode=0o755)
        destination.write_bytes(payload)
        destination.chmod(0o644)
        os.chown(destination, 0, 0)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("generic", "lenovo-83ed"), required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--kernel-version", required=True)
    parser.add_argument("--firmware-dir", type=Path, required=True)
    args = parser.parse_args()
    try:
        prepare_profile(args.profile, args.root, args.kernel_version, args.firmware_dir)
    except (OSError, ValueError, TypeError) as error:
        parser.exit(1, f"hardware profile rejected: {error}\n")


if __name__ == "__main__":
    main()
