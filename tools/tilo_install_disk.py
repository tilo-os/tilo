#!/usr/bin/env python3
"""Create a disposable Tilo ARM disk from an immutable local OCI mirror."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import time
from typing import Any


SCHEMA = "tilo.vm-disk-install/v1"
DIGEST_REF = re.compile(r"^.+@sha256:([0-9a-f]{64})$")


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def immutable_digest(reference: str) -> str:
    match = DIGEST_REF.fullmatch(reference)
    if not match:
        raise ValueError(f"OCI reference must be digest-pinned: {reference}")
    return f"sha256:{match.group(1)}"


def validate(args: argparse.Namespace) -> dict[str, Any]:
    authoritative_digest = immutable_digest(args.image)
    mirror_digest = immutable_digest(args.mirror.removeprefix("docker://"))
    if authoritative_digest != mirror_digest:
        raise ValueError("authoritative image and local mirror have different digests")
    seed = args.firstboot_seed.resolve(strict=True)
    if seed.is_symlink() or not seed.is_file():
        raise ValueError("first-boot seed must be a regular, non-symlink file")
    if seed.stat().st_size > 1024:
        raise ValueError("first-boot seed exceeds the production 1024-byte limit")
    output = args.output.resolve()
    if output.exists():
        raise ValueError(f"output must not already exist: {output}")
    if not output.parent.is_dir():
        raise ValueError(f"output parent does not exist: {output.parent}")
    script = args.disk_image.resolve(strict=True)
    if not script.is_file():
        raise ValueError(f"disk-image tool is not a file: {script}")
    return {
        "authoritative_image": args.image,
        "local_mirror": args.mirror.removeprefix("docker://"),
        "image_digest": authoritative_digest,
        "firstboot_seed": {
            "path": str(seed),
            "bytes": seed.stat().st_size,
            "sha256": sha256(seed),
        },
        "output_directory": str(output),
        "disk_image_tool": {"path": str(script), "sha256": sha256(script)},
        "size": args.size,
    }


def command(args: argparse.Namespace, inputs: dict[str, Any]) -> list[str]:
    return [
        "bash",
        inputs["disk_image_tool"]["path"],
        inputs["authoritative_image"],
        inputs["output_directory"],
        "--composefs",
        "--sealed-source-imgref",
        inputs["local_mirror"],
        "--firstboot-seed",
        inputs["firstboot_seed"]["path"],
        "--size",
        inputs["size"],
        "--raw",
    ]


def write_receipt(path: Path, receipt: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(receipt, stream, indent=2, sort_keys=True)
        stream.write("\n")


def run(args: argparse.Namespace) -> int:
    started_at = utc_now()
    started = time.monotonic()
    if args.receipt and args.receipt.resolve().exists():
        raise FileExistsError(f"receipt already exists: {args.receipt.resolve()}")
    inputs = validate(args)
    argv = command(args, inputs)
    receipt: dict[str, Any] = {
        "schema": SCHEMA,
        "created_at": started_at,
        "kind": "dry-run" if args.dry_run else "installed-disk",
        "inputs": inputs,
        "command": argv,
    }
    if args.dry_run:
        receipt["duration_seconds"] = round(time.monotonic() - started, 3)
        receipt["status"] = "preflight-passed"
        if args.receipt:
            write_receipt(args.receipt.resolve(), receipt)
        print(json.dumps(receipt, indent=2, sort_keys=True))
        return 0

    process = subprocess.run(argv, check=False)
    receipt["duration_seconds"] = round(time.monotonic() - started, 3)
    receipt["exit_code"] = process.returncode
    receipt["status"] = "complete" if process.returncode == 0 else "failed"
    disk = Path(inputs["output_directory"]) / "disk.raw"
    if disk.is_file():
        stat = disk.stat()
        receipt["output"] = {
            "path": str(disk),
            "bytes": stat.st_size,
            "allocated_bytes": getattr(stat, "st_blocks", 0) * 512,
        }
    write_receipt(args.receipt.resolve(), receipt)
    print(json.dumps(receipt, indent=2, sort_keys=True))
    return process.returncode


def parser() -> argparse.ArgumentParser:
    repo = Path(__file__).resolve().parents[1]
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--image", required=True, help="authoritative digest-pinned OCI reference")
    result.add_argument("--mirror", required=True, help="digest-pinned local registry mirror")
    result.add_argument("--output", type=Path, required=True, help="new disposable output directory")
    result.add_argument("--firstboot-seed", type=Path, required=True, help="NAME:SHA512_CRYPT seed file")
    result.add_argument("--size", default="64G")
    result.add_argument("--disk-image", type=Path, default=repo / "os/dev/disk-image.sh")
    result.add_argument("--receipt", type=Path, help="new immutable JSON receipt path")
    result.add_argument("--dry-run", action="store_true", help="validate and print the exact command without root or disk writes")
    return result


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    if not args.dry_run and args.receipt is None:
        print("tilo-install-disk: --receipt is required for a real install", file=sys.stderr)
        return 2
    try:
        return run(args)
    except (FileExistsError, OSError, ValueError) as error:
        print(f"tilo-install-disk: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
