#!/usr/bin/env python3
"""Read-only installed-guest proof for the disposable alongside GUI cycle.

Expected identities come from independently admitted finalization output. This
supplements host sentinel checks; it neither mounts disks nor installs anything.
"""
import argparse
import configparser
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess


def require(value, message):
    if not value:
        raise ValueError(message)


def command(argv, timeout=300):
    return subprocess.check_output(argv, timeout=timeout).decode("utf-8", "strict")


def validate_expected(e):
    for key, pattern in {"composefs_digest": r"[0-9a-f]{128}",
                         "target_uki_sha256": r"[0-9a-f]{64}",
                         "source_manifest_digest": r"sha256:[0-9a-f]{64}",
                         "source_config_digest": r"sha256:[0-9a-f]{64}"}.items():
        require(re.fullmatch(pattern, e[key]) is not None, "invalid " + key)
    require(type(e["generation"]) is int and e["generation"] >= 0, "invalid generation")
    require(e["mode"] in ("prepared-alongside", "guided-alongside"), "invalid mode")
    require(isinstance(e["installed_target"], str) and bool(e["installed_target"]), "target missing")


def check_runtime(e, cmdline, generation, mount, table):
    validate_expected(e)
    tokens = cmdline.split()
    for key, value in (("composefs", e["composefs_digest"]), ("tilo.generation", str(e["generation"]))):
        require([x for x in tokens if x.split("=", 1)[0] == key] == [key + "=" + value], "wrong/duplicate " + key)
    require(not any(x.split("=", 1)[0] == "tilo.install" for x in tokens), "installer flag remains")
    require(generation.strip() == str(e["generation"]), "running generation differs")
    require(mount["target"] == "/" and mount["fstype"] == "overlay", "unexpected root mount")
    require(mount["source"] == "composefs:" + e["composefs_digest"], "mounted root seal differs")
    options = mount["options"].split(",")
    require("ro" in options and "verity=require" in options, "running root lacks required read-only verity")
    disks = [x for x in table["blockdevices"] if x["type"] == "disk"]
    require(len(disks) == 1 and disks[0].get("serial") == "tilo-install-target", "medium present or wrong target")
    parts = [x for x in disks[0].get("children", []) if x["type"] == "part"]
    prepared = e["mode"] == "prepared-alongside"
    require(len(parts) == (5 if prepared else 6), "wrong installed partition count")
    expected = [(1, 2048 if prepared else 512), (2049 if prepared else 513, 512), (2561 if prepared else 1025, 512)]
    for index, (start_mib, size_mib) in enumerate(expected):
        p = parts[index]
        require(int(p["start"]) * 512 == start_mib * 2**20 and int(p["size"]) == size_mib * 2**20, "foreign partition geometry differs")
        require(p["partuuid"].lower() == str(index + 1) + "0000000-0000-4000-8000-00000000000" + str(index + 1), "foreign partition UUID differs")
    root = parts[-1]
    boot = parts[-2]
    require(int(boot["size"]) == 2**30 and int(boot["start"]) * 512 ==
            (3073 if prepared else 3585) * 2**20, "boot fixture geometry differs")
    require(boot.get("fstype") == "ext4" and boot.get("label") == "TILO_BOOT",
            "installed boot filesystem differs")
    require(int(root["size"]) == (75492848 * 512 if prepared else 36350 * 2**20), "root fixture size differs")
    require(int(root["start"]) * 512 == (4097 if prepared else 4609) * 2**20, "root fixture offset differs")
    require(root.get("fstype") == "crypto_LUKS", "installed root is not encrypted")
    require(any(x["type"] == "crypt" and x.get("fstype") == "btrfs" for x in root.get("children", [])), "encrypted root not opened as Btrfs")
    if not prepared:
        esp = parts[3]
        require(int(esp["start"]) * 512 == 1537 * 2**20 and int(esp["size"]) == 2**31, "dedicated ESP geometry differs")
    return disks[0]



def selected_entry(esp, raw, digest):
    require(4 < len(raw) <= 4096 and (len(raw) - 4) % 2 == 0, "invalid loader entry EFI variable")
    name = raw[4:].decode("utf-16-le", "strict").rstrip("\0")
    require(re.fullmatch(r"[A-Za-z0-9_.+-]+\.conf", name) is not None, "unsupported selected loader entry")
    lines = {}
    for line in (esp / "loader/entries" / name).read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"): continue
        fields = line.split(None, 1)
        require(len(fields) == 2 and fields[0] not in lines, "ambiguous selected entry")
        lines[fields[0]] = fields[1]
    require(not set(lines) - {"title", "version", "sort-key", "efi"}, "selected entry overrides signed UKI")
    require(lines.get("efi") == "/EFI/Linux/bootc/bootc_composefs-" + digest + ".efi", "bootloader selected a different UKI")
    return {"entry": name, "efi": lines["efi"], "source": "LoaderEntrySelected EFI variable"}


def check_bootc_status(doc, e):
    # Retained bootc 1.16.4 composefs status leaves status.type unset.
    booted = doc["status"].get("booted")
    require(isinstance(booted, dict), "no booted deployment")
    require(booted.get("incompatible") is False and booted.get("ostree") is None, "wrong backend")
    cfs = booted.get("composefs") or {}
    require(cfs.get("verity") == e["composefs_digest"] and cfs.get("missingVerityAllowed") is False, "bootc verity differs")
    require(cfs.get("bootType") == "Uki" and cfs.get("bootloader") == "systemd", "bootc boot method differs")
    image = booted.get("image") or {}
    require(image.get("imageDigest") == e["source_manifest_digest"] and image.get("architecture") == "arm64", "bootc booted image differs")
    reference = image.get("image") or {}
    require(reference.get("image") == e["installed_target"] and reference.get("transport") == "registry", "bootc update reference differs")


def measure_verity(path):
    import fcntl
    # Linux UAPI FS_IOC_MEASURE_VERITY; struct fsverity_digest is two u16s.
    data = bytearray(struct.pack("=HH", 0, 64) + bytes(64))
    with path.open("rb") as f:
        fcntl.ioctl(f.fileno(), 0xC0046686, data, True)
    algorithm, size = struct.unpack_from("=HH", data)
    require(algorithm == 2 and size == 64, "composefs image is not SHA512 fs-verity")
    return bytes(data[4:68]).hex()


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--expected", type=Path, required=True)
    ap.add_argument("--sysroot", type=Path, required=True, help="actual mounted backing filesystem, usually /sysroot")
    ap.add_argument("--esp", type=Path, required=True, help="actual mounted target ESP, usually /boot")
    a = ap.parse_args()
    require(os.geteuid() == 0, "run in installed disposable guest as root")
    e = json.loads(a.expected.read_text())
    validate_expected(e)
    mounts = json.loads(command(["findmnt", "--json", "--mountpoint", "/", "-o", "TARGET,SOURCE,FSTYPE,OPTIONS"]))
    require(len(mounts["filesystems"]) == 1, "ambiguous root mount")
    mount = mounts["filesystems"][0]
    table = json.loads(command(["lsblk", "--json", "--bytes", "-o", "NAME,PATH,TYPE,SIZE,FSTYPE,PARTUUID,START,SERIAL,MOUNTPOINTS"]))
    cmdline = Path("/proc/cmdline").read_text()
    disk = check_runtime(e, cmdline, Path("/usr/lib/tilo/generation").read_text(), mount, table)
    esp_mount = json.loads(command(["findmnt", "--json", "--mountpoint", str(a.esp), "-o", "TARGET,SOURCE,FSTYPE,OPTIONS"]))["filesystems"]
    parts = [x for x in disk["children"] if x["type"] == "part"]
    esp_part = parts[0 if e["mode"] == "prepared-alongside" else 3]
    require(len(esp_mount) == 1 and esp_mount[0]["source"] == esp_part["path"] and esp_mount[0]["fstype"] == "vfat", "ESP is not on the installed target")
    selected = selected_entry(a.esp, Path("/sys/firmware/efi/efivars/LoaderEntrySelected-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f").read_bytes(), e["composefs_digest"])
    uki = a.esp / ("EFI/Linux/bootc/bootc_composefs-" + e["composefs_digest"] + ".efi")
    with uki.open("rb") as f:
        uki_sha = hashlib.file_digest(f, "sha256").hexdigest()
    require(uki_sha == e["target_uki_sha256"], "target UKI differs from authenticated signed UKI")
    backing = json.loads(command(["findmnt", "--json", "--mountpoint", str(a.sysroot), "-o", "TARGET,SOURCE,FSTYPE,OPTIONS"]))["filesystems"]
    encrypted_paths = {x["path"] for x in parts[-1].get("children", []) if x["type"] == "crypt"}
    require(len(backing) == 1 and backing[0]["source"].split("[", 1)[0] in encrypted_paths and backing[0]["fstype"] == "btrfs", "sysroot does not belong to the installed encrypted target")
    repo = a.sysroot / "composefs"
    seal = measure_verity(repo / "images" / e["composefs_digest"])
    require(seal == e["composefs_digest"], "kernel-measured repository image seal differs")
    origin = configparser.ConfigParser(interpolation=None, strict=True)
    origin.read(a.sysroot / "state/deploy" / e["composefs_digest"] / (e["composefs_digest"] + ".origin"))
    require(origin.get("origin", "container-image-reference") == "ostree-unverified-image:docker://" + e["installed_target"], "installed update origin differs")
    require(origin.get("image", "manifest_digest") == e["source_manifest_digest"], "installed manifest differs")
    cfs = ["bootc", "internals", "cfs", "--", "--repo", str(repo), "--hash", "sha512", "--require-verity", "--no-upgrade"]
    command(cfs + ["oci", "fsck", "@" + e["source_manifest_digest"]], timeout=1800)
    manifest = json.loads(command(cfs + ["oci", "inspect", "--manifest", "@" + e["source_manifest_digest"]]))
    require(manifest["config"]["digest"] == e["source_config_digest"], "installed OCI config differs")
    status = json.loads(command(["bootc", "status", "--json"]))
    check_bootc_status(status, e)
    result = {"purpose": "tilo-installed-guest-runtime-verification", "checks_passed": True,
              "expected": e, "cmdline": cmdline.strip(), "root_mount": mount,
              "blockdevices": table, "esp_mount": esp_mount[0], "target_uki_sha256": uki_sha,
              "kernel_measured_composefs_digest": seal, "bootc_status": status,
              "bootloader_selected_entry": selected, "installed_oci_fsck_passed": True,
              "secure_boot_or_tpm_attestation": False,
              "boot_id": Path("/proc/sys/kernel/random/boot_id").read_text().strip(),
              "physical_installation": False,
              "scope": "runtime identity, encrypted target and verity root; combine with host sentinel and GUI evidence"}
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
