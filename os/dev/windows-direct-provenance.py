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
PROOF_SCHEMA_V2 = "tilo.windows-vm-install-proof-check.v2"
BUILD_TYPE = "https://tilo.sh/buildtypes/bootc-os/v1"
BUILDER_ID = "https://code.tilo.so/tilo/t/os-release-v1"
SOURCE = re.compile(r"git\+ssh://code\.tilo\.so/tilo/tilo@([0-9a-f]{40})\Z")
PUBLIC_REPOSITORY = "registry.tilo.so/tilo/tilo"
# Board images carry private firmware and stay on the forge's private registry.
BOARD_REPOSITORY = "code.tilo.so/tilo/tilo"


def repository(board):
    return BOARD_REPOSITORY if board else PUBLIC_REPOSITORY


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
    require(type(image) is str
            and image.split("@", 1)[0] in (PUBLIC_REPOSITORY, BOARD_REPOSITORY)
            and image.split("@", 1)[1].startswith("sha256:")
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
    require(image.split("@", 1)[0] == repository(kind == "lenovo-83ed"),
            "CI image is not in its official repository")
    return pinned, child_digest, children, kind


def checked_guest_proof(path, child_digest, ci_image=None, ci_provenance=None):
    require(path.is_file() and not path.is_symlink() and path.stat().st_size <= 64 * 1024,
            "checked VM proof must be a bounded regular file")
    doc = json.loads(path.read_text(encoding="utf-8"))
    require(type(doc) is dict and doc.get("schema") in (PROOF_SCHEMA, PROOF_SCHEMA_V2),
            "checked VM proof schema differs")
    guest = doc.get("guest_proof")
    source = doc.get("source_check")
    require(type(guest) is dict and type(source) is dict,
            "checked VM proof is incomplete")
    require(guest.get("architecture") == "arm64"
            and guest.get("installed_image_digest") == child_digest,
            "VM-installed image differs from signed CI ARM64 child")
    if doc["schema"] == PROOF_SCHEMA:
        require(guest.get("schema") == "tilo.vm-install-image-proof.v1"
                and guest.get("booted_image_digest") == child_digest,
                "VM installer image differs from signed CI ARM64 child")
        booted_composefs = guest.get("composefs_digest")
        installed_composefs = booted_composefs
    else:
        require(type(ci_provenance) is dict
                and ci_provenance.get("image_kind") in ("lenovo-83ed", "generic-child"),
                "VM selected target has no independently verified CI provenance")
        board = ci_provenance["image_kind"] == "lenovo-83ed"
        require(guest.get("schema") == "tilo.vm-install-image-proof.v2"
                and doc.get("first_boot_verified") is (not board)
                and type(doc.get("installer_medium_sha256")) is str
                and HEX.fullmatch(doc["installer_medium_sha256"])
                and type(guest.get("booted_image_digest")) is str
                and SHA.fullmatch(guest["booted_image_digest"])
                and guest.get("composefs_digest") == guest.get("installed_composefs_digest"),
                "VM target proof has inconsistent booted and installed images")
        booted_composefs = guest.get("booted_composefs_digest")
        installed_composefs = guest.get("installed_composefs_digest")
        selected = guest.get("selected_target")
        require(type(selected) is dict
                and selected.get("schema") == "tilo.installer-offline-target.v1"
                and selected.get("target_digest") == child_digest
                and selected.get("target_ref") == repository(board) + "@" + child_digest
                and selected.get("architecture") == "aarch64"
                and selected.get("hardware_profile") == ("lenovo-83ed" if board else "generic"),
                "VM selected target differs from signed CI image")
        revision = ci_provenance.get("source_revision")
        require(type(revision) is str and re.fullmatch(r"[0-9a-f]{40}", revision),
                "independently verified CI source revision is invalid")
        if board:
            origin = "code.tilo.so/tilo/tilo:lenovo-83ed-" + revision
        else:
            generation = ci_provenance.get("generation")
            require(type(generation) is int and generation > 0,
                    "generic CI generation is invalid")
            origin = "registry.tilo.so/tilo/tilo:gen" + str(generation)
        signed = selected.get("provenance")
        require(type(signed) is dict
                and selected.get("update_origin") == origin
                and guest.get("installed_update_origin")
                == "ostree-unverified-image:docker://" + origin
                and selected.get("verifier_key_sha256") == ci_provenance.get("key_sha256")
                and signed.get("source_revision") == revision
                and signed.get("firmware_manifest_sha256")
                == ci_provenance.get("firmware_manifest_sha256")
                and signed.get("firmware_candidate")
                == ci_provenance.get("firmware_candidate")
                and (signed.get("generation") in (None, 1) if board else
                     signed.get("generation") == ci_provenance.get("generation"))
                and signed.get("invocation_id") == ci_provenance.get("invocation_id"),
                "VM selected target provenance differs from independently verified CI")
    composefs = guest.get("composefs_digest")
    require(type(composefs) is str and COMPOSEFS.fullmatch(composefs)
            and type(booted_composefs) is str and COMPOSEFS.fullmatch(booted_composefs)
            and type(installed_composefs) is str and COMPOSEFS.fullmatch(installed_composefs)
            and doc.get("installer_composefs") == booted_composefs
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


def cosign_file_arg(path, key_arg):
    resolved = path.resolve(strict=True)
    if key_arg is None:
        return str(resolved)
    require(platform.system() == "Windows" and resolved.drive
            and len(resolved.drive) == 2 and resolved.drive[1] == ":",
            "WSL Cosign file path requires a native Windows file")
    return "/mnt/" + resolved.drive[0].lower() + "/" + "/".join(resolved.parts[1:])


def bounded_file(path, maximum):
    require(path.is_file() and not path.is_symlink() and path.stat().st_size <= maximum,
            "signed CI medium input must be a bounded regular file")
    return path.read_bytes()


def signed_medium_identity(identity_path, ci_guest_proof_path, key,
                           command_json=None, key_arg=None):
    raw = bounded_file(identity_path, 64 * 1024)
    signature = Path(str(identity_path) + ".sig")
    bundle = Path(str(identity_path) + ".bundle")
    bounded_file(signature, 64 * 1024)
    bounded_file(bundle, 2 * 1024 * 1024)
    ci_proof_raw = bounded_file(ci_guest_proof_path, 64 * 1024)
    require(key.is_file() and not key.is_symlink(),
            "CI verification key is not a regular file")
    key_value = key_arg if key_arg is not None else str(key.resolve(strict=True))
    if key_arg is not None:
        require(cosign_file_arg(key, key_arg) == key_arg,
                "WSL Cosign key path differs from the native verified key")
    argv = [*cosign_prefix(command_json), "verify-blob", "--offline", "--key", key_value,
            "--bundle", cosign_file_arg(bundle, key_arg),
            "--signature", cosign_file_arg(signature, key_arg),
            cosign_file_arg(identity_path, key_arg)]
    result = subprocess.run(argv, capture_output=True, text=True, timeout=180)
    require(result.returncode == 0,
            "CI installer medium identity signature verification failed: " + result.stderr.strip())
    identity = json.loads(raw)
    fields = {
        "schema", "target_profile", "target_image", "live_base_image",
        "build_revision", "run_id", "run_attempt", "qcow2_sha256",
        "archive_sha256", "guest_proof_sha256"}
    require(type(identity) is dict and (
        (identity.get("schema") == "tilo.whpx-ci-installer-medium.v1"
         and set(identity) == fields)
        or (identity.get("schema") == "tilo.whpx-ci-installer-medium.v2"
            and set(identity) == fields | {"os_source_revision"}
            and type(identity["os_source_revision"]) is str
            and re.fullmatch(r"[0-9a-f]{40}", identity["os_source_revision"]))),
        "CI installer medium identity has unexpected fields")
    require(identity["target_profile"] in ("generic", "lenovo-83ed")
            and type(identity["target_image"]) is str
            and identity["target_image"].split("@", 1)[0]
            == repository(identity["target_profile"] == "lenovo-83ed")
            and re.fullmatch(r"[^@]+@sha256:[0-9a-f]{64}", identity["target_image"])
            and type(identity["live_base_image"]) is str
            and re.fullmatch(r"registry\.tilo\.so/tilo/tilo@sha256:[0-9a-f]{64}",
                             identity["live_base_image"])
            and type(identity["build_revision"]) is str
            and re.fullmatch(r"[0-9a-f]{40}", identity["build_revision"])
            and type(identity["run_id"]) is str
            and re.fullmatch(r"[0-9]+", identity["run_id"])
            and type(identity["run_attempt"]) is str
            and re.fullmatch(r"[0-9]+", identity["run_attempt"])
            and all(type(identity[key]) is str and HEX.fullmatch(identity[key])
                    for key in ("qcow2_sha256", "archive_sha256", "guest_proof_sha256"))
            and hashlib.sha256(ci_proof_raw).hexdigest() == identity["guest_proof_sha256"],
            "CI installer medium identity or native guest proof differs")
    ci_proof = json.loads(ci_proof_raw)
    require(type(ci_proof) is dict
            and ci_proof.get("schema") == "tilo.vm-install-image-proof.v2"
            and ci_proof.get("architecture") == "arm64"
            and ci_proof.get("installed_image_digest") == identity["target_image"].split("@", 1)[1]
            and type(ci_proof.get("selected_target")) is dict
            and ci_proof["selected_target"].get("schema") == "tilo.installer-offline-target.v1"
            and ci_proof["selected_target"].get("target_ref") == identity["target_image"]
            and ci_proof["selected_target"].get("target_digest")
            == ci_proof["installed_image_digest"]
            and ci_proof["selected_target"].get("hardware_profile") == identity["target_profile"],
            "CI-native guest proof differs from signed medium target")
    return {**identity, "identity_sha256": hashlib.sha256(raw).hexdigest()}


def checked_medium_pair(board, generic, board_checked, generic_checked,
                        board_child_digest, generic_child_digest, generic_index_image,
                        source_revision):
    require(board.get("os_source_revision", board["build_revision"]) == source_revision
            and generic.get("os_source_revision", generic["build_revision"]) == source_revision,
            "signed CI media source revision differs from signed CI OS images")
    require(board["target_profile"] == "lenovo-83ed"
            and generic["target_profile"] == "generic"
            and board["target_image"] == BOARD_REPOSITORY + "@" + board_child_digest
            and generic["target_image"] == PUBLIC_REPOSITORY + "@" + generic_child_digest
            and board["live_base_image"] == generic_index_image
            and generic["live_base_image"] == generic_index_image
            and board["build_revision"] == generic["build_revision"]
            and board["run_id"] == generic["run_id"]
            and board["run_attempt"] == generic["run_attempt"]
            and board["qcow2_sha256"] == board_checked.get("installer_medium_sha256")
            and generic["qcow2_sha256"] == generic_checked.get("installer_medium_sha256")
            and board_checked.get("first_boot_verified") is False
            and generic_checked.get("first_boot_verified") is True,
            "signed CI media do not match the paired WHPX install proofs")


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
        elif kind == "generic-child":
            if (type(external.get("generation")) is not int
                    or external["generation"] < 1
                    or any(field in external for field in
                           ("children", "hardwareProfile", "architecture",
                            "firmwareManifestSha256"))):
                continue
            firmware_candidate = None
        elif kind == "lenovo-83ed":
            firmware = [item["uri"] for item in dependencies
                        if type(item) is dict and type(item.get("uri")) is str
                        and re.fullmatch(r"code\.tilo\.so/tilo/tilo-lenovo-83ed-firmware@sha256:[0-9a-f]{64}",
                                         item["uri"])]
            candidate = firmware[0] if len(firmware) == 1 else None
            if (type(external.get("generation")) is not int or external["generation"] < 1
                    or external.get("hardwareProfile") != "lenovo-83ed"
                    or external.get("architecture") != "aarch64"
                    or type(external.get("firmwareManifestSha256")) is not str
                    or HEX.fullmatch(external["firmwareManifestSha256"]) is None
                    or "children" in external
                    or type(candidate) is not str
                    or re.fullmatch(r"code\.tilo\.so/tilo/tilo-lenovo-83ed-firmware@sha256:[0-9a-f]{64}",
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
        # Cosign normalizes the SLSA metadata field to invocationID. Retain
        # support for the producer's legacy spelling, but reject ambiguity.
        invocation_fields = [key for key in ("invocationID", "invocationId")
                             if type(metadata) is dict and key in metadata]
        invocation = metadata[invocation_fields[0]] if len(invocation_fields) == 1 else None
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
                        "invocation_id": invocation,
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


def dual_ci_medium_gate(args, board_checked, board_child_digest,
                        board_source_revision):
    """Require paired CI media and separate generic full-boot proof for board copy."""
    names = ("generic_ci_image", "generic_index_manifest", "generic_child_manifest",
             "generic_vm_proof_checked", "generic_medium_identity",
             "generic_ci_guest_proof", "board_medium_identity", "board_ci_guest_proof")
    require(all(getattr(args, name, None) is not None for name in names),
            "physical Lenovo install requires paired signed CI media and generic WHPX proof")
    index_digest, child_digest, children, kind = signed_ci_child(
        args.generic_ci_image, args.generic_index_manifest,
        args.generic_child_manifest)
    require(kind == "generic-index", "paired generic image must be signed release index")
    index_provenance = verify_ci_image(
        args.generic_ci_image, args.verify_key, children, "generic-index",
        args.cosign_command_json, args.cosign_key_arg)
    child_image = PUBLIC_REPOSITORY + "@" + child_digest
    child_provenance = verify_ci_image(
        child_image, args.verify_key, [], "generic-child",
        args.cosign_command_json, args.cosign_key_arg)
    require(all(index_provenance[field] == child_provenance[field]
                for field in ("source_revision", "generation", "invocation_id")),
            "generic child SLSA differs from signed release index")
    require(child_provenance["source_revision"] == board_source_revision,
            "generic and Lenovo CI images must come from the same source revision")
    generic_checked = checked_guest_proof(
        args.generic_vm_proof_checked, child_digest, child_image, child_provenance)
    require(generic_checked["schema"] == PROOF_SCHEMA_V2,
            "generic WHPX cycle requires signed offline target proof v2")
    board_medium = signed_medium_identity(
        args.board_medium_identity, args.board_ci_guest_proof, args.verify_key,
        args.cosign_command_json, args.cosign_key_arg)
    generic_medium = signed_medium_identity(
        args.generic_medium_identity, args.generic_ci_guest_proof, args.verify_key,
        args.cosign_command_json, args.cosign_key_arg)
    checked_medium_pair(board_medium, generic_medium, board_checked, generic_checked,
                        board_child_digest, child_digest, args.generic_ci_image,
                        board_source_revision)
    return {"generic_ci_index_digest": index_digest,
            "generic_ci_child_digest": child_digest,
            "generic_ci_source_revision": child_provenance["source_revision"],
            "generic_ci_generation": child_provenance["generation"],
            "generic_ci_slsa_statement_sha256": child_provenance["statement_sha256"],
            "generic_vm_proof_checked_sha256": hashlib.sha256(
                bounded_file(args.generic_vm_proof_checked, 64 * 1024)).hexdigest(),
            "board_medium_identity_sha256": board_medium["identity_sha256"],
            "generic_medium_identity_sha256": generic_medium["identity_sha256"],
            "board_medium_sha256": board_medium["qcow2_sha256"],
            "generic_medium_sha256": generic_medium["qcow2_sha256"]}
