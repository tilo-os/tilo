#!/usr/bin/env python3
"""Bind a completed VM install to the booted image and installed deployment.

The caller writes the result to /run before closing the encrypted target and
emits it only after the install backend finishes successfully. The record is
guest evidence, not a signature or a substitute for host readback.
"""

import argparse
import configparser
import hashlib
import json
from pathlib import Path
import re
import subprocess


DIGEST = re.compile(r"sha256:[0-9a-f]{64}\Z")
COMPOSEFS = re.compile(r"[0-9a-f]{128}\Z")
GUID = re.compile(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\Z")


def require(ok, message):
    if not ok:
        raise ValueError(message)


def command(*args):
    return subprocess.run(args, check=True, capture_output=True, text=True,
                          timeout=30).stdout.strip()


def composefs_from_uki(uki):
    args = command("/usr/libexec/tilo-uki-cmdline", "show", str(uki)).split()
    values = [arg.removeprefix("composefs=") for arg in args
              if arg.startswith("composefs=")]
    require(len(values) == 1 and COMPOSEFS.fullmatch(values[0]),
            "installed target UKI has no unique composefs digest")
    return values[0]


def hash_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def build_proof(args):
    root = args.target_root.resolve(strict=True)
    deploy_etc = args.deploy_etc.resolve(strict=True)
    relative = deploy_etc.relative_to(root)
    require(len(relative.parts) == 4 and relative.parts[:2] == ("state", "deploy")
            and relative.parts[3] == "etc" and COMPOSEFS.fullmatch(relative.parts[2]),
            "installed deployment has an unexpected /etc path")
    composefs = relative.parts[2]
    origin = deploy_etc.parent / (composefs + ".origin")
    require(origin.is_file() and not origin.is_symlink(),
            "installed deployment origin is missing or unsafe")
    parser = configparser.ConfigParser(interpolation=None, strict=True)
    with origin.open(encoding="utf-8") as source:
        parser.read_file(source)
    installed_digest = parser.get("image", "manifest_digest")
    require(DIGEST.fullmatch(installed_digest),
            "installed deployment has no OCI manifest digest")

    booted = json.loads(command("bootc", "status", "--format", "json"))["status"]["booted"]
    booted_digest = booted["image"]["imageDigest"]
    architecture = booted["image"]["architecture"]
    require(architecture == "arm64", "VM installer did not boot an ARM64 image")
    require(DIGEST.fullmatch(booted_digest) and booted_digest == installed_digest,
            "installed deployment does not match the booted CI image digest")
    require(booted.get("incompatible") is False,
            "booted installer image is incompatible")
    booted_composefs = booted["composefs"]["verity"]
    require(booted_composefs == composefs and COMPOSEFS.fullmatch(composefs),
            "installed composefs digest differs from booted image")
    uki = args.target_uki.resolve(strict=True)
    require(uki.is_file() and not uki.is_symlink()
            and uki.is_relative_to(args.efi_root.resolve(strict=True)),
            "installed target UKI is outside the selected ESP")
    require(composefs_from_uki(uki) == composefs,
            "installed target UKI differs from deployed composefs digest")

    disk_guid = command("blkid", "-p", "-s", "PTUUID", "-o", "value", str(args.disk))
    require(GUID.fullmatch(disk_guid), "installed disk has no GPT identity")
    identities = {}
    for role in ("efi", "boot", "root"):
        partuuid = command("blkid", "-s", "PARTUUID", "-o", "value",
                           str(getattr(args, role + "_partition")))
        require(GUID.fullmatch(partuuid), f"installed {role} has no GPT identity")
        identities[role] = partuuid.lower()
    require(len(set(identities.values())) == 3,
            "installed target partition identities overlap")
    return {"schema": "tilo.vm-install-image-proof.v1",
            "architecture": architecture,
            "booted_image_digest": booted_digest,
            "installed_image_digest": installed_digest,
            "composefs_digest": composefs,
            "target_uki_sha256": hash_file(uki),
            "disk_guid": disk_guid.lower(), "partuuids": identities}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("target-root", "deploy-etc", "efi-root", "target-uki", "disk",
                 "efi-partition", "boot-partition", "root-partition", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    result = build_proof(args)
    destination = args.output
    require(destination.parent.is_dir() and not destination.is_symlink(),
            "proof output directory is missing or unsafe")
    temporary = destination.with_suffix(".tmp")
    temporary.write_text(json.dumps(result, sort_keys=True, separators=(",", ":")) + "\n",
                         encoding="utf-8")
    temporary.chmod(0o600)
    temporary.replace(destination)


if __name__ == "__main__":
    main()
