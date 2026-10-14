#!/usr/bin/env python3
"""Stage verified local firmware and an image-owned hardware profile.

Run inside the native image builder before dracut. All inputs and output
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
from hardware_profile import ARCH_CONFIG, DEFINITION_PATH, PROFILES_DIR, load_definition, validate_target_imgref


FIRMWARE_PATH = Path("usr/lib/firmware")
PROFILE_PATH = Path("usr/lib/tilo/hardware-profile.json")
PARSER_PATH = Path("usr/lib/tilo/hardware_profile.py")
DRACUT_PATH = Path("etc/dracut.conf.d/41-tilo-hardware.conf")
# Sorts after the image's own kargs.d drop-ins, so a board console named here
# is the last console= and becomes /dev/console.
KARGS_PATH = Path("usr/lib/bootc/kargs.d/10-hardware-profile.toml")
# Broadcom's sample station address. Vendor NVRAM files (the Orange Pi's
# AP6256 among them) ship it, and brcmfmac programs whatever the NVRAM names,
# so every board would share one MAC. The pinned bytes are checked as
# published; only the installed copy drops this line, and the radio keeps the
# address it was given.
SAMPLE_BROADCOM_MAC = b"macaddr=00:90:4c:c5:12:38"


def installed_firmware(destination: str, payload: bytes) -> bytes:
    if not destination.endswith(".txt"):
        return payload
    lines = payload.splitlines(keepends=True)
    return b"".join(line for line in lines if line.rstrip(b"\r\n") != SAMPLE_BROADCOM_MAC)


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


def module_state(kernel: Path, name: str) -> str:
    """Return ``builtin``, ``module``, or ``absent`` without following symlinks."""
    builtin = kernel / "modules.builtin"
    suffix = f"/{name}.ko"
    if builtin.exists() or builtin.is_symlink():
        builtin = checked_path(kernel, Path("modules.builtin"), required=True)
        for line in builtin.read_text(encoding="utf-8", errors="replace").splitlines():
            if line.endswith(suffix):
                return "builtin"
    for directory, dirnames, filenames in os.walk(kernel, followlinks=False):
        current = Path(directory)
        if current.is_symlink():
            dirnames.clear()
            continue
        dirnames[:] = [entry for entry in dirnames if not (current / entry).is_symlink()]
        for filename in filenames:
            candidate = current / filename
            if candidate.is_symlink() or not candidate.is_file():
                continue
            if filename == f"{name}.ko" or filename.startswith(f"{name}.ko."):
                return "module"
    return "absent"


def prepare_profile(profile: str, root: Path, kernel_version: str, firmware_dir: Path,
                    profiles_dir: Path = PROFILES_DIR, *, target_update_ref: str | None = None) -> None:
    if profile == "generic":
        return
    definition = load_definition(profile, profiles_dir)
    validate_target_imgref(target_update_ref, root=root)
    architecture = definition["architecture"]
    if platform.machine() != architecture:
        raise ValueError(f"{profile} hardware profile requires a native {architecture} builder")
    if os.geteuid() != 0:
        raise ValueError("hardware profile preparation must run as root inside the image builder")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]*", kernel_version):
        raise ValueError("invalid kernel version for hardware profile")

    kernel = Path(f"usr/lib/modules/{kernel_version}")
    config = checked_path(root, kernel / "config", required=True)
    if ARCH_CONFIG[architecture] not in config.read_text(encoding="utf-8").splitlines():
        raise ValueError(f"hardware profile requires a matching {architecture} kernel configuration")
    dtb_relative = kernel / "dtb" / definition["devicetree"] if definition["devicetree"] else None
    dtb = None
    if dtb_relative is not None:
        dtb = checked_path(root, dtb_relative, required=True).read_bytes()
        if len(dtb) < 40 or struct.unpack_from(">II", dtb) != (0xD00DFEED, len(dtb)):
            raise ValueError("hardware profile device tree has an invalid flattened-device-tree header")

    firmware_names = definition["firmware"]
    system_firmware = definition.get("system_firmware", [])
    # These files come from the image's reviewed distro RPM snapshot rather
    # than the board-local OEM artifact. Require their installed bytes before
    # writing any generated profile state, and explicitly carry them into the
    # pre-root initramfs where a modular display driver requests them.
    installed_system_firmware = []
    for path in system_firmware:
        logical = FIRMWARE_PATH / path
        representations = []
        for suffix in ("", ".xz", ".zst"):
            relative = Path(str(logical) + suffix)
            candidate = checked_path(root, relative, required=False)
            if candidate.exists() or candidate.is_symlink():
                representations.append((relative, checked_path(root, relative, required=True)))
        if not representations:
            raise ValueError(f"required system firmware is missing: {path}")
        if len(representations) != 1:
            raise ValueError(f"required system firmware has ambiguous installed representations: {path}")
        relative, installed = representations[0]
        if installed.stat().st_size == 0:
            raise ValueError(f"required system firmware is empty: {path}")
        installed_system_firmware.append(relative.relative_to(FIRMWARE_PATH).as_posix())
    manifest_path = checked_path(firmware_dir, Path("manifest.json"), required=True) if firmware_names else None
    manifest = {"schema_version": 1, "profile": profile, "files": {}}
    if manifest_path and manifest_path.stat().st_size > 16384:
        raise ValueError("firmware manifest is too large")
    if manifest_path:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if not isinstance(manifest, dict) or set(manifest) != {"schema_version", "profile", "files"}:
        raise ValueError("firmware manifest has missing or unknown fields")
    if type(manifest["schema_version"]) is not int or manifest["schema_version"] != 1:
        raise ValueError("unsupported firmware manifest schema")
    if manifest["profile"] != profile:
        raise ValueError("firmware manifest belongs to another hardware profile")
    hashes = manifest["files"]
    if not isinstance(hashes, dict) or set(hashes) != set(firmware_names):
        raise ValueError("firmware manifest must name exactly the hardware definition's firmware files")

    outputs: dict[Path, bytes] = {}
    for name, destination in firmware_names.items():
        digest = hashes[name]
        if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise ValueError(f"firmware manifest has no valid SHA-256 for {name}")
        payload = checked_path(firmware_dir, Path(name), required=True).read_bytes()
        if not payload or hashlib.sha256(payload).hexdigest() != digest:
            raise ValueError(f"firmware does not match its pinned SHA-256: {name}")
        outputs[FIRMWARE_PATH / destination] = installed_firmware(destination, payload)

    document = {
        "schema_version": 1,
        "id": profile,
        "architecture": architecture,
        "devicetree": f"/{dtb_relative.as_posix()}" if dtb_relative else None,
        "devicetree_sha256": hashlib.sha256(dtb).hexdigest() if dtb else None,
        "target_update_ref": target_update_ref,
    }
    outputs[PROFILE_PATH] = (json.dumps(document, indent=2) + "\n").encode("utf-8")
    outputs[DEFINITION_PATH] = (json.dumps(definition, indent=2) + "\n").encode("utf-8")
    # The runtime backend validates the definition with this parser. Keep
    # both in the same image generation; an older parser rejects schema 2
    # definitions even when their firmware and DTB bytes are valid.
    parser = Path(__file__).with_name("hardware_profile.py").read_bytes().replace(b"\r\n", b"\n")
    if not parser or b"\r" in parser or b"\0" in parser:
        raise ValueError("hardware profile parser is not a valid LF script")
    outputs[PARSER_PATH] = parser
    initramfs_firmware = [*firmware_names.values(), *installed_system_firmware]
    modular_boot = []
    for name in definition.get("boot_modules", []):
        state = module_state(config.parent, name)
        if state == "absent":
            raise ValueError(f"hardware profile boot module is not built in or installed: {name}")
        if state == "module":
            modular_boot.append(name)
    dracut_lines = ["# Hardware-profile firmware and boot modules needed before the root is mounted."]
    if initramfs_firmware:
        firmware_paths = " ".join(f"/{(FIRMWARE_PATH / name).as_posix()}" for name in initramfs_firmware)
        dracut_lines.append(f'install_items+=" {firmware_paths} "')
    if modular_boot:
        dracut_lines.append(f'add_drivers+=" {" ".join(modular_boot)} "')
    outputs[DRACUT_PATH] = ("\n".join(dracut_lines) + "\n").encode("utf-8")
    # bootc container ukify (os/dev/uki.sh) seals kargs.d into the UKI command
    # line, so the board's arguments are measured with the rest of the image's.
    kernel_args = definition.get("kernel_args", [])
    if kernel_args:
        outputs[KARGS_PATH] = ("# Kernel arguments of the hardware profile.\nkargs = "
                               + json.dumps(kernel_args) + "\n").encode("utf-8")
    if "wifi_wake_on_wlan" in definition:
        # Validation binds this quirk to the Lenovo 83ED/WCN7850 definition.
        # A connection's explicit wake flags still override an NM default.
        outputs[Path("usr/lib/NetworkManager/conf.d/30-tilo-lenovo-wifi.conf")] = (
            "# Lenovo 83ED WCN7850 WoW resume can leave MHI unusable.\n"
            "# Disable WoWLAN by default; no profile mutation.\n"
            "[connection-tilo-lenovo-wifi]\n"
            "match-device=driver:ath12k_wifi7_pci\n"
            f"wifi.wake-on-wlan={definition['wifi_wake_on_wlan']}\n"
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
    parser.add_argument("--profile", required=True)
    parser.add_argument("--profiles-dir", type=Path, default=PROFILES_DIR)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--kernel-version", required=True)
    parser.add_argument("--firmware-dir", type=Path, required=True)
    parser.add_argument("--target-update-ref")
    args = parser.parse_args()
    try:
        prepare_profile(args.profile, args.root, args.kernel_version, args.firmware_dir, args.profiles_dir,
                        target_update_ref=args.target_update_ref)
    except (OSError, ValueError, TypeError) as error:
        parser.exit(1, f"hardware profile rejected: {error}\n")


if __name__ == "__main__":
    main()
