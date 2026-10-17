#!/usr/bin/env python3
"""Turn a sealed Tilo image root back into an unsealed one, in place.

    unseal-rootfs.py [ROOT]            (default /)

The production image is sealed: its kernel and initramfs live only inside the
UKI under /boot/EFI/Linux, whose signed command line pins the composefs digest
of the rootfs. The dev image (os/Containerfile.dev) layers files on top of that
image, so the digest can no longer match, and its dev layers have to merge into
/usr with systemd-sysext. It is therefore installed unsealed: a plain
vmlinuz + initramfs.img in /usr/lib/modules/<kver>/, from which `bootc install`
writes ordinary boot entries (see docs/security/composefs-decision.md).

This restores that layout from what the sealed image carries: each of the two
files from the split-out kernel directory (/kernel/<kver>/, which `bootc
container split-kernel-and-rootfs` leaves in the image) when it is there,
otherwise from the UKI's own `.linux` or `.initrd` section. The production
image keeps only vmlinuz there; its initramfs is the UKI's `.initrd`, byte for
byte. It then removes every UKI, so nothing on the image claims a
seal it no longer has. A root that is already unsealed (the `rootfs` build
target) passes through unchanged. Every failure is fatal: a dev image that boots
the wrong kernel, or no kernel, must not build.
"""

from __future__ import annotations

import shutil
import struct
import sys
from pathlib import Path


def fail(message: str) -> None:
    raise SystemExit(f"unseal-rootfs: {message}")


def only_kernel(root: Path) -> str:
    modules = root / "usr/lib/modules"
    versions = sorted(p.name for p in modules.iterdir() if p.is_dir()) if modules.is_dir() else []
    if len(versions) != 1:
        fail(f"expected exactly one kernel under {modules}, found {versions or 'none'}")
    return versions[0]


def pe_sections(data: bytes):
    """Yield (name, file_offset, raw_size, virtual_size) for each PE section."""
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe:pe + 4] != b"PE\0\0":
        raise ValueError("not a PE image: no PE signature at e_lfanew")
    count = struct.unpack_from("<H", data, pe + 6)[0]
    base = pe + 24 + struct.unpack_from("<H", data, pe + 20)[0]
    for index in range(count):
        header = base + index * 40
        name = data[header:header + 8].rstrip(b"\0").decode("ascii", "replace")
        size, _address, raw_size, offset = struct.unpack_from("<IIII", data, header + 8)
        yield name, offset, raw_size, size


def uki_sections(uki: Path) -> dict[str, bytes]:
    """The UKI's sections by name."""
    data = uki.read_bytes()
    found: dict[str, bytes] = {}
    try:
        table = list(pe_sections(data))
    except (ValueError, struct.error) as error:
        fail(f"{uki}: {error}")
    for name, offset, raw_size, size in table:
        if name in found:
            fail(f"{uki} has two {name} sections")
        if not size or size > raw_size or offset + size > len(data):
            fail(f"{uki}: section {name} has invalid bounds")
        found[name] = data[offset:offset + size]
    return found


def restore_pair(root: Path, split: Path, ukis: list[Path]) -> tuple[bytes, bytes, str]:
    """The kernel and initramfs to restore, and where they came from: each from
    the split-out directory when it holds it, otherwise from the one UKI."""
    files = {name: split / name for name in ("vmlinuz", "initramfs.img")}
    if all(path.is_file() for path in files.values()):
        return files["vmlinuz"].read_bytes(), files["initramfs.img"].read_bytes(), f"/kernel/{split.name}"
    if len(ukis) != 1:
        fail(f"no kernel to restore: usr/lib/modules/{split.name} has no vmlinuz + initramfs.img, "
             f"{split} does not hold both, and there are {len(ukis)} UKIs under /boot/EFI/Linux "
             f"(need exactly one)")
    sections = uki_sections(ukis[0])
    restored, origins = [], []
    for name, section in (("vmlinuz", ".linux"), ("initramfs.img", ".initrd")):
        if files[name].is_file():
            restored.append(files[name].read_bytes())
            origins.append(f"/kernel/{split.name}/{name}")
        elif section in sections:
            restored.append(sections[section])
            origins.append(f"{section} of {ukis[0].relative_to(root)}")
        else:
            fail(f"{ukis[0]} has no {section} section to restore the kernel from")
    return restored[0], restored[1], " and ".join(origins)


def write(path: Path, data: bytes, mode: int) -> None:
    temporary = path.with_name(f".{path.name}.tmp")
    temporary.write_bytes(data)
    temporary.chmod(mode)
    temporary.replace(path)


def main(argv: list[str]) -> int:
    root = Path(argv[1] if len(argv) > 1 else "/")
    kver = only_kernel(root)
    kdir = root / "usr/lib/modules" / kver
    vmlinuz, initramfs = kdir / "vmlinuz", kdir / "initramfs.img"
    ukis = sorted((root / "boot/EFI/Linux").glob("*.efi"))
    split = root / "kernel" / kver

    if vmlinuz.is_file() and initramfs.is_file():
        source = "already unsealed"
    else:
        kernel, initrd, source = restore_pair(root, split, ukis)
        write(vmlinuz, kernel, 0o755)
        write(initramfs, initrd, 0o600)

    for uki in ukis:
        uki.unlink()
        # Addons (`<uki>.efi.extra.d/*.addon.efi`) only ever extend that UKI.
        shutil.rmtree(uki.with_name(uki.name + ".extra.d"), ignore_errors=True)
    shutil.rmtree(root / "boot/tilo-target", ignore_errors=True)
    shutil.rmtree(root / "kernel", ignore_errors=True)

    left = sorted(str(p.relative_to(root)) for p in (root / "boot").rglob("*.efi")) \
        if (root / "boot").is_dir() else []
    if left:
        fail(f"sealed boot images remain after unsealing: {', '.join(left)}")
    for path in (vmlinuz, initramfs):
        if not path.is_file() or path.stat().st_size == 0:
            fail(f"{path} is missing or empty after unsealing")
    print(f"unsealed: kernel {kver} from {source}; removed {len(ukis)} UKI(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
