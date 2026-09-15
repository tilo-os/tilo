#!/usr/bin/env python3
"""Resolve an image-owned hardware profile before the UKI is measured/signed.

Usage: uki-hardware-profile.py ROOT KERNEL_VERSION ARCHITECTURE

Print the validated DTB path under ROOT, or nothing for a generic image. The
profile and DTB are immutable rootfs inputs, so their bytes are also covered by
the composefs digest. This is a build-time input, never a target-side path API.
"""

import hashlib
import json
import re
import stat
import struct
import sys
from pathlib import Path
from hardware_profile import ARCH_CONFIG, DEFINITION_PATH, read_definition


PROFILE = Path("usr/lib/qos/hardware-profile.json")
FIELDS = {"schema_version", "id", "architecture", "devicetree", "devicetree_sha256"}


def trusted_path(root: Path, relative: Path, *, regular: bool) -> Path:
    """Reject writable, non-root-owned or redirected image inputs."""
    current = root
    for index, part in enumerate(("", *relative.parts)):
        if part:
            current = current / part
        info = current.lstat()
        if stat.S_ISLNK(info.st_mode):
            raise ValueError(f"hardware profile input contains a symlink: {current}")
        if info.st_uid != 0 or info.st_mode & 0o022:
            raise ValueError(f"hardware profile input must be root-owned and not group/world writable: {current}")
        want_file = regular and index == len(relative.parts)
        if want_file:
            if not stat.S_ISREG(info.st_mode):
                raise ValueError(f"hardware profile input is not a regular file: {current}")
        elif not stat.S_ISDIR(info.st_mode):
            raise ValueError(f"hardware profile input is not a directory: {current}")
    return current


def resolve_profile(root: Path, kver: str, architecture: str) -> Path | None:
    candidate = root / PROFILE
    # lexists behavior matters: a dangling symlink is an invalid profile, not
    # an instruction to silently fall back to the generic boot path.
    if not candidate.exists() and not candidate.is_symlink():
        return None
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]*", kver):
        raise ValueError("invalid kernel version for hardware profile")
    profile = trusted_path(root, PROFILE, regular=True)
    if profile.stat().st_size > 16384:
        raise ValueError("hardware profile is too large")
    data = json.loads(profile.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or set(data) != FIELDS:
        raise ValueError("hardware profile has missing or unknown fields")
    if type(data["schema_version"]) is not int or data["schema_version"] != 1:
        raise ValueError("unsupported hardware profile schema")
    definition = read_definition(trusted_path(root, DEFINITION_PATH, regular=True))
    if data["id"] != definition["id"]:
        raise ValueError("hardware profile id does not match its image-owned definition")
    if architecture not in ARCH_CONFIG or data["architecture"] != architecture or definition["architecture"] != architecture:
        raise ValueError("hardware profile requires a matching native image architecture")
    # The reviewed definition is copied into the sealed rootfs. It binds the
    # model to this kernel's DTB without model-specific branches in the builder.
    if definition["devicetree"] is None:
        if data["devicetree"] is not None or data["devicetree_sha256"] is not None:
            raise ValueError("firmware-provided device tree profile must not override a DTB")
        return None
    relative = Path(f"usr/lib/modules/{kver}/dtb") / definition["devicetree"]
    if data["devicetree"] != f"/{relative.as_posix()}":
        raise ValueError("hardware profile device tree does not match this kernel and model")
    digest = data["devicetree_sha256"]
    if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise ValueError("hardware profile has no valid pinned DTB SHA-256")
    dtb = trusted_path(root, relative, regular=True)
    payload = dtb.read_bytes()
    if len(payload) < 40 or struct.unpack_from(">II", payload) != (0xD00DFEED, len(payload)):
        raise ValueError("hardware profile device tree has an invalid flattened-device-tree header")
    if hashlib.sha256(payload).hexdigest() != digest:
        raise ValueError("hardware profile device tree does not match its pinned SHA-256")
    return dtb


def main() -> None:
    if len(sys.argv) != 4:
        raise SystemExit(__doc__.splitlines()[2])
    try:
        dtb = resolve_profile(Path(sys.argv[1]), sys.argv[2], sys.argv[3])
    except (OSError, ValueError, TypeError) as error:
        raise SystemExit(f"hardware profile rejected: {error}") from error
    if dtb is not None:
        print(dtb)


if __name__ == "__main__":
    main()
