#!/usr/bin/env python3
"""CI manifest and guest-proof checks shared by source receipt and final copy."""

import hashlib
import importlib.util
import json
from pathlib import Path
import platform
import re
import subprocess


SHA = re.compile(r"sha256:[0-9a-f]{64}\Z")
HEX = re.compile(r"[0-9a-f]{64}\Z")
COMPOSEFS = re.compile(r"[0-9a-f]{128}\Z")
IMAGE = "application/vnd.oci.image.manifest.v1+json"
DOCKER_IMAGE = "application/vnd.docker.distribution.manifest.v2+json"
INDEX = "application/vnd.oci.image.index.v1+json"
DOCKER_INDEX = "application/vnd.docker.distribution.manifest.list.v2+json"
PROOF_SCHEMA = "tilo.windows-vm-install-proof-check.v1"


def require(ok, message):
    if not ok:
        raise ValueError(message)


def raw_document(path):
    require(path.is_file() and not path.is_symlink() and path.stat().st_size <= 8 * 1024 * 1024,
            "CI manifest must be a bounded regular file")
    raw = path.read_bytes()
    try:
        doc = json.loads(raw)
    except (ValueError, UnicodeDecodeError) as error:
        raise ValueError("CI manifest is not JSON") from error
    require(type(doc) is dict and doc.get("schemaVersion") == 2,
            "CI manifest has an unsupported schema")
    return raw, doc


def signed_ci_child(image, index_path, child_path):
    require("@sha256:" in image and SHA.fullmatch(image.split("@", 1)[1]),
            "CI image must be digest-pinned")
    pinned = image.split("@", 1)[1]
    raw, doc = raw_document(index_path)
    require("sha256:" + hashlib.sha256(raw).hexdigest() == pinned,
            "supplied CI top-level manifest differs from signed image pin")
    media = doc.get("mediaType")
    if media in (INDEX, DOCKER_INDEX):
        descriptors = doc.get("manifests")
        require(type(descriptors) is list and len(descriptors) <= 256,
                "CI index descriptor list is invalid")
        matches = [item for item in descriptors if type(item) is dict
                   and type(item.get("platform")) is dict
                   and item["platform"].get("os") == "linux"
                   and item["platform"].get("architecture") == "arm64"
                   and item["platform"].get("variant") in (None, "v8")]
        require(len(matches) == 1 and matches[0].get("mediaType") in (IMAGE, DOCKER_IMAGE)
                and type(matches[0].get("digest")) is str
                and SHA.fullmatch(matches[0]["digest"]),
                "CI index must name exactly one Linux ARM64 image manifest")
        child_digest = matches[0]["digest"]
    else:
        require(media in (IMAGE, DOCKER_IMAGE), "CI pin is not an OCI image or index")
        child_digest = pinned
    child_raw, child = raw_document(child_path)
    require("sha256:" + hashlib.sha256(child_raw).hexdigest() == child_digest
            and child.get("mediaType") in (IMAGE, DOCKER_IMAGE)
            and type(child.get("config")) is dict
            and type(child["config"].get("digest")) is str
            and SHA.fullmatch(child["config"]["digest"])
            and type(child.get("layers")) is list and child["layers"],
            "CI ARM64 child manifest does not match signed index descriptor")
    return pinned, child_digest


def checked_guest_proof(path, child_digest):
    require(path.is_file() and not path.is_symlink() and path.stat().st_size <= 64 * 1024,
            "checked VM proof must be a bounded regular file")
    doc = json.loads(path.read_text(encoding="utf-8"))
    require(type(doc) is dict and doc.get("schema") == PROOF_SCHEMA,
            "checked VM proof schema differs")
    guest = doc.get("guest_proof")
    source = doc.get("source_check")
    require(type(guest) is dict and type(source) is dict,
            "checked VM proof is incomplete")
    require(guest.get("schema") == "tilo.vm-install-image-proof.v1"
            and guest.get("architecture") == "arm64"
            and guest.get("booted_image_digest") == child_digest
            and guest.get("installed_image_digest") == child_digest,
            "VM-installed image differs from signed CI ARM64 child")
    composefs = guest.get("composefs_digest")
    require(type(composefs) is str and COMPOSEFS.fullmatch(composefs)
            and doc.get("installer_composefs") == composefs
            and type(guest.get("target_uki_sha256")) is str
            and HEX.fullmatch(guest["target_uki_sha256"]),
            "VM composefs or UKI binding differs")
    require(source.get("schema") == "tilo.windows-direct-vm-template-check.v1"
            and source.get("preserved_gpt_verified") is True
            and source.get("preserved_edge_samples_verified") is True
            and source.get("final_partition_count") == 3
            and source.get("disk_guid") == guest.get("disk_guid")
            and source.get("final_partuuids") == guest.get("partuuids"),
            "VM proof does not match the verified source GPT")
    return doc


_fat_path = Path(__file__).with_name("read-fat32-efi.py")
_spec = importlib.util.spec_from_file_location("tilo_fat32_efi", _fat_path)
fat = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(fat)


def installed_uki_sha256(stream, esp_row, composefs):
    require(COMPOSEFS.fullmatch(composefs), "invalid installed composefs digest")
    reader = fat.Fat32(stream, esp_row["offset_bytes"], esp_row["size_bytes"])
    return reader.file_sha256("EFI/Linux/bootc/bootc_composefs-" + composefs + ".efi")


def cosign_prefix(command_json):
    if command_json is None:
        return ["cosign"]
    require(command_json.is_file() and not command_json.is_symlink()
            and command_json.stat().st_size <= 4096,
            "Cosign command description must be a bounded regular file")
    argv = json.loads(command_json.read_text(encoding="utf-8"))
    require(type(argv) is list and 1 <= len(argv) <= 16
            and all(type(arg) is str and 0 < len(arg) <= 512 for arg in argv),
            "Cosign command description has invalid arguments")
    return argv


def verify_ci_image(image, key, command_json=None, key_arg=None):
    require(type(image) is str and "@" in image
            and SHA.fullmatch(image.split("@", 1)[1]),
            "CI image must be digest-pinned")
    require(key.is_file() and not key.is_symlink(),
            "CI verification key is not a regular file")
    key_value = key_arg if key_arg is not None else str(key)
    require(type(key_value) is str and 0 < len(key_value) <= 4096,
            "Cosign key argument is invalid")
    if key_arg is not None:
        resolved = key.resolve(strict=True)
        require(platform.system() == "Windows" and resolved.drive
                and len(resolved.drive) == 2 and resolved.drive[1] == ":",
                "WSL Cosign key path requires a native Windows file")
        expected = "/mnt/" + resolved.drive[0].lower() + "/" + "/".join(
            resolved.parts[1:])
        require(key_arg == expected,
                "WSL Cosign key path differs from the native verified key")
    prefix = cosign_prefix(command_json)
    for command in (["verify", "--key", key_value, image],
                    ["verify-attestation", "--key", key_value,
                     "--type", "slsaprovenance1", image]):
        result = subprocess.run([*prefix, *command], capture_output=True,
                                text=True, timeout=180)
        require(result.returncode == 0,
                "CI image signature/provenance verification failed: "
                + result.stderr.strip())
    return hashlib.sha256(key.read_bytes()).hexdigest()
