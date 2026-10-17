#!/usr/bin/env python3
"""Bind final installer build provenance to a qcow2 before release packaging.

This records build provenance, not a physical boot or UKI acceptance result.
The release catalog/signatures authenticate the packaged media independently.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "os/dev"))
from hardware_profile import (  # noqa: E402
    ARCH_CONFIG, PROFILE_FIELDS, load_definition, profile_id,
    validate_definition, validate_target_imgref,
)

FIELDS = {"schema_version", "source_image_id", "architecture", "hardware_profile",
          "profile_json", "profile_sha256", "definition", "qcow2_sha256", "qcow2_size"}
CONTAINER_READ = """
import json, os, subprocess, sys
from pathlib import Path
sys.path.insert(0, '/run/tilo-media-validation')
from hardware_profile import read_profile, read_definition, trusted_path, PROFILE_PATH, DEFINITION_PATH
profile = read_profile(Path('/'))
exporter = Path('/usr/libexec/tilo-prepared-setup')
# Images without the prepared setup helper carry no consumer facts; an
# installed helper's catalog export must succeed.
facts = None
if os.path.lexists(exporter):
    trusted_path(Path('/'), exporter.relative_to('/'), regular=True)
    result = subprocess.run([str(exporter), '--data-consumer-catalog-facts'], check=True,
                            capture_output=True, timeout=140,
                            env={'PATH':'/usr/bin:/bin','LANG':'C','LC_ALL':'C'})
    if len(result.stdout) > 16384 or len(result.stderr) > 4096:
        raise ValueError('installed consumer facts exceed bound')
    facts = json.loads(result.stdout)
print(json.dumps({
    'data_consumers': facts,
    'profile_json': (Path('/') / PROFILE_PATH).read_bytes().decode('utf-8') if profile is not None else None,
    'definition': read_definition(trusted_path(Path('/'), DEFINITION_PATH, regular=True)) if profile is not None else None,
}))
"""


# The closed receipts the prepared setup helper's consumer checks export
# (crates/platform/imager-backend/src/installed_consumers/data.rs).
HOME_KEYS = {"schema", "archive_protocol", "seed_protocol", "firstboot_user_sha256",
             "seed_helper_sha256", "prepared_helper_sha256", "tar_sha256"}
APPS_KEYS = {"schema", "subset_protocol", "prepared_helper_sha256",
             "qualification_script_sha256", "native_qualification_sha256"}


def consumer_shape(value, keys, protocols):
    if type(value) is not dict or set(value) != keys:
        raise ValueError("unknown or missing consumer fields")
    for key, expected in protocols.items():
        if type(value[key]) is not int or value[key] != expected:
            raise ValueError("unsupported consumer protocol")
    for key in keys:
        if key.endswith("_sha256") and not (type(value[key]) is str
                                            and re.fullmatch("[0-9a-f]{64}", value[key])):
            raise ValueError("invalid consumer digest")
    return value


def validate_consumers(value):
    if not isinstance(value, dict) or set(value) not in ({'home_archive'}, {'home_archive', 'applications', 'original_source'}):
        raise ValueError('unknown or incomplete installed consumer facts')
    consumer_shape(value['home_archive'], HOME_KEYS,
                   {'schema': 1, 'archive_protocol': 2, 'seed_protocol': 1})
    if 'applications' in value:
        consumer_shape(value['applications'], APPS_KEYS, {'schema': 1, 'subset_protocol': 2})
        original = value['original_source']
        if not isinstance(original, dict) or type(original.get('original_source_protocol')) is not int or original['original_source_protocol'] != 6:
            raise ValueError('application facts lack the qualified source6 consumer')
    return value


def digest(path):
    if path.is_symlink() or not path.is_file() or path.stat().st_size == 0:
        raise ValueError("installer qcow2 must be a nonempty regular file")
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def snapshot_profile(snapshot, architecture):
    if architecture not in ARCH_CONFIG:
        raise ValueError("unsupported media architecture")
    payload = snapshot["profile_json"]
    definition = snapshot["definition"]
    if payload is None:
        if definition is not None:
            raise ValueError("hardware definition is missing its generated profile")
        return "generic", None, None
    if not isinstance(payload, str) or len(payload.encode()) > 16384:
        raise ValueError("invalid generated hardware profile payload")
    data = json.loads(payload)
    definition = validate_definition(definition)
    if not isinstance(data, dict) or set(data) != PROFILE_FIELDS:
        raise ValueError("generated hardware profile has missing or unknown fields")
    if type(data["schema_version"]) is not int or data["schema_version"] != 1:
        raise ValueError("unsupported generated hardware profile schema")
    name = profile_id(data["id"])
    if name != definition["id"] or data["architecture"] != architecture or definition["architecture"] != architecture:
        raise ValueError("generated hardware profile identity does not match the image architecture")
    if load_definition(name) != definition:
        raise ValueError("final image hardware definition differs from the reviewed packaging definition")
    origin = validate_target_imgref(data["target_update_ref"])
    return name, hashlib.sha256(payload.encode()).hexdigest(), origin


def create(image, qcow):
    inspection = subprocess.check_output(
        ["podman", "image", "inspect", image, "--format", "{{.Id}} {{.Architecture}}"], text=True
    ).strip().split()
    if len(inspection) != 2 or not re.fullmatch(r"(?:sha256:)?[0-9a-f]{64}", inspection[0]):
        raise ValueError("final installer image has no immutable OCI identity")
    native_id, architecture = inspection
    native_id = native_id.removeprefix("sha256:")
    image_id = "sha256:" + native_id
    architecture = {"arm64": "aarch64", "amd64": "x86_64"}.get(architecture, architecture)
    # Validate immutable, root-owned profile metadata inside the final image.
    # Pin the run to the inspected ID so a changing tag cannot swap its bytes.
    with tempfile.TemporaryDirectory(prefix="tilo-media-validator-") as validation_dir:
        shutil.copyfile(REPO / "os/dev/hardware_profile.py", Path(validation_dir) / "hardware_profile.py")
        snapshot = json.loads(subprocess.check_output([
            "podman", "run", "--rm", "--network=none", "--read-only", "--user=0",
            "--volume", f"{validation_dir}:/run/tilo-media-validation:ro,Z",
            "--entrypoint", "python3", native_id, "-c", CONTAINER_READ,
        ], text=True))
    name, profile_hash, _ = snapshot_profile(snapshot, architecture)
    record = {
        "schema_version": 1, "source_image_id": image_id,
        "architecture": architecture, "hardware_profile": name,
        "profile_json": snapshot["profile_json"], "profile_sha256": profile_hash,
        "definition": snapshot["definition"],
        "qcow2_sha256": digest(qcow), "qcow2_size": qcow.stat().st_size,
    }
    if snapshot.get("data_consumers") is not None:
        record["data_consumers"] = validate_consumers(snapshot["data_consumers"])
    sidecar = Path(str(qcow) + ".hardware.json")
    if sidecar.is_symlink():
        raise ValueError("media provenance destination must not be a symlink")
    sidecar.write_text(json.dumps(record, indent=2) + "\n")
    return sidecar


def validate(qcow, architecture, name):
    profile_id(name)
    if architecture not in ARCH_CONFIG:
        raise ValueError("unsupported media architecture")
    if name != "generic" and load_definition(name)["architecture"] != architecture:
        raise ValueError("hardware profile requires its declared media architecture")
    sidecar = Path(str(qcow) + ".hardware.json")
    if sidecar.is_symlink():
        raise ValueError("media provenance sidecar must not be a symlink")
    if not sidecar.exists() and name == "generic":
        # Deliberate compatibility: old generic builders did not emit provenance.
        return None
    if not sidecar.is_file() or sidecar.stat().st_size > 131072:
        raise ValueError("board media requires a bounded regular .hardware.json provenance sidecar")
    record = json.loads(sidecar.read_text())
    if not isinstance(record, dict) or set(record) not in (FIELDS, FIELDS | {"data_consumers"}) or type(record["schema_version"]) is not int or record["schema_version"] != 1:
        raise ValueError("invalid media provenance schema")
    if 'data_consumers' in record:
        validate_consumers(record['data_consumers'])
    actual_name, profile_hash, origin = snapshot_profile(record, architecture)
    if record["architecture"] != architecture or record["hardware_profile"] != name or actual_name != name:
        raise ValueError("packaging profile does not match the final installer image")
    if record["profile_sha256"] != profile_hash or not re.fullmatch(r"sha256:[0-9a-f]{64}", record["source_image_id"]):
        raise ValueError("media provenance has an invalid profile or OCI digest")
    if type(record["qcow2_size"]) is not int or record["qcow2_size"] != qcow.stat().st_size or record["qcow2_sha256"] != digest(qcow):
        raise ValueError("installer qcow2 changed after its hardware provenance was recorded")
    return {
        "source_image_id": record["source_image_id"],
        "input_qcow2_sha256": record["qcow2_sha256"],
        "hardware_profile_sha256": profile_hash,
        "target_update_ref": origin,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    make = sub.add_parser("create")
    make.add_argument("image")
    make.add_argument("qcow", type=Path)
    check = sub.add_parser("validate")
    check.add_argument("qcow", type=Path)
    check.add_argument("architecture")
    check.add_argument("profile")
    facts = sub.add_parser("consumer-facts")
    facts.add_argument("qcow", type=Path)
    facts.add_argument("architecture")
    facts.add_argument("profile")
    args = parser.parse_args()
    try:
        if args.command == "create":
            print(create(args.image, args.qcow))
        elif args.command == "consumer-facts":
            validate(args.qcow, args.architecture, args.profile)
            sidecar = Path(str(args.qcow) + ".hardware.json")
            record = json.loads(sidecar.read_text()) if sidecar.exists() else {}
            print(json.dumps(record.get("data_consumers", {})))
        else:
            print(json.dumps(validate(args.qcow, args.architecture, args.profile)))
    except (OSError, ValueError, TypeError, KeyError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"media provenance rejected: {error}") from error


if __name__ == "__main__":
    main()
