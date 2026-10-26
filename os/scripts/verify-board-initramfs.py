#!/usr/bin/env python3
"""Check a board image's initramfs against its hardware profile.

    verify-board-initramfs.py KVER

build-initramfs.sh runs this right after dracut when the image carries a
hardware-profile definition (/usr/lib/tilo/hardware-profile-definition.json,
written by os/dev/prepare-hardware-profile.py); generic images have none and
skip it. It reads the initramfs back with lsinitrd, which needs no privileges,
and requires:

  * every firmware file the profile names (its local firmware destinations
    and its distro `system_firmware`), uncompressed, XZ or Zstandard;
  * every `boot_modules` entry that the kernel builds as a module, at the
    path it has under /usr/lib/modules/KVER (built-in ones must be listed in
    modules.builtin, so a renamed or dropped driver fails here too);
  * for a profile whose internal keyboard is on an i2c bus, the bus, its HID
    transport and the input layer built into the kernel (modules.builtin):
    no dracut module loads them, so a modular driver leaves a prompt that
    takes no key;
  * when the sibling-unlock unit is in the initramfs, what its network needs
    for Wi-Fi: NetworkManager, its Wi-Fi device plugin, wpa_supplicant and
    the D-Bus activation file NetworkManager starts it through. The dracut
    module installs those optionally, so a missing package would otherwise
    leave an initramfs that unlocks over Ethernet only, silently.

It proves presence in the initramfs, not that the radio associates: that
takes the board itself.
"""

from __future__ import annotations

import argparse
import fnmatch
import json
import os
import subprocess
import sys
from pathlib import Path

DEFINITION = Path("usr/lib/tilo/hardware-profile-definition.json")
FIRMWARE = "usr/lib/firmware"
FIRMWARE_SUFFIXES = ("", ".xz", ".zst")
SIBLING_UNLOCK_UNIT = "usr/lib/systemd/system/tilo-sibling-unlock@.service"
# What the sibling-unlock network needs for Wi-Fi, as patterns over the
# initramfs's paths (usr/sbin is a symlink to usr/bin on Fedora).
WIFI_UNLOCK = {
    "NetworkManager": ("usr/bin/NetworkManager", "usr/sbin/NetworkManager"),
    "NetworkManager's Wi-Fi device plugin": ("usr/lib*/NetworkManager/*/libnm-device-plugin-wifi.so",),
    "wpa_supplicant": ("usr/bin/wpa_supplicant", "usr/sbin/wpa_supplicant"),
    "wpa_supplicant's D-Bus activation": ("usr/share/dbus-1/system-services/fi.w1.wpa_supplicant1.service",),
}
# What an internal i2c keyboard needs built in, named as their objects are in
# modules.builtin (prepare-hardware-profile.py reads it the same way), with
# the Kconfig symbol each one is set through. Systemd reads keys through the
# event device, and Fedora's kernel has every one of these as a module: the
# GENI controller the X1E80100's buses are, the HID transport over it, HID
# core, and the input layer.
I2C_INPUT_BUILTIN = {
    "i2c-qcom-geni": "CONFIG_I2C_QCOM_GENI",
    "i2c-hid-of": "CONFIG_I2C_HID_OF",
    "i2c-core": "CONFIG_I2C",
    "hid-core": "CONFIG_HID",
    "evdev": "CONFIG_INPUT_EVDEV",
    "atkbd": "CONFIG_INPUT_KEYBOARD",
}


def listed_paths(listing: str) -> set[str]:
    """The paths in lsinitrd's output, relative to the initramfs root.

    lsinitrd prints `ls -l`-style lines (cpio -tv), and for a squashfs-wrapped
    image also unsquashfs -ll lines below `squashfs-root/`.
    """
    paths = set()
    for line in listing.splitlines():
        fields = line.split()
        if len(fields) < 6 or len(fields[0]) != 10 or fields[0][0] not in "-dlcbps":
            continue
        if fields[0][0] == "l" and "->" in fields:
            path = fields[fields.index("->") - 1]
        else:
            path = fields[-1]
        path = path.removeprefix("./").removeprefix("squashfs-root/")
        paths.add(path)
    return paths


def module_files(kernel: Path) -> dict[str, str]:
    """Module name -> its path relative to the image root, for KVER's modules."""
    found = {}
    for directory, _, filenames in os.walk(kernel):
        for filename in filenames:
            name, dot, _ = filename.partition(".ko")
            if dot and filename[len(name):] in (".ko", ".ko.xz", ".ko.zst", ".ko.gz"):
                found.setdefault(name, (Path(directory) / filename).relative_to(kernel.parents[3]).as_posix())
    return found


def builtin_modules(kernel: Path) -> set[str]:
    """The object names of what the kernel builds into itself."""
    builtin = kernel / "modules.builtin"
    if not builtin.is_file():
        return set()
    return {Path(line.strip()).name.removesuffix(".ko")
            for line in builtin.read_text(encoding="utf-8", errors="replace").splitlines() if line.strip()}


def builtin_input_bus(definition: dict, builtin: set[str]) -> list[str]:
    """The built-in drivers an internal keyboard's bus needs, and does not have."""
    if not any(keyboard.get("bus") == "i2c" for keyboard in definition.get("internal_keyboards", [])):
        return []
    return [f"internal i2c keyboard: {symbol} is not built into the kernel, "
            "so the initramfs has no driver to load at the unlock prompt"
            for name, symbol in I2C_INPUT_BUILTIN.items() if name not in builtin]


def problems(definition: dict, paths: set[str], root: Path, kver: str) -> list[str]:
    found = []
    firmware = [*definition.get("firmware", {}).values(), *definition.get("system_firmware", [])]
    for name in firmware:
        if not any(f"{FIRMWARE}/{name}{suffix}" in paths for suffix in FIRMWARE_SUFFIXES):
            found.append(f"firmware {name} is not in the initramfs")
    kernel = root / "usr/lib/modules" / kver
    modules = module_files(kernel)
    builtin = builtin_modules(kernel)
    for name in definition.get("boot_modules", []):
        if name in modules:
            if modules[name] not in paths:
                found.append(f"boot module {name} ({modules[name]}) is not in the initramfs")
        elif name not in builtin:
            found.append(f"boot module {name} is neither built into kernel {kver} nor installed for it")
    found.extend(builtin_input_bus(definition, builtin))
    if SIBLING_UNLOCK_UNIT in paths:
        for what, patterns in WIFI_UNLOCK.items():
            if not any(fnmatch.filter(paths, pattern) for pattern in patterns):
                found.append(f"sibling unlock is in the initramfs but {what} is not, so it cannot use Wi-Fi")
    return found


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("kver")
    parser.add_argument("--root", type=Path, default=Path("/"))
    parser.add_argument("--initramfs", type=Path, help="default: ROOT/usr/lib/modules/KVER/initramfs.img")
    args = parser.parse_args()
    definition_path = args.root / DEFINITION
    if not definition_path.is_file():
        return 0
    definition = json.loads(definition_path.read_text(encoding="utf-8"))
    image = args.initramfs or args.root / "usr/lib/modules" / args.kver / "initramfs.img"
    listing = subprocess.run(["lsinitrd", str(image)], check=True, capture_output=True, text=True).stdout
    paths = listed_paths(listing)
    # An unreadable image lists nothing; that must not pass as "nothing missing".
    if f"usr/lib/modules/{args.kver}/modules.dep" not in paths:
        print(f"lsinitrd listed no modules.dep for {args.kver} in {image}; cannot check it", file=sys.stderr)
        return 1
    found = problems(definition, paths, args.root, args.kver)
    for problem in found:
        print(f"{definition['id']} initramfs: {problem}", file=sys.stderr)
    if found:
        return 1
    print(f"{definition['id']} initramfs carries its profile's firmware and boot modules"
          + (" and Wi-Fi for sibling unlock" if SIBLING_UNLOCK_UNIT in paths else ""))
    return 0


if __name__ == "__main__":
    sys.exit(main())
