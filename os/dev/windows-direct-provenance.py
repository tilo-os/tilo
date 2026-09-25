#!/usr/bin/env python3
"""CI manifest and guest-proof checks shared by source receipt and final copy."""

import hashlib
import importlib.util
import base64
import binascii
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
BUILD_TYPE = "https://tilo.sh/buildtypes/bootc-os/v1"
BUILDER_ID = "https://code.podesta.ai/qos/forgejo/actions/bootc-release-v1"
SOURCE = re.compile(r"git\+ssh://code\.podesta\.ai/qos/qos@([0-9a-f]{40})\Z")


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
    require(type(image) is str and image.startswith("code.podesta.ai/qos/tilo@sha256:")
            and SHA.fullmatch(image.split("@", 1)[1]),
            "CI image must pin the official Tilo release index")
    pinned = image.split("@", 1)[1]
    raw, doc = raw_document(index_path)
    require("sha256:" + hashlib.sha256(raw).hexdigest() == pinned,
            "supplied CI top-level manifest differs from signed image pin")
    media = doc.get("mediaType")
    if media in (INDEX, DOCKER_INDEX):
        kind = "generic-index"
        descriptors = doc.get("manifests")
        require(type(descriptors) is list and len(descriptors) == 2,
                "CI index must contain exactly two release children")
        children = []
        for item in descriptors:
            require(type(item) is dict and item.get("mediaType") in (IMAGE, DOCKER_IMAGE)
                    and type(item.get("digest")) is str and SHA.fullmatch(item["digest"])
                    and type(item.get("platform")) is dict
                    and item["platform"].get("os") == "linux",
                    "CI release index has an invalid child descriptor")
            arch = item["platform"].get("architecture")
            variant = item["platform"].get("variant")
            require((arch == "arm64" and variant in (None, "v8"))
                    or (arch == "amd64" and variant in (None, "")),
                    "CI release index has an unsupported platform")
            children.append({"architecture": "aarch64" if arch == "arm64" else "x86_64",
                             "digest": item["digest"]})
        children.sort(key=lambda row: row["architecture"])
        require([row["architecture"] for row in children] == ["aarch64", "x86_64"]
                and children[0]["digest"] != children[1]["digest"],
                "CI release index has duplicate or missing platforms")
        child_digest = children[0]["digest"]
    else:
        require(media in (IMAGE, DOCKER_IMAGE),
                "CI image is neither a release index nor Lenovo ARM64 manifest")
        kind = "lenovo-83ed"
        children = []
        child_digest = pinned
    child_raw, child = raw_document(child_path)
    require("sha256:" + hashlib.sha256(child_raw).hexdigest() == child_digest
            and child.get("mediaType") in (IMAGE, DOCKER_IMAGE)
            and type(child.get("config")) is dict
            and type(child["config"].get("digest")) is str
            and SHA.fullmatch(child["config"]["digest"])
            and type(child.get("layers")) is list and child["layers"],
            "CI ARM64 child manifest does not match signed index descriptor")
    return pinned, child_digest, children, kind


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


def installed_loader(stream, esp_row):
    reader = fat.Fat32(stream, esp_row["offset_bytes"], esp_row["size_bytes"])
    paths = (("EFI/systemd/systemd-bootaa64.efi", "\\EFI\\systemd\\systemd-bootaa64.efi"),
             ("EFI/BOOT/BOOTAA64.EFI", "\\EFI\\BOOT\\BOOTAA64.EFI"))
    for source_path, efi_path in paths:
        try:
            return efi_path, reader.file_sha256(source_path)
        except fat.FatError as error:
            if str(error) != "FAT32 path is missing":
                raise
    raise ValueError("source ESP has no ARM64 EFI loader")


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


def wrappers(output):
    require(type(output) is str and 0 < len(output) <= 8 * 1024 * 1024,
            "Cosign verification output is empty or oversized")
    try:
        parsed = [json.loads(output)]
    except json.JSONDecodeError:
        parsed = [json.loads(line) for line in output.splitlines() if line.strip()]
    rows = [item for value in parsed for item in (value if type(value) is list else [value])]
    require(rows and all(type(row) is dict for row in rows),
            "Cosign verification output has an invalid wrapper")
    return rows


def checked_slsa_attestation(output, pinned, children, kind):
    matches = []
    for wrapper in wrappers(output):
        payload = wrapper.get("payload")
        if type(payload) is not str:
            continue
        try:
            statement_raw = base64.b64decode(payload, validate=True)
            statement = json.loads(statement_raw)
        except (ValueError, UnicodeDecodeError, binascii.Error):
            continue
        if type(statement) is not dict:
            continue
        subjects = statement.get("subject")
        predicate = statement.get("predicate")
        if (statement.get("predicateType") != "https://slsa.dev/provenance/v1"
                or type(subjects) is not list or type(predicate) is not dict
                or not any(type(subject) is dict and type(subject.get("digest")) is dict
                           and subject["digest"].get("sha256") == pinned.removeprefix("sha256:")
                           for subject in subjects)):
            continue
        definition = predicate.get("buildDefinition")
        details = predicate.get("runDetails")
        if type(definition) is not dict or type(details) is not dict:
            continue
        external = definition.get("externalParameters")
        internal = definition.get("internalParameters")
        dependencies = definition.get("resolvedDependencies")
        if (definition.get("buildType") != BUILD_TYPE
                or type(external) is not dict or type(internal) is not dict
                or type(dependencies) is not list or type(details.get("builder")) is not dict
                or details["builder"].get("id") != BUILDER_ID
                or internal.get("expectedReleaseDigest") != pinned):
            continue
        if kind == "generic-index":
            attested_children = external.get("children")
            if (type(attested_children) is not list or len(attested_children) != 2
                    or not all(type(row) is dict and set(row) == {"architecture", "digest"}
                               for row in attested_children)
                    or sorted(attested_children, key=lambda row: str(row["architecture"])) != children
                    or type(external.get("generation")) is not int
                    or external["generation"] < 1
                    or "hardwareProfile" in external):
                continue
            firmware_candidate = None
        elif kind == "lenovo-83ed":
            candidate = dependencies[-1].get("uri") if dependencies and type(dependencies[-1]) is dict else None
            if (external.get("generation") != 1
                    or external.get("hardwareProfile") != "lenovo-83ed"
                    or external.get("architecture") != "aarch64"
                    or type(external.get("firmwareManifestSha256")) is not str
                    or HEX.fullmatch(external["firmwareManifestSha256"]) is None
                    or "children" in external
                    or type(candidate) is not str
                    or re.fullmatch(r"code\.podesta\.ai/qos/[a-z0-9][a-z0-9._-]*@sha256:[0-9a-f]{64}",
                                    candidate) is None):
                continue
            firmware_candidate = candidate
        else:
            raise ValueError("unsupported CI image provenance kind")
        sources = [match.group(1) for dependency in dependencies
                   if type(dependency) is dict
                   and type(dependency.get("uri")) is str
                   if (match := SOURCE.fullmatch(dependency["uri"]))]
        metadata = details.get("metadata")
        invocation = metadata.get("invocationId") if type(metadata) is dict else None
        byproducts = details.get("byproducts")
        if (len(sources) != 1 or type(invocation) is not str
                or re.fullmatch(r"[0-9]+", invocation) is None
                or type(byproducts) is not list
                or not any(type(item) is dict
                           and item.get("name") == "independent-rebuild"
                           and item.get("uri") == "urn:tilo:verification:manifest-digest-equal"
                           for item in byproducts)):
            continue
        matches.append({"source_revision": sources[0],
                        "generation": external["generation"],
                        "image_kind": kind,
                        "firmware_candidate": firmware_candidate,
                        "firmware_manifest_sha256": external.get("firmwareManifestSha256"),
                        "statement_sha256": hashlib.sha256(statement_raw).hexdigest()})
    require(len(matches) == 1,
            "CI SLSA attestation does not uniquely bind the signed index, children, builder, source, and generation")
    return matches[0]


def verify_ci_image(image, key, children, kind, command_json=None, key_arg=None):
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
    outputs = []
    for command in (["verify", "--key", key_value, image],
                    ["verify-attestation", "--key", key_value,
                     "--type", "slsaprovenance1", image]):
        result = subprocess.run([*prefix, *command], capture_output=True,
                                text=True, timeout=180)
        require(result.returncode == 0,
                "CI image signature/provenance verification failed: "
                + result.stderr.strip())
        outputs.append(result.stdout)
    pinned = image.split("@", 1)[1]
    require(any(type(row.get("critical")) is dict
                and type(row["critical"].get("image")) is dict
                and row["critical"]["image"].get("docker-manifest-digest") == pinned
                for row in wrappers(outputs[0])),
            "CI signature does not bind the pinned image digest")
    statement = checked_slsa_attestation(outputs[1], pinned, children, kind)
    return {"key_sha256": hashlib.sha256(key.read_bytes()).hexdigest(),
            **statement}
