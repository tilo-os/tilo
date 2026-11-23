#!/usr/bin/env python3
"""Resume a paired t/WHPX Windows direct install without rebuilding OS media.

`prepare` takes the two media t's `medium/whpx-aarch64` check wrote
(`t outputs medium/whpx-aarch64 --to DIR`), verifies their signed identities and
the OS releases they install (signature and SLSA provenance), runs the generic
full-boot and Lenovo install-only WHPX cycles, then creates the source receipt.
Physical writes require separate `copy` and `bootnext` invocations.
"""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys


HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
SHA = re.compile(r"[0-9a-f]{64}\Z")
REV = re.compile(r"[0-9a-f]{40}\Z")
PUBLIC = "registry.tilo.so/tilo/tilo@sha256:"
BOARD = "code.tilo.so/tilo/tilo@sha256:"
PROFILES = ("generic", "lenovo-83ed")
MIN_FREE = 20 * 1024 * 1024 * 1024


def require(ok, message):
    if not ok:
        raise ValueError(message)


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(4 * 1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def regular(path):
    require(path.is_file() and not path.is_symlink(), f"missing regular file: {path}")
    return path


def free_space(path, minimum=MIN_FREE):
    available = shutil.disk_usage(path).free
    require(available >= minimum,
            f"only {available // (1024 ** 3)} GiB free at {path}; "
            f"need at least {minimum // (1024 ** 3)} GiB before this phase")


def read_json(path):
    return json.loads(regular(path).read_text(encoding="utf-8"))


def atomic_json(path, value):
    temporary = path.with_name(path.name + ".tmp")
    require(not temporary.exists(), f"stale temporary file: {temporary}")
    with temporary.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, indent=2, sort_keys=True)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    temporary.replace(path)


def command(argv, *, stdin=None, capture=False):
    result = subprocess.run(argv, input=stdin, text=stdin is not None,
                            capture_output=capture, check=False)
    require(result.returncode == 0, f"command failed ({result.returncode}): {argv[0]} {argv[1]}")
    return result.stdout if capture else None


def pinned_config(path):
    data = read_json(path)
    required = {"schema", "source_revision", "medium_outputs", "generic_image",
                "lenovo_image", "realized_plan",
                "source_raw", "generic_index_manifest", "generic_child_manifest",
                "lenovo_child_manifest", "verify_key", "cosign_command_json",
                "cosign_key_arg", "username", "tilo_vm_exe"}
    require(type(data) is dict and set(data) == required,
            "orchestrator input must contain exactly the documented fields")
    require(data["schema"] == "tilo.windows-direct-install.v2", "unknown input schema")
    require(type(data["source_revision"]) is str and REV.fullmatch(data["source_revision"]),
            "source_revision must be a full lowercase commit")
    require(type(data["medium_outputs"]) is str and Path(data["medium_outputs"]).is_absolute(),
            "medium_outputs must be an absolute path to t's WHPX check outputs")
    for key, prefix in (("generic_image", PUBLIC), ("lenovo_image", BOARD)):
        require(type(data[key]) is str and data[key].startswith(prefix)
                and SHA.fullmatch(data[key][len(prefix):]), f"invalid pinned {key}")
    require(type(data["username"]) is str
            and re.fullmatch(r"[a-z_][a-z0-9_-]{0,31}", data["username"]),
            "invalid account name")
    for key in ("realized_plan", "source_raw", "generic_index_manifest",
                "generic_child_manifest", "lenovo_child_manifest", "verify_key",
                "cosign_command_json", "tilo_vm_exe"):
        require(type(data[key]) is str and Path(data[key]).is_absolute(),
                f"{key} must be an absolute path")
        data[key] = Path(data[key]).resolve(strict=False)
    require(type(data["cosign_key_arg"]) is str and data["cosign_key_arg"].startswith("/mnt/"),
            "cosign_key_arg must be the WSL path of verify_key")
    for key in ("realized_plan", "generic_index_manifest", "generic_child_manifest",
                "lenovo_child_manifest", "verify_key", "cosign_command_json", "tilo_vm_exe"):
        regular(data[key])
    require(data["source_raw"] != data["realized_plan"], "source_raw aliases the plan")
    return data


def config_pin(data):
    encoded = json.dumps({key: str(value) if isinstance(value, Path) else value
                          for key, value in data.items()}, sort_keys=True).encode()
    value = hashlib.sha256(encoded)
    for key in ("realized_plan", "generic_index_manifest", "generic_child_manifest",
                "lenovo_child_manifest", "verify_key", "cosign_command_json", "tilo_vm_exe"):
        value.update(key.encode() + b"\0" + digest(data[key]).encode())
    return value.hexdigest()


class Progress:
    def __init__(self, work, pin):
        require(work.is_dir() and not work.is_symlink(), "work directory must already exist")
        self.path = work / "windows-direct-progress.json"
        self.data = read_json(self.path) if self.path.exists() else {
            "schema": "tilo.windows-direct-progress.v1", "config_sha256": pin, "steps": {}}
        require(self.data.get("schema") == "tilo.windows-direct-progress.v1"
                and self.data.get("config_sha256") == pin
                and type(self.data.get("steps")) is dict,
                "progress belongs to different inputs; choose another work directory")
        if not self.path.exists():
            atomic_json(self.path, self.data)

    def done(self, name, files):
        stored = self.data["steps"].get(name)
        if stored is None:
            return False
        require(type(stored) is dict and set(stored) == {str(path) for path in files},
                f"checkpoint {name} has unexpected outputs")
        require(all(regular(path) and digest(path) == stored[str(path)] for path in files),
                f"checkpoint {name} output changed")
        return True

    def mark(self, name, files):
        self.data["steps"][name] = {str(path): digest(regular(path)) for path in files}
        atomic_json_replace(self.path, self.data)


def atomic_json_replace(path, value):
    temporary = path.with_name(path.name + ".next")
    require(not temporary.exists(), f"stale checkpoint update: {temporary}")
    atomic_json(temporary, value)
    temporary.replace(path)


def copy_output(source, destination):
    require(source.is_file() and not source.is_symlink(), f"missing t check output: {source}")
    if destination.exists():
        regular(destination)
        require(digest(destination) == digest(source), f"retained copy differs from t's output: {source}")
        return
    partial = destination.with_name(destination.name + ".part")
    if partial.exists():
        regular(partial).unlink()
    with source.open("rb") as stream, partial.open("xb") as sink:
        shutil.copyfileobj(stream, sink, 4 * 1024 * 1024)
    partial.replace(destination)


def decompress_zst(source, output, archive_sha, image_sha):
    """Use only the qcow2 space; never materialize a second uncompressed copy."""
    require(SHA.fullmatch(archive_sha) and SHA.fullmatch(image_sha), "invalid medium hashes")
    regular(source)
    partial = output.with_name(output.name + ".part")
    require(not output.exists(), f"uncheckpointed medium already exists: {output}")
    if partial.exists():
        regular(partial).unlink()
    free_space(output.parent, max(MIN_FREE, source.stat().st_size * 4))
    compressed_sha = hashlib.sha256()
    with source.open("rb") as compressed, partial.open("xb") as sink:
        process = subprocess.Popen(["zstd", "--decompress", "--stdout"],
                                   stdin=subprocess.PIPE, stdout=sink)
        try:
            for block in iter(lambda: compressed.read(4 * 1024 * 1024), b""):
                compressed_sha.update(block)
                process.stdin.write(block)
            process.stdin.close()
            require(process.wait() == 0, "zstd decompression failed")
        except BaseException:
            process.kill()
            process.wait()
            raise
    require(compressed_sha.hexdigest() == archive_sha and digest(partial) == image_sha,
            "WHPX medium archive or decompressed qcow2 hash differs")
    partial.replace(output)


def proof_path(work, profile):
    return work / profile / "ci" / "guest-install-proof.json"


def identity_path(work, profile):
    return work / profile / "ci" / f"tilo-installer-{profile}-aarch64.whpx-source.json"


def medium_path(work, profile):
    return work / profile / f"tilo-installer-{profile}-aarch64.qcow2"


def verify_identity(data, work, profile):
    spec = importlib.util.spec_from_file_location("windows_direct_provenance",
                                                  HERE / "windows-direct-provenance.py")
    provenance = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(provenance)
    identity = provenance.signed_medium_identity(
        identity_path(work, profile), proof_path(work, profile), data["verify_key"],
        data["cosign_command_json"], data["cosign_key_arg"])
    expected = data["lenovo_image"] if profile == "lenovo-83ed" else None
    require(identity["build_revision"] == data["source_revision"]
            and identity.get("os_source_revision", identity["build_revision"])
            == data["source_revision"]
            and identity["live_base_image"] == data["generic_image"]
            and (expected is None or identity["target_image"] == expected),
            f"signed {profile} medium differs from the pinned source or images")
    return identity


def verify_ci_images(data, identities):
    spec = importlib.util.spec_from_file_location("windows_direct_provenance",
                                                  HERE / "windows-direct-provenance.py")
    provenance = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(provenance)
    _, generic_child, children, kind = provenance.signed_ci_child(
        data["generic_image"], data["generic_index_manifest"], data["generic_child_manifest"])
    require(kind == "generic-index"
            and identities["generic"]["target_image"] == PUBLIC + generic_child,
            "generic signed medium target is not index ARM64 child")
    _, board_child, _, board_kind = provenance.signed_ci_child(
        data["lenovo_image"], data["lenovo_child_manifest"], data["lenovo_child_manifest"])
    require(board_kind == "lenovo-83ed" and data["lenovo_image"] == BOARD + board_child,
            "Lenovo manifest differs from pinned image")
    verified = {}
    for image, descriptors, image_kind in (
            (data["generic_image"], children, "generic-index"),
            (PUBLIC + generic_child, [], "generic-child"),
            (data["lenovo_image"], [], "lenovo-83ed")):
        evidence = provenance.verify_ci_image(
            image, data["verify_key"], descriptors, image_kind,
            data["cosign_command_json"], data["cosign_key_arg"])
        require(evidence["source_revision"] == data["source_revision"],
                f"signed CI image source differs: {image}")
        verified[image_kind] = evidence
    require(all(verified["generic-index"][field] == verified["generic-child"][field]
                for field in ("generation", "invocation_id")),
            "generic child provenance differs from signed release index")


def fetch(data, work, progress):
    outputs = Path(data["medium_outputs"])
    require(outputs.is_absolute() and outputs.is_dir(), "medium_outputs is not a directory")
    whpx = outputs / "whpx"
    cycle = outputs / "cycle" / "guest-install-proof.json"
    require(whpx.is_dir(), "medium_outputs holds no whpx/ directory")
    for profile in PROFILES:
        directory = work / profile
        ci = directory / "ci"
        ci.mkdir(parents=True, exist_ok=True)
        medium = medium_path(work, profile)
        stem = f"tilo-installer-{profile}-aarch64"
        if progress.done("medium-" + profile, [medium]):
            identity = verify_identity(data, work, profile)
            require(progress.data["steps"]["medium-" + profile][str(medium)]
                    == identity["qcow2_sha256"],
                    "retained medium checkpoint differs from the signed identity")
            continue
        free_space(directory)
        for name in (stem + ".qcow2.zst", stem + ".qcow2.zst.sha256", stem + ".qcow2.sha256",
                     stem + ".whpx-source.json", stem + ".whpx-source.json.sig",
                     stem + ".whpx-source.json.bundle"):
            copy_output(whpx / name, ci / name)
        copy_output(cycle, ci / "guest-install-proof.json")
        identity = verify_identity(data, work, profile)
        require((ci / (stem + ".qcow2.zst.sha256")).read_text().split()[0]
                == identity["archive_sha256"]
                and (ci / (stem + ".qcow2.sha256")).read_text().split()[0]
                == identity["qcow2_sha256"], f"{profile} medium archive hash differs")
        decompress_zst(ci / (stem + ".qcow2.zst"), medium,
                       identity["archive_sha256"], identity["qcow2_sha256"])
        progress.mark("medium-" + profile, [medium])
    return {profile: verify_identity(data, work, profile) for profile in PROFILES}


def backend(data, script, *args):
    command([sys.executable, str(HERE / script), *map(str, args)])


def proof_checked(work, profile):
    path = work / profile / "scenario" / "vm-install-proof-checked.json"
    require(read_json(path).get("schema") == "tilo.windows-vm-install-proof-check.v2",
            f"{profile} WHPX proof is not v2")
    return path


def scenario_argv(data, work, profile, target, out):
    argv = [str(data["tilo_vm_exe"]), "scenario", "install", "--medium",
            str(medium_path(work, profile)),
            "--layout", "guided-alongside", "--target-disk", str(target),
            "--filesystem", "ext4", "--encrypt", "passphrase",
            "--graphics", "software", "--out", str(out), "--keep"]
    require(profile in PROFILES, "unknown WHPX profile")
    if profile == "lenovo-83ed":
        argv.append("--skip-target-boot")
    argv.extend(["--username", data["username"], "--credentials-stdin"])
    retained = out / "install.json"
    if retained.exists():
        steps = read_json(retained).get("steps", [])
        require(type(steps) is list and steps and steps[-1] == "power-off",
                "retained scenario install did not complete; inspect it before retrying")
        argv.append("--resume")
    return argv


def read_credentials():
    credentials = sys.stdin.read()
    require(0 < len(credentials) <= 4096, "credential input is empty or oversized")
    secret = json.loads(credentials)
    require(type(secret) is dict and set(secret) == {"account_password", "disk_passphrase"}
            and all(type(value) is str for value in secret.values())
            and secret["account_password"] != secret["disk_passphrase"],
            "stdin must contain distinct account_password and disk_passphrase")
    return credentials


def verify_template_at(source):
    output = command([sys.executable, str(HERE / "prepare-windows-direct-source.py"),
                      "verify-template", "--manifest", str(source) + ".manifest.json",
                      "--source-raw", str(source)], capture=True)
    return json.loads(output)


def verify_template(data):
    verify_template_at(data["source_raw"])


def template_matches_plan(manifest, plan, normalized_preserved):
    expected_targets = [{key: row[key] for key in
                         ("role", "type_guid", "offset_bytes", "size_bytes")}
                        for row in plan["target_partitions"]]
    return (manifest.get("disk_size_bytes") == plan["windows_disk"]["size_bytes"]
            and manifest.get("preserved_partitions") == normalized_preserved
            and manifest.get("target_partitions") == expected_targets)


def matching_empty_template(data, source):
    """Adopt only the exact untouched sparse template for this realized plan."""
    manifest = read_json(Path(str(source) + ".manifest.json"))
    spec = importlib.util.spec_from_file_location("windows_direct_copy",
                                                  HERE / "copy-windows-direct-final-install.py")
    direct = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(direct)
    plan = direct.load_plan(data["realized_plan"])
    require(template_matches_plan(manifest, plan, direct.normalized(plan["preserve_partitions"])),
            "existing sparse source template differs from realized plan")
    checked = verify_template_at(source)
    require(checked.get("final_partition_count") == 0
            and checked.get("preserved_gpt_verified") is True
            and checked.get("preserved_edge_samples_verified") is True,
            "existing sparse source is not an empty verified template")
    if sys.platform == "win32":
        spec = importlib.util.spec_from_file_location("windows_direct_source",
                                                      HERE / "prepare-windows-direct-source.py")
        source_module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(source_module)
        with source.open("rb", buffering=0) as stream:
            source_module.verify_sparse_windows(stream, source.stat().st_size)


def common_evidence(data, work):
    return ["--plan", data["realized_plan"], "--source-raw", data["source_raw"],
            "--ci-image", data["lenovo_image"], "--ci-index-manifest",
            data["lenovo_child_manifest"], "--ci-child-manifest", data["lenovo_child_manifest"],
            "--vm-proof-checked", proof_checked(work, "lenovo-83ed"),
            "--verify-key", data["verify_key"], "--cosign-command-json",
            data["cosign_command_json"], "--cosign-key-arg", data["cosign_key_arg"],
            "--board-medium-identity", identity_path(work, "lenovo-83ed"),
            "--board-ci-guest-proof", proof_path(work, "lenovo-83ed"),
            "--generic-ci-image", data["generic_image"],
            "--generic-index-manifest", data["generic_index_manifest"],
            "--generic-child-manifest", data["generic_child_manifest"],
            "--generic-vm-proof-checked", proof_checked(work, "generic"),
            "--generic-medium-identity", identity_path(work, "generic"),
            "--generic-ci-guest-proof", proof_path(work, "generic")]


def prepare(data, work, progress):
    identities = fetch(data, work, progress)
    require(identities["generic"]["build_revision"]
            == identities["lenovo-83ed"]["build_revision"]
            and identities["generic"]["target_image"].startswith(PUBLIC),
            "signed media are not one paired build")
    verify_ci_images(data, identities)
    generic_source = work / "generic" / "generic-source.raw"
    require(data["source_raw"] != generic_source,
            "physical source raw must differ from disposable generic target")
    generic_template = Path(str(generic_source) + ".manifest.json")
    generic_out = work / "generic" / "scenario"
    generic_proof = generic_out / "vm-install-proof-checked.json"
    credentials = None
    if not progress.done("generic-whpx", [generic_proof]):
        if not progress.done("generic-template", [generic_template]):
            require(not generic_source.exists() and not generic_template.exists(),
                    "generic source already exists without this checkpoint")
            backend(data, "prepare-windows-direct-source.py", "template", "--plan",
                    data["realized_plan"], "--output", generic_source)
            matching_empty_template(data, generic_source)
            progress.mark("generic-template", [generic_template])
        verify_template_at(generic_source)
        require(not generic_out.exists(), "uncheckpointed generic scenario directory exists")
        free_space(work / "generic")
        credentials = read_credentials()
        command(scenario_argv(data, work, "generic", generic_source, generic_out),
                stdin=credentials)
        verify_template_at(generic_source)
        proof_checked(work, "generic")
        require(read_json(generic_proof).get("first_boot_verified") is True,
                "generic WHPX first boot was not verified")
        progress.mark("generic-whpx", [generic_proof])
    # The generic VM target is only evidence generation, never physical-copy
    # input. Its checked proof persists; release its installed blocks now.
    if generic_source.exists():
        require(generic_source.parent == work / "generic" and not generic_source.is_symlink(),
                "unexpected generic source path")
        generic_source.unlink()
    if generic_template.exists():
        regular(generic_template).unlink()

    source = data["source_raw"]
    template = Path(str(source) + ".manifest.json")
    if not progress.done("template", [template]):
        if source.exists() or template.exists():
            require(source.exists() and template.exists(),
                    "source raw and template manifest must either both exist or both be new")
            matching_empty_template(data, source)
        else:
            backend(data, "prepare-windows-direct-source.py", "template", "--plan",
                    data["realized_plan"], "--output", source)
            matching_empty_template(data, source)
        progress.mark("template", [template])
    verify_template(data)

    board_out = work / "lenovo-83ed" / "scenario"
    board_proof = board_out / "vm-install-proof-checked.json"
    if not progress.done("lenovo-whpx", [board_proof]):
        require(not board_out.exists(), "uncheckpointed Lenovo scenario directory exists")
        free_space(source.parent)
        if credentials is None:
            credentials = read_credentials()
        command(scenario_argv(data, work, "lenovo-83ed", source, board_out), stdin=credentials)
        verify_template(data)
        proof_checked(work, "lenovo-83ed")
        progress.mark("lenovo-whpx", [board_proof])
    verify_template(data)

    if not progress.done("finalized", [template]):
        backend(data, "prepare-windows-direct-source.py", "finalize", "--plan",
                data["realized_plan"], "--source-raw", source)
        progress.mark("finalized", [template])
    receipt = work / "source-receipt.json"
    if not progress.done("receipt", [receipt]):
        require(not receipt.exists(), "uncheckpointed source receipt exists")
        backend(data, "prepare-windows-direct-source.py", "receipt", *common_evidence(data, work),
                "--output", receipt)
        progress.mark("receipt", [receipt])
    print(f"CI media, paired WHPX proofs and source receipt ready: {receipt}")


def copy(data, work, progress, disk_id):
    require(progress.done("receipt", [work / "source-receipt.json"]),
            "prepare must finish before physical copy")
    readback = work / "final-readback.json"
    if not progress.done("physical-copy", [readback]):
        require(not readback.exists(), "uncheckpointed final readback exists")
        backend(data, "copy-windows-direct-final-install.py", "execute",
                *common_evidence(data, work), "--source-receipt", work / "source-receipt.json",
                "--confirm-disk-id", disk_id, "--output", readback)
        progress.mark("physical-copy", [readback])
    print(f"Physical copy readback: {readback}")


def bootnext(work, progress, imager):
    readback = work / "final-readback.json"
    require(progress.done("physical-copy", [readback]), "physical copy must finish before BootNext")
    receipt = work / "bootnext-receipt.json"
    attempt = work / "bootnext-attempt.json"
    require(not progress.data["steps"].get("bootnext")
            and not receipt.exists() and not attempt.exists(),
            "BootNext already attempted; inspect firmware before another invocation")
    imager_path = Path(imager)
    require(imager_path.is_absolute(), "Imager must be an absolute executable path")
    imager_path = regular(imager_path.resolve(strict=True))
    atomic_json(attempt, {"schema": "tilo.windows-direct-bootnext-attempt.v1",
                          "readback_sha256": digest(readback),
                          "imager_sha256": digest(imager_path)})
    result = command([str(imager_path), "--tilo-final-bootnext", "--readback", str(readback)],
                     capture=True)
    parsed = json.loads(result)
    require(parsed.get("schema") == "tilo.windows-direct-armed-boot.v1",
            "Imager returned an unexpected BootNext receipt")
    atomic_json(receipt, parsed)
    progress.mark("bootnext", [receipt])
    print(f"BootNext armed; no reboot performed: {receipt}")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--work-dir", type=Path, required=True)
    commands = parser.add_subparsers(dest="phase", required=True)
    commands.add_parser("prepare")
    physical = commands.add_parser("copy")
    physical.add_argument("--confirm-disk-id", required=True)
    firmware = commands.add_parser("bootnext")
    firmware.add_argument("--imager", required=True)
    args = parser.parse_args(argv)
    try:
        require(sys.platform == "win32", "Windows direct install requires native Windows Python")
        data = pinned_config(args.input)
        work = args.work_dir.resolve(strict=True)
        progress = Progress(work, config_pin(data))
        if args.phase == "prepare":
            prepare(data, work, progress)
        elif args.phase == "copy":
            copy(data, work, progress, args.confirm_disk_id)
        else:
            bootnext(work, progress, args.imager)
        return 0
    except (OSError, ValueError, json.JSONDecodeError, subprocess.SubprocessError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
