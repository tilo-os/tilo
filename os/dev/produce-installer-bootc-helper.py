#!/usr/bin/env python3
"""Prepare and package the pinned native installer-only bootc helper."""

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import platform
import re
import shutil
import subprocess


HERE = Path(__file__).resolve().parent
PIN_PATH = HERE.parent / "external-assets" / "bootc" / "source.json"
spec = importlib.util.spec_from_file_location(
    "installer_bootc_helper", HERE / "installer_bootc_helper.py")
contract = importlib.util.module_from_spec(spec)
spec.loader.exec_module(contract)


def digest(path):
    return contract.sha256_file(Path(path))


def run(*argv, cwd=None):
    return subprocess.run(argv, cwd=cwd, check=True, text=True,
                          stdout=subprocess.PIPE).stdout.strip()


def pin():
    document = json.loads(PIN_PATH.read_text())
    required = {"schema", "purpose", "upstream", "source_tag", "source_commit",
                "source_cargo_lock_sha256", "patch", "patch_sha256",
                "rust_toolchain", "build_image", "build_command"}
    if set(document) != required or document["schema"] != 1:
        raise ValueError("installer bootc source pin has unexpected fields")
    expected = {
        "source_tag": contract.SOURCE_TAG,
        "source_commit": contract.SOURCE_COMMIT,
        "source_cargo_lock_sha256": contract.SOURCE_CARGO_LOCK_SHA256,
        "patch_sha256": contract.PATCH_SHA256,
        "rust_toolchain": contract.RUST_TOOLCHAIN,
        "build_image": contract.BUILD_IMAGE,
        "build_command": contract.BUILD_COMMAND,
    }
    if any(document[key] != value for key, value in expected.items()):
        raise ValueError("installer bootc source pin differs from runtime contract")
    patch = PIN_PATH.parent / document["patch"]
    if digest(patch) != document["patch_sha256"]:
        raise ValueError("installer bootc patch digest differs")
    return document, patch


def native_architecture():
    machine = platform.machine().lower()
    return {"arm64": "aarch64", "aarch64": "aarch64",
            "amd64": "x86_64", "x86_64": "x86_64"}.get(machine, machine)


def tool_versions(document):
    rustc = run("rustc", "--version")
    cargo = run("cargo", "--version")
    if not re.match(rf"^rustc {re.escape(document['rust_toolchain'])}(?: |$)", rustc):
        raise ValueError("Rust compiler differs from the pinned helper toolchain")
    return rustc, cargo


def prepare(args):
    document, patch = pin()
    if args.architecture != native_architecture():
        raise ValueError("installer bootc helper requires a native build host")
    rustc, cargo = tool_versions(document)
    source = args.source.resolve()
    source.mkdir(parents=True, exist_ok=False)
    run("git", "init", "--quiet", str(source))
    run("git", "-C", str(source), "remote", "add", "origin", document["upstream"])
    run("git", "-C", str(source), "fetch", "--quiet", "--depth=1", "origin",
        document["source_commit"])
    run("git", "-C", str(source), "checkout", "--quiet", "--detach", "FETCH_HEAD")
    if run("git", "-C", str(source), "rev-parse", "HEAD") != document["source_commit"]:
        raise ValueError("fetched installer bootc commit differs")
    tag_ref = f"refs/tags/{document['source_tag']}"
    run("git", "-C", str(source), "fetch", "--quiet", "--depth=1", "origin",
        f"{tag_ref}:{tag_ref}")
    if run("git", "-C", str(source), "rev-parse", f"{tag_ref}^{{commit}}") != document["source_commit"]:
        raise ValueError("installer bootc source tag does not select the pinned commit")
    if digest(source / "Cargo.lock") != document["source_cargo_lock_sha256"]:
        raise ValueError("installer bootc Cargo.lock digest differs")
    run("git", "-C", str(source), "apply", "--check", str(patch))
    run("git", "-C", str(source), "apply", str(patch))
    material = {
        "source_commit": document["source_commit"],
        "patch_sha256": document["patch_sha256"],
        "source_cargo_lock_sha256": document["source_cargo_lock_sha256"],
        "rustc_version": rustc, "cargo_version": cargo,
        "architecture": args.architecture, "build_image": document["build_image"],
    }
    cache_key = "installer-bootc-v1-" + hashlib.sha256(
        json.dumps(material, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    args.state.write_text(json.dumps(material, indent=2, sort_keys=True) + "\n")
    if args.github_output:
        with args.github_output.open("a") as output:
            output.write(f"cache_key={cache_key}\n")
    print(cache_key)


def package(args):
    document, _ = pin()
    state = json.loads(args.state.read_text())
    if state["architecture"] != args.architecture:
        raise ValueError("installer bootc prepared architecture differs")
    rustc, cargo = tool_versions(document)
    if state["rustc_version"] != rustc or state["cargo_version"] != cargo:
        raise ValueError("installer bootc toolchain changed after cache selection")
    built = args.target / "release" / "bootc"
    if not built.is_file():
        raise ValueError("installer bootc build output is missing")
    args.output.mkdir(parents=True, exist_ok=False)
    binary = args.output / "bootc"
    shutil.copyfile(built, binary)
    binary.chmod(0o755)
    identity = contract.binary_identity(binary, args.architecture)
    version = run(str(binary), "--version")
    if version != f"bootc {contract.BOOTC_VERSION}":
        raise ValueError("installer bootc version differs")
    receipt = {
        "schema": 1, "purpose": contract.PURPOSE,
        "architecture": args.architecture, "bootc_version": contract.BOOTC_VERSION,
        "source_tag": contract.SOURCE_TAG, "source_commit": contract.SOURCE_COMMIT,
        "source_cargo_lock_sha256": contract.SOURCE_CARGO_LOCK_SHA256,
        "patch_sha256": contract.PATCH_SHA256,
        "binary_sha256": identity["sha256"], "binary_build_id": identity["build_id"],
        "rustc_version": rustc, "cargo_version": cargo,
        "rust_toolchain": contract.RUST_TOOLCHAIN, "build_image": contract.BUILD_IMAGE,
        "build_target": contract.TARGETS[args.architecture],
        "build_command": contract.BUILD_COMMAND,
    }
    (args.output / "receipt.json").write_text(
        json.dumps(receipt, indent=2, sort_keys=True) + "\n")
    contract.verify_artifact(args.output, identity["sha256"], args.architecture)
    print(identity["sha256"])


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    prepare_parser = sub.add_parser("prepare")
    prepare_parser.add_argument("--architecture", choices=sorted(contract.MACHINES), required=True)
    prepare_parser.add_argument("--source", type=Path, required=True)
    prepare_parser.add_argument("--state", type=Path, required=True)
    prepare_parser.add_argument("--github-output", type=Path)
    prepare_parser.set_defaults(function=prepare)
    package_parser = sub.add_parser("package")
    package_parser.add_argument("--architecture", choices=sorted(contract.MACHINES), required=True)
    package_parser.add_argument("--target", type=Path, required=True)
    package_parser.add_argument("--state", type=Path, required=True)
    package_parser.add_argument("--output", type=Path, required=True)
    package_parser.set_defaults(function=package)
    args = parser.parse_args()
    args.function(args)


if __name__ == "__main__":
    main()
