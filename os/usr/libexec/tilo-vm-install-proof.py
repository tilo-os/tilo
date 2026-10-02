#!/usr/bin/env python3
"""Bind a completed VM install to its booted medium and installed deployment.

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
DEV_VM_MARKER = Path("/usr/share/tilo/unverified-vm-install")
VM_PROOF_PORT = Path("/dev/virtio-ports/org.tilo.vm-install-proof")


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


def copied_dev_manifest_digest():
    """Identify a VM-only bootc export by its exact local OCI manifest bytes."""
    require(DEV_VM_MARKER.is_file() and not DEV_VM_MARKER.is_symlink()
            and VM_PROOF_PORT.is_char_device(),
            "development copy proof requires the VM-only installer and proof port")
    command("systemd-detect-virt", "--vm", "--quiet")
    raw = subprocess.run(("skopeo", "inspect", "--raw",
                          "containers-storage:localhost/bootc"), check=True,
                         capture_output=True, timeout=30).stdout
    require(raw, "development copy has no local OCI manifest")
    return "sha256:" + hashlib.sha256(raw).hexdigest()


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
    installed_origin = parser.get("origin", "container-image-reference")

    booted = json.loads(command("bootc", "status", "--format", "json"))["status"]["booted"]
    booted_digest = booted["image"]["imageDigest"]
    architecture = booted["image"]["architecture"]
    require(architecture == "arm64", "VM installer did not boot an ARM64 image")
    require(DIGEST.fullmatch(booted_digest), "booted image digest is malformed")
    require(booted.get("incompatible") is False,
            "booted installer image is incompatible")
    booted_composefs = booted["composefs"]["verity"]
    require(COMPOSEFS.fullmatch(booted_composefs) and COMPOSEFS.fullmatch(composefs),
            "booted or installed composefs digest is malformed")
    receipt_path = getattr(args, "offline_target_receipt",
                           Path("/run/tilo-installer/offline-target.json"))
    selected = None
    if receipt_path.exists() or receipt_path.is_symlink():
        require(receipt_path.is_file() and not receipt_path.is_symlink()
                and receipt_path.stat().st_size <= 8192,
                "offline target verification receipt is unsafe")
        selected = json.loads(receipt_path.read_text(encoding="utf-8"))
        development = selected.get("schema") == "tilo.installer-dev-target.v1"
        require(selected.get("schema") in ("tilo.installer-offline-target.v1", "tilo.installer-dev-target.v1")
                and selected.get("target_digest") == installed_digest
                and selected.get("target_ref") == ("registry.tilo.so/tilo/tilo-vm" if development else {
                    "generic": "registry.tilo.so/tilo/tilo",
                    # Board images stay on the forge's private registry.
                    "lenovo-83ed": "code.tilo.so/tilo/tilo",
                }.get(selected.get("hardware_profile"), "")) + "@" + installed_digest
                and installed_origin == "ostree-unverified-image:docker://" + selected.get("update_origin", ""),
                "installed deployment differs from the independently signed offline target")
        if development:
            require(DEV_VM_MARKER.is_file() and not DEV_VM_MARKER.is_symlink()
                    and VM_PROOF_PORT.is_char_device() and selected.get("verifier_key_sha256") is None,
                    "development target proof requires the VM-only development installer")
            command("systemd-detect-virt", "--vm", "--quiet")
            require(selected.get("provenance", {}).get("source_revision") == selected.get("source_revision")
                    and selected.get("provenance", {}).get("invocation_id") == selected.get("ci_run_id"),
                    "development target source differs from its transport receipt")
            selected = {key: value for key, value in selected.items()
                        if key not in ("source_revision", "ci_run_id")}
    else:
        require(booted_composefs == composefs,
                "installed deployment does not match the booted CI image")
        if booted_digest != installed_digest:
            require(copied_dev_manifest_digest() == installed_digest,
                    "installed deployment differs from the VM-local bootc copy")
    require(not args.target_uki.is_symlink(),
            "installed target UKI is a symlink")
    uki = args.target_uki.resolve(strict=True)
    require(uki.is_file()
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
    dev_copy = not selected and booted_digest != installed_digest
    result = {"schema": "tilo.vm-install-image-proof.v4" if selected and development else
            "tilo.vm-install-image-proof.v2" if selected else
            "tilo.vm-install-image-proof.v3" if dev_copy else
            "tilo.vm-install-image-proof.v1",
            "architecture": architecture,
            "booted_image_digest": booted_digest,
            "installed_image_digest": installed_digest,
            "composefs_digest": composefs,
            "booted_composefs_digest": booted_composefs,
            "installed_composefs_digest": composefs,
            "installed_update_origin": installed_origin,
            "target_uki_sha256": hash_file(uki),
            "disk_guid": disk_guid.lower(), "partuuids": identities}
    if selected:
        result["selected_target"] = selected
    if dev_copy:
        result["copied_manifest_digest"] = installed_digest
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("target-root", "deploy-etc", "efi-root", "target-uki", "disk",
                 "efi-partition", "boot-partition", "root-partition", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--offline-target-receipt", type=Path,
                        default=Path("/run/tilo-installer/offline-target.json"))
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
