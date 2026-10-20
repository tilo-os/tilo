#!/usr/bin/env python3
"""Export one architecture's signed target policy from the exact published OS image.

The release catalog's `tilo-target-policy` entry selects the small boot bundle a
preparation host reviews instead of mounting a live installer. This producer is
the missing other half: it reads `/boot/tilo-target` from a digest-pinned,
published Tilo image, verifies the image's SCM revision label and the bundle's
release-authority signatures, and writes the deterministic archive and catalog
fragment that `build-release-catalog.sh` merges.

    apps/imager/packaging/export-target-policy.py export \
      --image registry.tilo.so/tilo/tilo@sha256:<child> \
      --arch x86_64 --release-id imager-v1.2.3 \
      --expected-source <40-hex commit> --output dist/tilo-target-policy

The image must be the exact child of the published multi-architecture release
index, and its `org.opencontainers.image.revision` label must name the commit
whose release this is. The archive is exactly `tilo-target.efi` and one
`predictions/generation-<N>.json` with its `.uki.sig`; both signatures verify
against the pinned `keys/tpm2-pcr-signing.public.pem` authority, using the same
RSA PKCS#1 v1.5 SHA-256 messages as `sign-target-prediction.sh` and
`tilo-crypto verify-rsa-sha256[-efi]`.

A development rehearsal may pass `--diagnostic`. It then exports the same bytes
as `tilo-target-policy-<ARCH>.diagnostic.tar` without checking signatures and
without ever writing a qualified `target-policy-catalog-<ARCH>.json` fragment.
It cannot be used to populate a catalog.
"""

import argparse
import base64
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import sys
import tarfile
import tempfile
from urllib.parse import urlsplit

REPO = Path(__file__).resolve().parents[3]
TARGET_UKI_FILE = "tilo-target.efi"
PREDICTIONS_DIR = "predictions"
PURPOSE = "tilo-target-policy"
LAYOUT = "target-policy-bundle-v1"
PACKAGE = "tilo-target-policy"
PACKAGE_ORIGIN = "https://code.tilo.so"
PACKAGE_ORIGIN_HOST = "code.tilo.so"
DEFAULT_AUTHORITY = REPO / "keys/tpm2-pcr-signing.public.pem"
DEFAULT_OWNER = "tilo"
BUILD_TYPE = "https://tilo.sh/buildtypes/bootc-os/v1"
MAX_UKI = 512 * 1024 * 1024
MAX_PREDICTION = 64 * 1024
MAX_SIGNATURE = 4 * 1024
MAX_ARCHIVE_BYTES = MAX_UKI + MAX_PREDICTION + MAX_SIGNATURE + 64 * 1024
RELEASE_ID = re.compile(r"imager-v[0-9]+\.[0-9]+\.[0-9]+\Z")
OWNER = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,63}\Z")
SOURCE_REVISION = re.compile(r"[0-9a-f]{40}\Z")
DIGEST = re.compile(r"sha256:[0-9a-f]{64}\Z")
IMAGE = re.compile(r"(?P<repository>registry\.tilo\.so/tilo/tilo)@(?P<digest>sha256:[0-9a-f]{64})\Z")
ARCHITECTURES = {"x86_64": 0x8664, "aarch64": 0xAA64}
CONFIG_ARCHITECTURES = {"x86_64": "amd64", "aarch64": "arm64"}
PREDICTION_KEYS = {"generation", "pcr_bank", "pcrs", "approved_policy", "signature"}


def invalid(message):
    raise ValueError(message)


# Reuse the canonical release-authority verification code. It is the same
# standard-library RSA PKCS#1 v1.5 SHA-256 check the release-signer probe uses.
def _authority_module():
    path = REPO / "os/dev/probe-release-signer.py"
    spec = importlib.util.spec_from_file_location("tilo_release_signer_probe", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def authority_key(path):
    module = _authority_module()
    der = module.pem_der(Path(path).read_text(encoding="ascii"), "PUBLIC KEY")
    return module.rsa_public(der)


def verify_authority_signature(authority, message, signature, what):
    if not _authority_module().verify_pkcs1_sha256(authority, message, signature):
        invalid(f"{what} does not carry the pinned release authority's signature")


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def sha256_file(path):
    with Path(path).open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def bounded_bytes(path, limit, what):
    target = Path(path)
    if target.is_symlink() or not target.is_file():
        invalid(f"{what} is not a regular file")
    size = target.stat().st_size
    if size < 1 or size > limit:
        invalid(f"{what} is empty or oversized")
    return target.read_bytes()


def pe_machine(data):
    if len(data) < 0x40 or data[:2] != b"MZ":
        invalid("target UKI is not a PE image")
    lfanew = struct.unpack_from("<I", data, 0x3C)[0]
    if lfanew < 0x40 or lfanew + 6 > len(data) or data[lfanew:lfanew + 4] != b"PE\0\0":
        invalid("target UKI has no PE header")
    return struct.unpack_from("<H", data, lfanew + 4)[0]


def parse_prediction(raw, generation=None):
    try:
        document = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        invalid("target prediction is not JSON")
    if not isinstance(document, dict) or not PREDICTION_KEYS <= set(document) <= PREDICTION_KEYS | {"label"}:
        invalid("target prediction has missing or unknown fields")
    value = document.get("generation")
    if type(value) is not int or value <= 0:
        invalid("target prediction has no positive generation")
    if generation is not None and value != generation:
        invalid(f"target prediction is not for generation {generation}")
    if document.get("pcr_bank") != "sha256" or document.get("pcrs") != [11]:
        invalid("target prediction is not the SHA-256 PCR 11 policy")
    approved = document.get("approved_policy")
    signature = document.get("signature")
    if not isinstance(approved, str) or not isinstance(signature, str):
        invalid("target prediction has no approved policy or signature")
    try:
        approved_bytes = base64.b64decode(approved, validate=True)
        signature_bytes = base64.b64decode(signature, validate=True)
    except (ValueError, base64.binascii.Error):
        invalid("target prediction fields are not base64")
    if len(approved_bytes) != 32:
        invalid("target prediction approved policy is not a SHA-256 digest")
    if not 256 <= len(signature_bytes) <= MAX_SIGNATURE:
        invalid("target prediction signature has an invalid size")
    if document.get("label", "") and not isinstance(document.get("label"), str):
        invalid("target prediction label is not text")
    return value, approved_bytes, signature_bytes


def verify_bundle_files(uki, prediction, uki_signature, authority_path, *, generation=None, arch=None):
    if not uki or len(uki) > MAX_UKI:
        invalid("target UKI is empty or oversized")
    if not prediction or len(prediction) > MAX_PREDICTION:
        invalid("target prediction is empty or oversized")
    if not uki_signature or len(uki_signature) > MAX_SIGNATURE:
        invalid("target UKI signature is empty or oversized")
    actual_generation, approved, policy_signature = parse_prediction(prediction, generation)
    authority = authority_key(authority_path)
    verify_authority_signature(
        authority,
        approved + struct.pack(">Q", actual_generation),
        policy_signature,
        "target generation prediction",
    )
    verify_authority_signature(authority, uki, uki_signature, "target UKI")
    if arch is not None and pe_machine(uki) != ARCHITECTURES[arch]:
        invalid(f"target UKI is not a {arch} PE image")
    return {
        "generation": actual_generation,
        "target_uki_sha256": sha256(uki),
    }


def expected_predictions(generation):
    return {
        f"generation-{generation}.json",
        f"generation-{generation}.json.uki.sig",
    }


def expected_names(generation):
    return {TARGET_UKI_FILE} | {
        f"{PREDICTIONS_DIR}/{name}" for name in expected_predictions(generation)
    }


def prediction_generation(name):
    prefix = f"{PREDICTIONS_DIR}/"
    if name.startswith(prefix):
        name = name[len(prefix):]
    if not name.startswith("generation-") or not name.endswith(".json"):
        return None
    digits = name[len("generation-"):-len(".json")]
    if not digits or len(digits) > 20 or digits.startswith("0") or not digits.isdigit():
        return None
    return int(digits)


def verify_bundle_directory(root, authority_path, *, generation=None, arch=None):
    root = Path(root)
    if root.is_symlink() or not root.is_dir():
        invalid("target bundle is not a directory")
    entries = {item.name for item in root.iterdir()}
    if entries != {TARGET_UKI_FILE, PREDICTIONS_DIR}:
        invalid("target bundle has missing or extra entries")
    predictions = root / PREDICTIONS_DIR
    if predictions.is_symlink() or not predictions.is_dir():
        invalid("target bundle predictions path is not a directory")
    names = {item.name for item in predictions.iterdir()}
    candidates = sorted({value for name in names if (value := prediction_generation(name)) is not None})
    if len(candidates) != 1:
        invalid("target bundle must carry exactly one generation prediction")
    actual_generation = candidates[0]
    if generation is not None and actual_generation != generation:
        invalid(f"target bundle does not carry generation {generation}")
    if names != expected_predictions(actual_generation):
        invalid("target bundle has missing or extra prediction entries")
    uki = bounded_bytes(root / TARGET_UKI_FILE, MAX_UKI, "target UKI")
    prediction = bounded_bytes(predictions / f"generation-{actual_generation}.json", MAX_PREDICTION, "target prediction")
    signature = bounded_bytes(
        predictions / f"generation-{actual_generation}.json.uki.sig",
        MAX_SIGNATURE,
        "target UKI signature",
    )
    return verify_bundle_files(
        uki, prediction, signature, authority_path, generation=actual_generation, arch=arch
    )


def read_archive(path, authority_path, *, generation=None, arch=None):
    path = Path(path)
    if path.is_symlink() or not path.is_file():
        invalid("target policy archive is not a regular file")
    if path.stat().st_size > MAX_ARCHIVE_BYTES:
        invalid("target policy archive is oversized")
    files = {}
    with tarfile.open(path, "r:") as archive:
        members = archive.getmembers()
        if not members or len(members) != 3:
            invalid("target policy archive must contain exactly three files")
        for member in members:
            if not member.isfile() or member.name in files:
                invalid("target policy archive contains a non-file or duplicate entry")
            name = member.name
            if name == TARGET_UKI_FILE:
                limit = MAX_UKI
            elif prediction_generation(name) is not None:
                limit = MAX_PREDICTION
            elif name.endswith(".json.uki.sig") and prediction_generation(name[:-len(".uki.sig")]) is not None:
                limit = MAX_SIGNATURE
            else:
                invalid("target policy archive contains an unexpected entry")
            if member.size < 1 or member.size > limit:
                invalid("target policy archive entry is empty or oversized")
            source = archive.extractfile(member)
            if source is None:
                invalid("target policy archive entry is unreadable")
            files[name] = source.read(limit + 1)
            if len(files[name]) != member.size:
                invalid("target policy archive entry changed while reading")
    if len(files) != 3:
        invalid("target policy archive must contain exactly three files")
    generations = {prediction_generation(name) for name in files if prediction_generation(name) is not None}
    if len(generations) != 1:
        invalid("target policy archive must contain exactly one generation prediction")
    actual_generation = generations.pop()
    if set(files) != expected_names(actual_generation):
        invalid("target policy archive has missing or unmatched entries")
    result = verify_bundle_files(
        files[TARGET_UKI_FILE],
        files[f"{PREDICTIONS_DIR}/generation-{actual_generation}.json"],
        files[f"{PREDICTIONS_DIR}/generation-{actual_generation}.json.uki.sig"],
        authority_path,
        generation=actual_generation,
        arch=arch,
    )
    return result, files


def write_archive(path, files):
    path = Path(path)
    temporary = path.with_name(f".{path.name}.tmp")
    with tarfile.open(temporary, "w", format=tarfile.USTAR_FORMAT) as archive:
        for name in sorted(files):
            data = files[name]
            member = tarfile.TarInfo(name)
            member.size = len(data)
            member.mtime = 0
            member.uid = member.gid = 0
            member.uname = member.gname = ""
            member.mode = 0o644
            member.type = tarfile.REGTYPE
            archive.addfile(member, io.BytesIO(data))
    os.replace(temporary, path)


def package_base(owner, release_id):
    return f"{PACKAGE_ORIGIN}/api/packages/{owner}/generic/{PACKAGE}/{release_id}"


def fragment_for(arch, release_id, owner, generation, target_uki_sha256, archive):
    archive = Path(archive)
    entry = {
        "purpose": PURPOSE,
        "layout": LAYOUT,
        "release_id": release_id,
        "arch": arch,
        "generation": generation,
        "target_uki_sha256": target_uki_sha256,
        "url": f"{package_base(owner, release_id)}/tilo-target-policy-{arch}.tar",
        "archive_sha256": sha256_file(archive),
        "archive_size": archive.stat().st_size,
    }
    validate_fragment(entry, release_id, owner)
    return entry


def validate_fragment(entry, release_id, owner):
    expected = {"purpose", "layout", "release_id", "arch", "generation",
                "target_uki_sha256", "url", "archive_sha256", "archive_size"}
    if not isinstance(entry, dict) or set(entry) != expected:
        invalid("target policy fragment has missing or unknown fields")
    if (entry["purpose"], entry["layout"], entry["release_id"]) != (PURPOSE, LAYOUT, release_id):
        invalid("target policy fragment does not describe this release")
    if entry["arch"] not in ARCHITECTURES or type(entry["generation"]) is not int or entry["generation"] <= 0:
        invalid("target policy fragment architecture or generation is invalid")
    for field in ("target_uki_sha256", "archive_sha256"):
        if not isinstance(entry[field], str) or not re.fullmatch(r"[0-9a-f]{64}", entry[field]):
            invalid(f"target policy fragment {field} is not a SHA-256 digest")
    if type(entry["archive_size"]) is not int or entry["archive_size"] <= 0:
        invalid("target policy fragment archive size is invalid")
    url = urlsplit(entry["url"])
    if (url.scheme != "https" or url.hostname != PACKAGE_ORIGIN_HOST
            or url.port not in (None, 443) or url.username or url.password
            or url.query or url.fragment):
        invalid("target policy fragment URL is not the trusted package origin")
    expected_url = f"{package_base(owner, release_id)}/tilo-target-policy-{entry['arch']}.tar"
    if entry["url"] != expected_url:
        invalid("target policy fragment URL does not match its package and architecture")


def check_image_config(config, arch, expected_source):
    if not isinstance(config, dict):
        invalid("image config is not an object")
    if config.get("architecture") != CONFIG_ARCHITECTURES[arch] or config.get("os") != "linux":
        invalid(f"published image is not Linux {arch}")
    labels = config.get("config", {}).get("Labels")
    if not isinstance(labels, dict):
        invalid("published image carries no labels")
    revision = labels.get("org.opencontainers.image.revision")
    if not isinstance(revision, str) or not SOURCE_REVISION.fullmatch(revision):
        invalid("published image has no full source revision label")
    if revision != expected_source:
        invalid(f"published image source {revision} differs from the committed source {expected_source}")
    return revision


def inspect_image_config(image, authfile):
    command = ["skopeo", "inspect", "--config", f"docker://{image}"]
    if authfile:
        command[2:2] = ["--authfile", authfile]
    result = subprocess.run(command, check=False, capture_output=True, text=True)
    if result.returncode:
        invalid(f"cannot inspect the published image: {result.stderr.strip()[-400:]}")
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError:
        invalid("published image config is not JSON")


def verify_image_signature(image, digest):
    key = REPO / "keys/cosign.pub"
    result = subprocess.run(["cosign", "verify", "--key", str(key), image],
                            check=False, capture_output=True, text=True)
    if result.returncode:
        invalid("published image has no valid release signature")
    try:
        rows = json.loads(result.stdout)
    except json.JSONDecodeError:
        invalid("release signature output is not JSON")
    if not isinstance(rows, list):
        rows = [rows]
    bound = {row.get("critical", {}).get("image", {}).get("docker-manifest-digest")
             for row in rows if isinstance(row, dict)}
    if digest not in bound:
        invalid("published image signature does not bind its exact digest")


def verify_image_provenance(image, expected_source, generation):
    key = REPO / "keys/cosign.pub"
    result = subprocess.run(
        ["cosign", "verify-attestation", "--key", str(key), "--type", "slsaprovenance1", image],
        check=False, capture_output=True, text=True)
    if result.returncode:
        invalid("published image has no valid SLSA provenance")
    try:
        rows = json.loads(result.stdout)
    except json.JSONDecodeError:
        invalid("release provenance output is not JSON")
    if not isinstance(rows, list):
        rows = [rows]
    source_uri = f"git+ssh://code.tilo.so/tilo/tilo@{expected_source}"
    digest = _image_digest(image)
    subject_sha = digest.removeprefix("sha256:")
    for row in rows:
        if not isinstance(row, dict):
            continue
        try:
            statement = json.loads(base64.b64decode(row["payload"], validate=True))
            predicate = statement["predicate"]
            definition = predicate["buildDefinition"]
            internal = definition["internalParameters"]
            external = definition["externalParameters"]
            dependencies = definition["resolvedDependencies"]
            subjects = statement["subject"]
        except (KeyError, TypeError, ValueError):
            continue
        if (definition.get("buildType") == BUILD_TYPE
                and any(isinstance(item, dict)
                        and item.get("digest", {}).get("sha256") == subject_sha
                        for item in subjects)
                and internal.get("expectedReleaseDigest") == digest
                and external.get("generation") == generation
                and "hardwareProfile" not in external and "children" not in external
                and any(isinstance(item, dict) and item.get("uri") == source_uri for item in dependencies)):
            return
    invalid("published image provenance does not bind its digest, source, and generation")


def _image_digest(image):
    match = IMAGE.fullmatch(image)
    if match is None:
        invalid("published image must be an exact registry.tilo.so/tilo/tilo digest")
    return match.group("digest")


def ensure_local_image(image, authfile):
    exists = subprocess.run(["podman", "image", "exists", image], check=False)
    if exists.returncode == 0:
        return
    command = ["podman", "pull"]
    if authfile:
        command += ["--authfile", authfile]
    command.append(image)
    result = subprocess.run(command, check=False, capture_output=True, text=True)
    if result.returncode:
        invalid(f"cannot pull the published image: {result.stderr.strip()[-400:]}")


def extract_bundle(image, arch, scratch):
    ensure_local_image(image, os.environ.get("REGISTRY_AUTH_FILE"))
    container = f"tilo-target-policy-{os.getpid()}-{arch}"
    subprocess.run(["podman", "rm", "-f", container], check=False,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    created = subprocess.run(["podman", "create", "--name", container, image],
                             check=False, capture_output=True, text=True)
    if created.returncode:
        invalid(f"cannot create an extraction container: {created.stderr.strip()[-400:]}")
    try:
        root = Path(scratch) / "bundle"
        root.mkdir()
        copied = subprocess.run(["podman", "cp", f"{container}:/boot/tilo-target", str(root)],
                                check=False, capture_output=True, text=True)
        if copied.returncode:
            invalid("published image has no /boot/tilo-target bundle")
        if not (root / TARGET_UKI_FILE).is_file() and (root / "tilo-target").is_dir():
            root = root / "tilo-target"
        generation_file = Path(scratch) / "generation"
        copied = subprocess.run(["podman", "cp", f"{container}:/usr/lib/tilo/generation", str(generation_file)],
                                check=False, capture_output=True, text=True)
        if copied.returncode:
            invalid("published image has no embedded generation")
        generation = generation_file.read_text(encoding="ascii", errors="strict").strip()
        if not re.fullmatch(r"[1-9][0-9]*", generation):
            invalid("published image has an invalid embedded generation")
        return root, int(generation)
    finally:
        subprocess.run(["podman", "rm", "-f", container], check=False,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def read_bundle(image, arch, expected_source, authority_path, *, diagnostic=False, scratch_root=None):
    if not diagnostic:
        image_digest = _image_digest(image)
        config = inspect_image_config(image, os.environ.get("REGISTRY_AUTH_FILE"))
        check_image_config(config, arch, expected_source)
        verify_image_signature(image, image_digest)
    scratch_root = scratch_root or os.environ.get("TILO_INSTALLER_SCRATCH") or os.environ.get("TMPDIR") or "/var/tmp"
    with tempfile.TemporaryDirectory(prefix="tilo-target-policy-", dir=scratch_root) as scratch:
        root, embedded_generation = extract_bundle(image, arch, scratch)
        if diagnostic:
            files = {}
            for path in (root / TARGET_UKI_FILE,
                         root / PREDICTIONS_DIR / f"generation-{embedded_generation}.json",
                         root / PREDICTIONS_DIR / f"generation-{embedded_generation}.json.uki.sig"):
                files[str(path.relative_to(root))] = bounded_bytes(
                    path, MAX_UKI if path.name == TARGET_UKI_FILE else MAX_PREDICTION, "diagnostic bundle entry")
            if set(files) != expected_names(embedded_generation):
                invalid("published image target bundle does not match its embedded generation")
            uki = files[TARGET_UKI_FILE]
            if pe_machine(uki) != ARCHITECTURES[arch]:
                invalid(f"target UKI is not a {arch} PE image")
            return {
                "generation": embedded_generation,
                "target_uki_sha256": sha256(uki),
                "files": files,
            }
        verify_image_provenance(image, expected_source, embedded_generation)
        result = verify_bundle_directory(root, authority_path, generation=embedded_generation, arch=arch)
        files = {
            TARGET_UKI_FILE: (root / TARGET_UKI_FILE).read_bytes(),
            f"{PREDICTIONS_DIR}/generation-{embedded_generation}.json":
                (root / PREDICTIONS_DIR / f"generation-{embedded_generation}.json").read_bytes(),
            f"{PREDICTIONS_DIR}/generation-{embedded_generation}.json.uki.sig":
                (root / PREDICTIONS_DIR / f"generation-{embedded_generation}.json.uki.sig").read_bytes(),
        }
        result["files"] = files
        return result


def export(args):
    if not RELEASE_ID.fullmatch(args.release_id):
        invalid("release id must be imager-vMAJOR.MINOR.PATCH")
    if not OWNER.fullmatch(args.owner):
        invalid("package owner must be a Forgejo user or organisation name")
    if not SOURCE_REVISION.fullmatch(args.expected_source):
        invalid("expected source must be a full lowercase commit")
    if args.diagnostic:
        result = read_bundle(args.image, args.arch, args.expected_source,
                             args.authority, diagnostic=True)
        output = Path(args.output)
        output.mkdir(parents=True, exist_ok=True)
        archive = output / f"{PACKAGE}-{args.arch}.diagnostic.tar"
        write_archive(archive, result["files"])
        for leftover in output.glob(f"{PACKAGE}-{args.arch}.tar"):
            leftover.unlink()
        for leftover in output.glob(f"target-policy-catalog-{args.arch}.json"):
            leftover.unlink()
        print(archive)
        return
    result = read_bundle(args.image, args.arch, args.expected_source, args.authority)
    output = Path(args.output)
    output.mkdir(parents=True, exist_ok=True)
    for leftover in output.glob(f"{PACKAGE}-{args.arch}.diagnostic.tar"):
        leftover.unlink()
    archive = output / f"{PACKAGE}-{args.arch}.tar"
    write_archive(archive, result["files"])
    fragment = fragment_for(args.arch, args.release_id, args.owner,
                            result["generation"], result["target_uki_sha256"], archive)
    fragment_path = output / f"target-policy-catalog-{args.arch}.json"
    temporary = fragment_path.with_name(f".{fragment_path.name}.tmp")
    temporary.write_text(json.dumps(fragment, sort_keys=True, separators=(",", ":")) + "\n", encoding="utf-8")
    os.replace(temporary, fragment_path)
    print(fragment_path)
    print(archive)


def verify(args):
    bundle = Path(args.bundle)
    fragments = sorted(bundle.glob("target-policy-catalog-*.json"))
    if len(fragments) != 1:
        invalid("bundle directory must contain exactly one target policy catalog fragment")
    fragment = json.loads(fragments[0].read_text(encoding="utf-8"))
    validate_fragment(fragment, args.release_id, args.owner)
    archive = bundle / fragment["url"].rsplit("/", 1)[-1]
    if archive.is_symlink() or not archive.is_file():
        invalid("target policy archive named by the fragment is missing")
    if archive.stat().st_size != fragment["archive_size"] or sha256_file(archive) != fragment["archive_sha256"]:
        invalid("target policy archive bytes do not match the fragment")
    result, _ = read_archive(archive, args.authority, arch=fragment["arch"])
    if (result["generation"], result["target_uki_sha256"]) != (
            fragment["generation"], fragment["target_uki_sha256"]):
        invalid("target policy archive does not match the fragment's generation or UKI digest")
    print(f"verified target policy {fragment['arch']} generation {fragment['generation']}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    export_parser = commands.add_parser("export", help="export a qualified archive and catalog fragment")
    export_parser.add_argument("--image", required=True)
    export_parser.add_argument("--arch", required=True, choices=sorted(ARCHITECTURES))
    export_parser.add_argument("--release-id", required=True)
    export_parser.add_argument("--expected-source", required=True)
    export_parser.add_argument("--output", required=True, type=Path)
    export_parser.add_argument("--owner", default=os.environ.get("TILO_TARGET_POLICY_OWNER", DEFAULT_OWNER))
    export_parser.add_argument("--authority", type=Path, default=DEFAULT_AUTHORITY)
    export_parser.add_argument("--diagnostic", action="store_true",
                               help="export diagnostic bytes only; never write a catalog fragment")
    export_parser.set_defaults(handler=export)
    verify_parser = commands.add_parser("verify", help="verify an exported bundle and fragment")
    verify_parser.add_argument("--bundle", required=True, type=Path)
    verify_parser.add_argument("--release-id", required=True)
    verify_parser.add_argument("--owner", default=os.environ.get("TILO_TARGET_POLICY_OWNER", DEFAULT_OWNER))
    verify_parser.add_argument("--authority", type=Path, default=DEFAULT_AUTHORITY)
    verify_parser.set_defaults(handler=verify)
    args = parser.parse_args()
    try:
        args.handler(args)
    except (OSError, ValueError, TypeError, KeyError, AttributeError) as error:
        parser.exit(1, f"target policy: {error}\n")


if __name__ == "__main__":
    main()
