#!/usr/bin/env python3
"""Authenticate and import the separately signed, offline installer target.

The live medium is transport, not the installed image.  `verify` runs before
any disk mutation; `import` rechecks the manifest after Podman loads it.
"""

import argparse
import base64
import binascii
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import tempfile


ROOT = Path("/var/lib/tilo-installer/target-oci")
KEY = Path("/usr/share/tilo/cosign.pub")
COSIGN = Path("/usr/libexec/tilo-cosign")
RECEIPT = Path("/run/tilo-installer/offline-target.json")
REF = re.compile(r"(?P<repository>registry\.tilo\.so/tilo/tilo|code\.tilo\.so/tilo/tilo)"
                 r"@(?P<digest>sha256:[0-9a-f]{64})\Z")
# Board images carry private firmware and stay on the forge's private registry.
REPOSITORY = {"generic": "registry.tilo.so/tilo/tilo", "lenovo-83ed": "code.tilo.so/tilo/tilo"}
BOARD_ORIGIN = re.compile(r"code\.tilo\.so/tilo/tilo:lenovo-83ed-[0-9a-f]{40}\Z")
GENERIC_ORIGIN = re.compile(r"registry\.tilo\.so/tilo/tilo:gen([1-9][0-9]*)\Z")
HEX = re.compile(r"[0-9a-f]{64}\Z")
BUILD_TYPE = "https://tilo.sh/buildtypes/bootc-os/v1"
BUILDER = "https://code.tilo.so/tilo/forgejo/actions/bootc-release-v1"
SOURCE = re.compile(r"git\+ssh://code\.tilo\.so/tilo/tilo@([0-9a-f]{40})\Z")
DEV_VM_MARKER = Path("/usr/share/tilo/unverified-vm-install")
DEV_FLAVOUR = Path("/usr/lib/tilo/image-flavour")
VM_PROOF_PORT = Path("/dev/virtio-ports/org.tilo.vm-install-proof")


def development_target(root):
    require(DEV_VM_MARKER.is_file() and not DEV_VM_MARKER.is_symlink()
            and DEV_FLAVOUR.is_file() and not DEV_FLAVOUR.is_symlink()
            and DEV_FLAVOUR.read_bytes() == b"dev\n"
            and VM_PROOF_PORT.is_char_device(),
            "unsigned development target requires the development VM installer")
    run("systemd-detect-virt", "--vm", "--quiet")
    module = Path(__file__).with_name("tilo-installer-target-transport.py")
    spec = importlib.util.spec_from_file_location("tilo_target_transport", module)
    transport = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(transport)
    return transport.verify(root, development=True)


def require(ok, message):
    if not ok:
        raise ValueError(message)


def run(*argv, timeout=180):
    result = subprocess.run(argv, text=True, capture_output=True, timeout=timeout)
    require(result.returncode == 0,
            f"{Path(argv[0]).name} failed (exit {result.returncode}): "
            f"{result.stderr.strip()[:1024] or 'no diagnostic stderr'}")
    return result.stdout.strip()


def checked_file(path, *, limit):
    info = path.lstat()
    require(stat.S_ISREG(info.st_mode) and not stat.S_ISLNK(info.st_mode)
            and info.st_size <= limit, f"unsafe or oversized target input: {path}")
    return path.read_bytes()


def read_target(root=ROOT):
    require(not root.is_symlink(), "offline target directory is a symlink")
    root = root.resolve(strict=True)
    require(root.is_dir(), "offline target directory is unsafe")
    metadata = json.loads(checked_file(root / "target.json", limit=8192))
    if type(metadata) is dict and metadata.get("schema") == "tilo.installer-dev-target.v1":
        return development_target(root), root / "oci"
    require(type(metadata) is dict and set(metadata) == {
        "schema", "target_ref", "target_digest", "architecture",
        "hardware_profile", "update_origin"}, "offline target metadata has invalid fields")
    require(all(type(metadata[key]) is str for key in metadata),
            "offline target metadata has invalid field types")
    match = REF.fullmatch(metadata["target_ref"])
    require(metadata["schema"] == "tilo.installer-offline-target.v1"
            and match is not None
            and metadata["target_digest"] == match["digest"]
            and metadata["architecture"] == "aarch64"
            and metadata["hardware_profile"] in {"generic", "lenovo-83ed"}
            and match["repository"] == REPOSITORY[metadata["hardware_profile"]]
            and ((metadata["hardware_profile"] == "lenovo-83ed"
                  and BOARD_ORIGIN.fullmatch(metadata["update_origin"]))
                 or (metadata["hardware_profile"] == "generic"
                     and GENERIC_ORIGIN.fullmatch(metadata["update_origin"]))),
            "offline target metadata has invalid image identity")
    layout = root / "oci"
    require(layout.is_dir() and not layout.is_symlink(), "offline OCI layout is unsafe")
    require(json.loads(checked_file(layout / "oci-layout", limit=256))
            == {"imageLayoutVersion": "1.0.0"}, "offline OCI layout version is invalid")
    index = json.loads(checked_file(layout / "index.json", limit=1024 * 1024))
    require(type(index) is dict and index.get("schemaVersion") == 2
            and type(index.get("manifests")) is list, "offline OCI index is invalid")
    descriptors = [item for item in index["manifests"]
                   if type(item) is dict and item.get("digest") == metadata["target_digest"]]
    require(len(descriptors) == 1, "offline OCI index has no unique target manifest")
    descriptor = descriptors[0]
    annotations = descriptor.get("annotations")
    require(descriptor.get("mediaType") in {
        "application/vnd.oci.image.manifest.v1+json",
        "application/vnd.docker.distribution.manifest.v2+json"}
        and type(annotations) is dict and annotations.get("kind")
        == "dev.cosignproject.cosign/image",
        "offline OCI target descriptor is not the selected image")
    manifest_path = layout / "blobs/sha256" / match["digest"].removeprefix("sha256:")
    manifest = checked_file(manifest_path, limit=16 * 1024 * 1024)
    require(len(manifest) == descriptor.get("size")
            and hashlib.sha256(manifest).hexdigest() == match["digest"].removeprefix("sha256:"),
            "offline OCI manifest differs from pinned CI digest")
    parsed_manifest = json.loads(manifest)
    config = parsed_manifest.get("config")
    require(parsed_manifest.get("schemaVersion") == 2 and type(config) is dict
            and type(config.get("digest")) is str
            and re.fullmatch(r"sha256:[0-9a-f]{64}", config["digest"])
            and type(config.get("size")) is int and 0 < config["size"] <= 16 * 1024 * 1024,
            "offline target manifest has invalid config descriptor")
    config_bytes = checked_file(layout / "blobs/sha256" / config["digest"][7:],
                                limit=16 * 1024 * 1024)
    require(len(config_bytes) == config["size"]
            and hashlib.sha256(config_bytes).hexdigest() == config["digest"][7:],
            "offline target config differs from pinned manifest")
    image_config = json.loads(config_bytes)
    require(type(image_config) is dict and image_config.get("architecture") == "arm64"
            and image_config.get("os") == "linux",
            "offline target image is not Linux ARM64")
    return metadata, layout


def wrappers(output):
    require(0 < len(output) <= 8 * 1024 * 1024, "Cosign verification output is invalid")
    try:
        values = [json.loads(output)]
    except json.JSONDecodeError:
        values = [json.loads(line) for line in output.splitlines() if line.strip()]
    rows = [row for value in values for row in (value if type(value) is list else [value])]
    require(rows and all(type(row) is dict for row in rows), "Cosign verification output is malformed")
    return rows


def checked_provenance(output, metadata):
    matches = []
    for row in wrappers(output):
        try:
            statement = json.loads(base64.b64decode(row["payload"], validate=True))
        except (KeyError, TypeError, ValueError, binascii.Error):
            continue
        if type(statement) is not dict:
            continue
        if statement.get("predicateType") != "https://slsa.dev/provenance/v1":
            continue
        subjects = statement.get("subject")
        predicate = statement.get("predicate")
        if not isinstance(subjects, list) or not isinstance(predicate, dict):
            continue
        if not any(type(item) is dict and type(item.get("digest")) is dict
                   and item["digest"].get("sha256") == metadata["target_digest"][7:]
                   for item in subjects):
            continue
        definition = predicate.get("buildDefinition", {})
        details = predicate.get("runDetails", {})
        if not isinstance(definition, dict) or not isinstance(details, dict):
            continue
        external = definition.get("externalParameters", {})
        internal = definition.get("internalParameters", {})
        dependencies = definition.get("resolvedDependencies", [])
        byproducts = details.get("byproducts", [])
        builder = details.get("builder")
        if (definition.get("buildType") != BUILD_TYPE
                or type(builder) is not dict or builder.get("id") != BUILDER
                or not isinstance(external, dict) or not isinstance(internal, dict)
                or not isinstance(dependencies, list) or not isinstance(byproducts, list)
                or internal.get("expectedReleaseDigest") != metadata["target_digest"]
                or type(external.get("generation")) is not int
                or external["generation"] < 1
                or not any(type(item) is dict and item.get("name") == "independent-rebuild"
                           and item.get("uri") == "urn:tilo:verification:manifest-digest-equal"
                           for item in byproducts)):
            continue
        sources = [SOURCE.fullmatch(item.get("uri", "")) for item in dependencies
                   if type(item) is dict and type(item.get("uri")) is str]
        sources = [item[1] for item in sources if item is not None]
        firmware = dependencies[-1].get("uri") if dependencies and type(dependencies[-1]) is dict else None
        run_metadata = details.get("metadata")
        invocation = run_metadata.get("invocationId") if type(run_metadata) is dict else None
        if len(sources) != 1 or not re.fullmatch(r"[0-9]+", str(invocation)):
            continue
        if metadata["hardware_profile"] == "lenovo-83ed":
            if (external.get("generation", 0) < 1
                    or external.get("architecture") != "aarch64"
                    or external.get("hardwareProfile") != "lenovo-83ed"
                    or not isinstance(external.get("firmwareManifestSha256"), str)
                    or not HEX.fullmatch(external["firmwareManifestSha256"])
                    or "children" in external
                    or metadata["update_origin"] !=
                    f"code.tilo.so/tilo/tilo:lenovo-83ed-{sources[0]}"
                    or type(firmware) is not str
                    or not re.fullmatch(r"code\.tilo\.so/tilo/tilo-lenovo-83ed-firmware@sha256:[0-9a-f]{64}", firmware)):
                continue
            firmware_manifest = external["firmwareManifestSha256"]
        else:
            if (metadata["update_origin"] !=
                    f"registry.tilo.so/tilo/tilo:gen{external['generation']}"
                    or any(key in external for key in
                           ("hardwareProfile", "architecture", "firmwareManifestSha256", "children"))):
                continue
            firmware = None
            firmware_manifest = None
        matches.append({"source_revision": sources[0],
                        "firmware_candidate": firmware,
                        "firmware_manifest_sha256": firmware_manifest,
                        "generation": external["generation"],
                        "invocation_id": invocation})
    require(len(matches) == 1, "offline target has no unique CI SLSA provenance")
    return matches[0]


def verify(root=ROOT):
    metadata, layout = read_target(root)
    if metadata["schema"] == "tilo.installer-dev-target.v1":
        result = dict(metadata, verifier_key_sha256=None, provenance={
            "source_revision": metadata["source_revision"],
            "invocation_id": metadata["ci_run_id"],
        })
        save_receipt(result)
        import_image(root)
        return result
    require(KEY.is_file() and COSIGN.is_file() and os.access(COSIGN, os.X_OK),
            "immutable Cosign verifier or public key is missing")
    signature = wrappers(run(str(COSIGN), "verify", "--offline", "--key", str(KEY),
                             "--local-image", str(layout), timeout=300))
    require(any(type(row.get("critical")) is dict
                and row["critical"].get("image", {}).get("docker-manifest-digest")
                == metadata["target_digest"]
                and row["critical"].get("identity", {}).get("docker-reference")
                == REPOSITORY[metadata["hardware_profile"]] for row in signature),
            "offline Cosign signature does not bind the selected target")
    provenance = checked_provenance(run(str(COSIGN), "verify-attestation", "--offline",
                                        "--key", str(KEY), "--type", "slsaprovenance1",
                                        "--local-image", str(layout), timeout=300), metadata)
    result = dict(metadata, provenance=provenance,
                  verifier_key_sha256=hashlib.sha256(KEY.read_bytes()).hexdigest())
    save_receipt(result)
    return result


def save_receipt(result):
    RECEIPT.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    temporary = RECEIPT.with_suffix(".tmp")
    temporary.write_text(json.dumps(result, sort_keys=True, separators=(",", ":")) + "\n")
    temporary.chmod(0o600)
    temporary.replace(RECEIPT)


def import_image(root=ROOT):
    metadata, layout = read_target(root)
    require(RECEIPT.is_file() and not RECEIPT.is_symlink(),
            "offline target has no verified preflight")
    receipt = json.loads(checked_file(RECEIPT, limit=8192))
    require(type(receipt) is dict
            and all(receipt.get(key) == value for key, value in metadata.items()),
            "offline target metadata changed after verified preflight")
    # Cosign's saved layout also contains signature/attestation descriptors.
    # Construct a tiny one-descriptor view with a symlink to the same hashed
    # blobs. Skopeo transfers directly into the local store; `podman load`
    # stages another archive-sized copy in /var/tmp before importing it.
    destination = 'localhost/tilo-offline-target:' + metadata['target_digest'][7:]
    index = json.loads(checked_file(layout / "index.json", limit=1024 * 1024))
    descriptor = next(item for item in index["manifests"]
                      if item.get("digest") == metadata["target_digest"])
    descriptor = dict(descriptor, annotations=dict(descriptor.get("annotations", {}),
                       **{"org.opencontainers.image.ref.name": "target"}))
    with tempfile.TemporaryDirectory(prefix="tilo-target-view-", dir=RECEIPT.parent) as temp:
        view = Path(temp)
        (view / "oci-layout").write_text('{"imageLayoutVersion":"1.0.0"}')
        (view / "index.json").write_text(json.dumps({
            "schemaVersion": 2, "manifests": [descriptor]}, separators=(",", ":")))
        (view / "blobs").symlink_to(layout / "blobs", target_is_directory=True)
        # Authority was checked above. This local copy validates blob hashes
        # and preserves the exact selected manifest; no registry is contacted.
        run('skopeo', '--insecure-policy', 'copy', '--preserve-digests',
            f'oci:{view}:target', 'containers-storage:' + destination, timeout=3600)
    image_id = run('podman', 'image', 'inspect', '--format', '{{.Id}}', destination, timeout=120)
    require(re.fullmatch(r'(?:sha256:)?[0-9a-f]{64}', image_id),
            'offline OCI import returned no immutable image ID')
    imported_digest = run("podman", "image", "inspect", "--format", "{{.Digest}}",
                          image_id, timeout=120)
    require(imported_digest == metadata["target_digest"],
            "imported image manifest differs from the signed target")
    require(re.fullmatch(r"(?:sha256:)?[0-9a-f]{64}", image_id),
            "offline Podman import returned no immutable image ID")
    if metadata["schema"] == "tilo.installer-dev-target.v1":
        run("podman", "run", "--rm", "--pull=never", "--network=none", "--cap-drop=ALL",
            "--entrypoint", "/bin/sh", image_id, "-ec",
            'test "$(cat /usr/lib/tilo/image-flavour)" = dev; '
            'test -x /usr/sbin/sshd; test -f /usr/lib/systemd/system/sshd.service; '
            'test -d /usr/lib/modules', timeout=120)
        mounted = Path(run('podman', 'image', 'mount', image_id, timeout=120))
        try:
            require(mounted.is_absolute() and mounted.is_dir(), 'development image mount is unsafe')
            if not (mounted / 'boot/tilo-target/tilo-target.efi').is_file():
                run('python3', '/usr/libexec/tilo-installer-dev-target-uki.py',
                    'validate-image', '--image-root', str(mounted), timeout=120)
        finally:
            run('podman', 'image', 'unmount', image_id, timeout=120)
    run("podman", "run", "--rm", "--pull=never", "--network=none", "--cap-drop=ALL",
        "--entrypoint", "/usr/bin/python3", image_id,
        "/usr/lib/tilo/hardware_profile.py", "--install-update-origin",
        metadata["update_origin"], timeout=120)
    # Bootc consumes this immutable ID; the guest proof checks its resulting
    # deployment's manifest digest and origin against the signed receipt.
    return image_id


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("verify", "import"))
    args = parser.parse_args()
    try:
        if args.command == "verify":
            print(json.dumps(verify(), sort_keys=True))
        else:
            print(import_image())
    except (ValueError, OSError, KeyError, json.JSONDecodeError) as exc:
        parser.exit(1, f"offline installer target: {exc}\n")


if __name__ == "__main__":
    main()
