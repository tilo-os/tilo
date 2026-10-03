#!/usr/bin/env python3
"""Record small, source-bound OCI digests for a failed reproduction gate.

Only index, manifest and config JSON are read from the OCI tar. Layer blobs,
environment values and BuildKit secrets are never copied to the report.
"""

import argparse
import hashlib
import json
import re
import tarfile
from pathlib import Path


SHA256 = re.compile(r"sha256:[0-9a-f]{64}\Z")
ARCH = {"aarch64": "arm64", "x86_64": "amd64"}


def checked_blob(archive: tarfile.TarFile, digest: str) -> dict:
    if not SHA256.fullmatch(digest):
        raise ValueError(f"invalid OCI digest: {digest}")
    member = archive.extractfile(f"blobs/sha256/{digest[7:]}")
    if member is None:
        raise ValueError(f"missing OCI metadata blob: {digest}")
    data = member.read()
    if hashlib.sha256(data).hexdigest() != digest[7:]:
        raise ValueError(f"OCI metadata blob fails digest check: {digest}")
    return json.loads(data)


def report(archive_path: Path, source: str, run_id: str, arch: str, role: str) -> dict:
    if not re.fullmatch(r"[0-9a-f]{40}", source):
        raise ValueError("source must be a full commit SHA")
    if not re.fullmatch(r"[0-9]+", run_id):
        raise ValueError("run ID must be numeric")
    if arch not in ARCH or role not in {"primary", "reproduce"}:
        raise ValueError("unsupported architecture or role")
    with tarfile.open(archive_path, "r") as archive:
        index_file = archive.extractfile("index.json")
        if index_file is None:
            raise ValueError("OCI archive lacks index.json")
        index = json.load(index_file)
        digests = {item.get("digest") for item in index.get("manifests", [])}
        if len(digests) != 1:
            raise ValueError("OCI archive must contain one unique manifest")
        manifest_digest = digests.pop()
        manifest = checked_blob(archive, manifest_digest)
        config_digest = manifest["config"]["digest"]
        config = checked_blob(archive, config_digest)
    if config.get("architecture") != ARCH[arch] or config.get("os") != "linux":
        raise ValueError("OCI config architecture or OS differs from job binding")
    if config.get("config", {}).get("Labels", {}).get(
        "org.opencontainers.image.revision"
    ) != source:
        raise ValueError("OCI config source revision differs from job binding")
    layers = manifest["layers"]
    diff_ids = config["rootfs"]["diff_ids"]
    if len(layers) != len(diff_ids) or any(
        not SHA256.fullmatch(item["digest"]) for item in layers
    ) or any(not SHA256.fullmatch(item) for item in diff_ids):
        raise ValueError("OCI layer and rootfs diff ID lists disagree")
    return {
        "schema": 1,
        "source": source,
        "run_id": run_id,
        "arch": arch,
        "role": role,
        "manifest_digest": manifest_digest,
        "config_digest": config_digest,
        "created": config.get("created"),
        "layers": [
            {"digest": item["digest"], "diff_id": diff_ids[i], "size": item["size"]}
            for i, item in enumerate(layers)
        ],
        # Hash history entries rather than publishing environment/build args.
        "history_digests": [
            "sha256:" + hashlib.sha256(json.dumps(
                item, sort_keys=True, separators=(",", ":")
            ).encode()).hexdigest()
            for item in config.get("history", [])
        ],
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--source", required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--arch", required=True)
    parser.add_argument("--role", required=True)
    args = parser.parse_args()
    document = report(args.archive, args.source, args.run_id, args.arch, args.role)
    args.output.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n",
                           encoding="utf-8")


if __name__ == "__main__":
    main()
