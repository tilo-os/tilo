#!/usr/bin/env python3
"""Fetch and verify the inputs of a Tilo local-model catalogue.

Every byte comes from the vendor's own source and is pinned in this
repository: model files by revision and SHA-256 (``<model>/model-files.sha256``,
fetched from the Hugging Face resolve URL at that revision), runtime packs by
OCI manifest digest (``runtimes/*.json``, the manifest fetched with
``skopeo``). A model is a Hugging Face snapshot (vLLM) or one GGUF file
(llama.cpp); its runtime pack names the processors it runs on. No model or
engine is a default: each device ranks what it supports. Nothing is
repackaged or mirrored. The output directory holds the verified files and
``local-models.json``, the source ``tilo-model-catalog`` compiles and signs.
This script never signs anything.

    dist/local-models/prepare-release.py \\
        --output /secure/build/local-models \\
        --base-image-digest "$TILO_BASE_IMAGE_DIGEST" \\
        dist/local-models/qwen3-4b-awq dist/local-models/qwen3-4b-gguf \\
        dist/local-models/smolvlm2-500m
"""

import argparse
import hashlib
import json
import pathlib
import re
import shutil
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
REVISION = re.compile(r"[0-9a-f]{40}|[0-9a-f]{64}")
SHA256 = re.compile(r"[0-9a-f]{64}")
RELEASE_KEY = "tilo-local-model-release-2026-01"
MODEL_FIELDS = {
    "id",
    "label",
    "description",
    "repository",
    "revision",
    "format",
    "license_id",
    "license_file",
    "requires_acceptance",
    "runtime",
    "required_memory_mib",
    "required_accelerator_mib",
    "context_window",
    "capabilities",
    "accelerator_memory_percent",
}
RUNTIME_FIELDS = {"id", "kinds", "version", "engine", "image", "manifest_url", "architecture"}
# The processors each engine's builds run on, and the model format it loads.
ENGINES = {
    "vllm": ({"nvidia_cuda", "amd_rocm", "cpu"}, "hf_snapshot"),
    "llama_cpp": ({"vulkan", "cpu"}, "gguf"),
}


def snapshot_path(path):
    parts = path.split("/")
    return bool(path) and not path.startswith("/") and all(part not in ("", ".", "..") for part in parts)


def load_manifest(model_dir, fmt):
    """`sha256  path` lines, validated for the release's format."""
    files = []
    for line in (model_dir / "model-files.sha256").read_text().splitlines():
        if not line.strip():
            continue
        digest, path = line.split("  ", 1)
        if not SHA256.fullmatch(digest) or not snapshot_path(path):
            raise ValueError(f"{model_dir.name}: bad manifest line {line!r}")
        files.append((digest, path))
    paths = [path for _, path in files]
    if fmt == "gguf":
        if len(paths) != 1 or not paths[0].endswith(".gguf") or "/" in paths[0]:
            raise ValueError(f"{model_dir.name}: a GGUF release is exactly one .gguf file")
    elif len(set(paths)) != len(paths) or "config.json" not in paths:
        raise ValueError(f"{model_dir.name}: manifest needs unique paths and config.json")
    return files


def load_release(model_dir):
    release = json.loads((model_dir / "release.json").read_text())
    if set(release) != MODEL_FIELDS:
        raise ValueError(f"{model_dir.name}: release.json fields must be exactly {sorted(MODEL_FIELDS)}")
    if not REVISION.fullmatch(release["revision"]):
        raise ValueError(f"{model_dir.name}: revision must be an immutable lowercase commit")
    if release["format"] not in {fmt for _, fmt in ENGINES.values()}:
        raise ValueError(f"{model_dir.name}: format must be hf_snapshot or gguf")
    return release


def load_runtime(name):
    runtime = json.loads((HERE / "runtimes" / f"{name}.json").read_text())
    if set(runtime) != RUNTIME_FIELDS:
        raise ValueError(f"runtime {name}: fields must be exactly {sorted(RUNTIME_FIELDS)}")
    repository, _, digest = runtime["image"].rpartition("@sha256:")
    if not repository or not SHA256.fullmatch(digest):
        raise ValueError(f"runtime {name}: image must be pinned by manifest digest")
    if not runtime["manifest_url"].endswith(f"/manifests/sha256:{digest}"):
        raise ValueError(f"runtime {name}: manifest URL must name the same digest")
    if runtime["engine"] not in ENGINES:
        raise ValueError(f"runtime {name}: engine must be one of {sorted(ENGINES)}")
    kinds = runtime["kinds"]
    if not kinds or len(set(kinds)) != len(kinds) or not set(kinds) <= ENGINES[runtime["engine"]][0]:
        raise ValueError(f"runtime {name}: kinds must be processors its engine runs on")
    return runtime


def resolve_url(release, path):
    return f"https://huggingface.co/{release['repository']}/resolve/{release['revision']}/{path}"


def sha256_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def fetch(url, destination, expected):
    """Resume `url` into `destination` and verify it, or fail."""
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists() and sha256_file(destination) == expected:
        return
    subprocess.run(
        ["curl", "--fail", "--location", "--proto", "=https", "--proto-redir", "=https",
         "--tlsv1.2", "--retry", "3", "--continue-at", "-", "--output", str(destination), url],
        check=True,
    )
    actual = sha256_file(destination)
    if actual != expected:
        destination.unlink()
        raise ValueError(f"{url}: SHA-256 {actual} is not the pinned {expected}")


def fetch_manifest(runtime, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    raw = subprocess.run(
        ["skopeo", "inspect", "--raw", f"docker://{runtime['image']}"], check=True, capture_output=True
    ).stdout
    digest = runtime["image"].rpartition("@sha256:")[2]
    if hashlib.sha256(raw).hexdigest() != digest:
        raise ValueError(f"runtime {runtime['id']}: registry manifest does not hash to its digest")
    destination.write_bytes(raw)


def model_artifact(release, files, slug):
    """The compiler's artifact for a release: every snapshot file, or its one GGUF."""
    entries = [
        {
            "path": path,
            "file": f"models/{slug}/{path}",
            "url": resolve_url(release, path),
            "sha256": digest,
        }
        for digest, path in files
    ]
    if release["format"] == "gguf":
        (entry,) = entries
        del entry["path"]
        return {"gguf": entry}
    return {"hf_snapshot": {"files": entries}}


def catalog_source(models, runtimes, key_id, base_image_digest, root):
    """The `tilo-model-catalog` source for already verified files under `root`."""
    source_runtimes = [
        {
            "id": runtime["id"],
            "kinds": runtime["kinds"],
            "version": runtime["version"],
            "engine": runtime["engine"],
            "oci_image": runtime["image"],
            "artifact": {
                "file": f"runtimes/{runtime['id']}.manifest.json",
                "url": runtime["manifest_url"],
                "sha256": runtime["image"].rpartition("@sha256:")[2],
            },
            "base_image_digest": base_image_digest,
            "architecture": runtime["architecture"],
            "kernel_abi": None,
            "requires_reboot": False,
        }
        for runtime in runtimes
    ]
    by_id = {runtime["id"]: runtime for runtime in runtimes}
    source_models = []
    for release, files in models:
        slug = release["id"].replace("/", "--")
        installed = sum((root / "models" / slug / path).stat().st_size for _, path in files)
        runtime = by_id[release["runtime"]]
        if ENGINES[runtime["engine"]][1] != release["format"]:
            raise ValueError(f"{release['id']}: {runtime['engine']} does not load {release['format']}")
        source_models.append(
            {
                "id": release["id"],
                "label": release["label"],
                "description": release["description"],
                "revision": release["revision"],
                "artifact": model_artifact(release, files, slug),
                "license_id": release["license_id"],
                "license_url": f"https://huggingface.co/{release['repository']}/blob/{release['revision']}/{release['license_file']}",
                "requires_acceptance": release["requires_acceptance"],
                "installed_size_bytes": installed,
                "engines": [runtime["engine"]],
                "allowed_runtimes": runtime["kinds"],
                "required_memory_mib": release["required_memory_mib"],
                "required_accelerator_mib": release["required_accelerator_mib"],
                "context_window": release["context_window"],
                "capabilities": release["capabilities"],
                "accelerator_memory_percent": release["accelerator_memory_percent"],
            }
        )
    return {
        "schema": 1,
        "production": True,
        "key_id": key_id,
        "runtimes": source_runtimes,
        "models": source_models,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--base-image-digest", required=True)
    parser.add_argument("--key-id", default=RELEASE_KEY, help="trust key id the catalogue will be signed with")
    parser.add_argument("models", nargs="+", type=pathlib.Path)
    args = parser.parse_args()
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", args.base_image_digest):
        parser.error("base image digest must be sha256: followed by 64 lowercase hex characters")
    for command in ("curl", "skopeo"):
        if shutil.which(command) is None:
            parser.error(f"required command is unavailable: {command}")

    models = []
    for path in args.models:
        release = load_release(path)
        models.append((release, load_manifest(path, release["format"])))
    runtimes = [load_runtime(name) for name in sorted({release["runtime"] for release, _ in models})]
    root = args.output.resolve()
    for runtime in runtimes:
        fetch_manifest(runtime, root / "runtimes" / f"{runtime['id']}.manifest.json")
    for release, files in models:
        slug = release["id"].replace("/", "--")
        for digest, path in files:
            fetch(resolve_url(release, path), root / "models" / slug / path, digest)
    source = catalog_source(models, runtimes, args.key_id, args.base_image_digest, root)
    (root / "local-models.json").write_text(json.dumps(source, indent=2) + "\n")
    print(f"Verified every pinned input under {root}; compile {root / 'local-models.json'} with tilo-model-catalog.")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, subprocess.CalledProcessError, OSError) as error:
        print(f"prepare-release: {error}", file=sys.stderr)
        sys.exit(1)
