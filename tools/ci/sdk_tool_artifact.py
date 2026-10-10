"""Bind a same-run native SDK tool to its tested source and immutable builder."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess

SCHEMA = "tilo.ci.sdk-tool/v1"
FIELDS = {"schema", "source_commit", "source_tree", "cargo_lock_sha256", "builder_image",
          "run_id", "run_attempt", "target", "toolchain", "cargo_arguments", "environment",
          "product", "resources"}
ARGUMENTS = ["build", "--locked", "-p", "tilo-app-cli", "--bin", "tilo-app"]
ENVIRONMENT = {"CARGO_INCREMENTAL": "0", "CARGO_PROFILE_DEV_DEBUG": "0"}


def sha(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as file:
        for chunk in iter(lambda: file.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def git(repo: Path, *args: str) -> str:
    return subprocess.run(["git", *args], cwd=repo, check=True, capture_output=True,
                          text=True).stdout.strip()


def identity(repo: Path, supplied: dict) -> dict:
    if any(os.environ.get(key) != value for key, value in ENVIRONMENT.items()):
        raise ValueError("SDK tool environment differs from the tested build")
    builder = supplied["builder_image"]
    if not re.fullmatch(r"registry\.tilo\.so/tilo/tilo-builder@sha256:[0-9a-f]{64}", builder):
        raise ValueError("SDK tool requires an immutable CI builder")
    if supplied["run_id"] < 1 or supplied["run_attempt"] < 1 or not supplied["toolchain"]:
        raise ValueError("SDK tool requires exact workflow run/attempt and toolchain")
    if git(repo, "status", "--porcelain", "--untracked-files=no"):
        raise ValueError("SDK tool requires clean tested source")
    source, tree = git(repo, "rev-parse", "HEAD"), git(repo, "rev-parse", "HEAD^{tree}")
    if not all(re.fullmatch(r"[0-9a-f]{40}", value) for value in (source, tree)):
        raise ValueError("SDK tool requires exact source and tree identities")
    return dict(schema=SCHEMA, source_commit=source, source_tree=tree,
                cargo_lock_sha256=sha(repo / "Cargo.lock"), **supplied,
                target="x86_64-unknown-linux-gnu", cargo_arguments=ARGUMENTS,
                environment=ENVIRONMENT)


def native_tool(path: Path) -> None:
    if path.is_symlink() or not path.is_file() or not 0 < path.stat().st_size <= 512 << 20:
        raise ValueError("missing or invalid bounded native SDK tool")
    with path.open("rb") as file:
        header = file.read(64)
    if len(header) < 64 or header[:6] != b"\x7fELF\x02\x01" or header[18:20] != b"\x3e\x00":
        raise ValueError("SDK tool is not a native x86_64 ELF")


def resources(cgroup: Path) -> dict:
    # A qualification must observe its envelope, rather than skip a missing metric.
    quota, period = (cgroup / "cpu.max").read_text().split()
    limit = int((cgroup / "memory.max").read_text())
    peak = int((cgroup / "memory.peak").read_text())
    if quota == "max" or int(quota) != 2 * int(period) or limit != 5 << 30:
        raise ValueError("qualification requires the unchanged 2-CPU/5-GiB quality envelope")
    if peak < 1 or peak * 100 >= limit * 85:
        raise ValueError("qualification producer approaches memory limit (85% cutoff)")
    return dict(cpu_quota=int(quota), cpu_period=int(period), memory_limit=limit,
                memory_peak=peak, memory_peak_percent=100 * peak / limit)


def create(repo: Path, directory: Path, expected: dict, cgroup: Path) -> str:
    for key, value in ENVIRONMENT.items():
        if os.environ.get(key) != value:
            raise ValueError("SDK tool build environment differs from both consumers")
    source = repo / "target/debug/tilo-app"
    native_tool(source)
    directory.mkdir(parents=True, exist_ok=False)
    product = directory / "tilo-app"
    shutil.copyfile(source, product)
    native_tool(product)
    if sha(source) != sha(product):
        raise ValueError("SDK tool changed during artifact delivery")
    receipt = dict(expected, product=dict(path="tilo-app", sha256=sha(product),
                                         bytes=product.stat().st_size), resources=resources(cgroup))
    manifest = directory / "manifest.json"
    manifest.write_text(json.dumps(receipt, sort_keys=True, separators=(",", ":")) + "\n")
    return sha(manifest)


def install(repo: Path, directory: Path, expected: dict, receipt_sha: str) -> None:
    manifest = directory / "manifest.json"
    if manifest.is_symlink() or manifest.stat().st_size > 64 << 10 \
            or not re.fullmatch(r"[0-9a-f]{64}", receipt_sha) or sha(manifest) != receipt_sha:
        raise ValueError("SDK tool receipt differs from authenticated producer output")
    receipt = json.loads(manifest.read_bytes())
    if set(receipt) != FIELDS or any(receipt.get(key) != value for key, value in expected.items()):
        raise ValueError("SDK tool source/builder/run/toolchain identity mismatch")
    record = receipt["product"]
    if set(record) != {"path", "sha256", "bytes"} or record["path"] != "tilo-app":
        raise ValueError("SDK tool artifact layout mismatch")
    product = directory / "tilo-app"
    native_tool(product)
    if sha(product) != record["sha256"] or product.stat().st_size != record["bytes"]:
        raise ValueError("SDK tool artifact bytes mismatch")
    destination = repo / "target/debug/tilo-app"
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.is_symlink():
        raise ValueError("SDK tool destination must not be a symlink")
    shutil.copyfile(product, destination)
    destination.chmod(0o755)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("create", "install"))
    parser.add_argument("--repo", required=True, type=Path)
    parser.add_argument("--directory", required=True, type=Path)
    parser.add_argument("--builder-image", required=True)
    parser.add_argument("--run-id", required=True, type=int)
    parser.add_argument("--run-attempt", required=True, type=int)
    parser.add_argument("--toolchain", required=True)
    parser.add_argument("--receipt-sha256", default="")
    parser.add_argument("--cgroup", type=Path, default=Path("/sys/fs/cgroup"))
    args = parser.parse_args()
    expected = identity(args.repo, dict(builder_image=args.builder_image, run_id=args.run_id,
                                       run_attempt=args.run_attempt, toolchain=args.toolchain))
    if args.mode == "create":
        print(create(args.repo, args.directory, expected, args.cgroup))
    else:
        install(args.repo, args.directory, expected, args.receipt_sha256)


if __name__ == "__main__":
    main()
