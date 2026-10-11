"""Produce the v1 same-run layer handoff using the maintained native packer."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import tomllib
from pathlib import Path

SCHEMA = "tilo.vm.layer-artifacts/v1"
COHORT = ("launcher", "install", "shell", "apps+runtime")


def sha(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as file:
        for chunk in iter(lambda: file.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def identities(source: str, builder: str, run: int, attempt: int) -> dict:
    if not re.fullmatch(r"[0-9a-f]{40}", source):
        raise ValueError("handoff requires an exact source commit")
    if not re.fullmatch(r"registry\.tilo\.so/tilo/tilo-builder@sha256:[0-9a-f]{64}", builder):
        raise ValueError("handoff requires the immutable CI builder")
    if run < 1 or attempt < 1:
        raise ValueError("handoff requires a positive workflow run and attempt")
    return dict(schema=SCHEMA, repository="tilo/tilo", source_commit=source,
                builder_image=builder, run_id=run, run_attempt=attempt, architecture="x86_64")


def components(requested: list[str]) -> list[str]:
    if len(requested) != len(set(requested)) or not requested:
        raise ValueError("handoff needs unique requested components")
    return list(dict.fromkeys([*COHORT, *requested])) if "apps+runtime" in requested else requested


def file_record(root: Path, path: Path) -> dict:
    return dict(path=path.relative_to(root).as_posix(), sha256=sha(path), bytes=path.stat().st_size)


def source_files(repo: Path) -> dict:
    listed = subprocess.run(["git", "ls-files", "-z"], cwd=repo, check=True,
                            capture_output=True).stdout.split(b"\0")
    files = {}
    for raw in filter(None, listed):
        name = raw.decode("utf-8")
        path = repo / name
        if not path.resolve().is_relative_to(repo.resolve()):
            raise ValueError("source input escapes the worktree")
        if path.is_symlink():
            payload = b"\1" + str(path.readlink()).encode() + path.read_bytes()
        elif path.is_file():
            payload = b"\0" + path.read_bytes()
        else:
            raise ValueError("source input is not a file")
        files[name] = hashlib.sha256(payload).hexdigest()
    if not files:
        raise ValueError("empty source inventory")
    return files


def header(path: Path) -> dict:
    with path.open("rb") as file:
        line = file.readline(1 << 20)
    prefix = b"tilo-vm-layer-begin "
    if not line.startswith(prefix) or not line.endswith(b"\n"):
        raise ValueError("missing full layer stream header")
    value = json.loads(line[len(prefix):])
    if value.get("reused") or value.get("binary"):
        raise ValueError("handoff requires a full ordinary layer stream")
    for key in ("layer_hash", "source_digest"):
        if not re.fullmatch(r"[0-9a-f]{64}", value.get(key, "")):
            raise ValueError("missing layer/source identity")
    if not value.get("toolchain") or value.get("target") != "x86_64-unknown-linux-gnu":
        raise ValueError("missing or mismatched native build facts")
    # The CLI verifies decoded archive/footer and signatures before daemon mutations.
    with path.open("rb") as file:
        file.seek(max(0, path.stat().st_size - 256))
        footer = file.read().splitlines()[-1]
    if not re.fullmatch(rb"tilo-vm-layer-end [0-9a-f]{64} [1-9][0-9]*", footer):
        raise ValueError("incomplete or cache-only stream")
    return value


def verify(manifest: Path, expected: dict, receipt_sha: str, requested: list[str]) -> list[str]:
    """Check authenticated handoff identity/completeness before starting the VM."""
    if not re.fullmatch(r"[0-9a-f]{64}", receipt_sha) or sha(manifest) != receipt_sha:
        raise ValueError("artifact receipt differs from authenticated producer output")
    receipt = json.loads(manifest.read_bytes())
    if set(receipt) != {*expected, "layers"} or any(receipt.get(k) != v for k, v in expected.items()):
        raise ValueError("artifact source/builder/run/architecture mismatch")
    found = {}
    for layer in receipt["layers"]:
        name = layer["component"]
        if name in found:
            raise ValueError("duplicate handoff component")
        found[name] = layer
        for kind in ("stream", "inventory"):
            record = layer[kind]
            relative = Path(record["path"])
            path = manifest.parent / relative
            if relative.is_absolute() or any(p in (".", "..") for p in relative.parts) \
                    or path.is_symlink() or not path.resolve().is_relative_to(manifest.parent.resolve()):
                raise ValueError("artifact path escapes handoff")
            if record["bytes"] < 1 or file_record(manifest.parent, path) != record:
                raise ValueError("artifact bytes/digest mismatch")
        packed = header(manifest.parent / layer["stream"]["path"])
        if any(packed[k] != layer[k] for k in ("component", "target", "toolchain", "source_digest", "layer_hash")) \
                or layer["source_digest"] != layer["inventory"]["sha256"]:
            raise ValueError("artifact header/inventory identity mismatch")
        inventory = json.loads((manifest.parent / layer["inventory"]["path"]).read_bytes())
        provenance = inventory["provenance"]
        if provenance.get("source_commit") != expected["source_commit"] \
                or provenance.get("component") != name or provenance.get("dirty_tree_hash"):
            raise ValueError("artifact inventory source/component mismatch")
    ordered = components(requested)
    if set(found) != set(ordered):
        raise ValueError("incomplete or unexpected component handoff")
    return ordered


def produce(repo: Path, out: Path, expected: dict, requested: list[str], packer: str) -> str:
    head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=repo, check=True,
                          capture_output=True, text=True).stdout.strip()
    dirty = subprocess.run(["git", "status", "--porcelain", "--untracked-files=no"], cwd=repo,
                           check=True, capture_output=True).stdout
    if head != expected["source_commit"] or dirty:
        raise ValueError("producer requires the exact clean source")
    out.mkdir(parents=True, exist_ok=False)
    manifest = tomllib.loads((repo / "os/components.toml").read_text())
    policies = {c["name"]: c for c in manifest["component"]}
    files = source_files(repo)
    layers = []
    for name in components(requested):
        policy = policies.get(name)
        if not policy or policy.get("kind") != "userspace":
            raise ValueError("artifact producer supports maintained userspace components only")
        profile = policy.get("profile", "release")
        features = list(dict.fromkeys([*policy.get("features", []), *policy.get("dev_features", [])]))
        inventory = out / f".tilo-vm-inputs-{name}.json"
        inventory.write_text(json.dumps(dict(provenance=dict(component=name, authority="local",
                            source_commit=head, builder="ci", cargo_lock_hash=sha(repo / "Cargo.lock"),
                            builder_image_digest=expected["builder_image"].split("@", 1)[1],
                            cargo_profile=profile, features=features), files=files), separators=(",", ":")) + "\n")
        stream = out / f"{name}.stream"
        command = [packer, "layer-pack", "--component", name, "--profile", profile,
                   "--source-inventory", str(inventory)]
        with stream.open("wb") as output:
            subprocess.run(command, cwd=repo, stdout=output, check=True)
        if source_files(repo) != files:
            raise ValueError("source changed during upstream assembly")
        packed = header(stream)
        if packed["component"] != name or packed["source_digest"] != sha(inventory):
            raise ValueError("packer source/component mismatch")
        layers.append(dict(component=name, profile=profile, features=features,
                           **{k: packed[k] for k in ("target", "toolchain", "source_digest", "layer_hash")},
                           stream=file_record(out, stream), inventory=file_record(out, inventory)))
    receipt = out / "manifest.json"
    receipt.write_text(json.dumps(dict(expected, layers=layers), sort_keys=True, separators=(",", ":")) + "\n")
    digest = sha(receipt)
    verify(receipt, expected, digest, requested)
    return digest


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("produce", "verify"))
    parser.add_argument("--source", required=True)
    parser.add_argument("--builder-image", required=True)
    parser.add_argument("--run-id", required=True, type=int)
    parser.add_argument("--run-attempt", required=True, type=int)
    parser.add_argument("--components", required=True)
    parser.add_argument("--directory", required=True, type=Path)
    parser.add_argument("--repo", type=Path)
    parser.add_argument("--packer", default="/usr/local/bin/tilo-vm")
    parser.add_argument("--receipt-sha256")
    args = parser.parse_args()
    expected = identities(args.source, args.builder_image, args.run_id, args.run_attempt)
    requested = args.components.split(",")
    if args.mode == "produce":
        print(produce(args.repo.resolve(), args.directory.resolve(), expected, requested, args.packer))
    else:
        verify(args.directory / "manifest.json", expected, args.receipt_sha256 or "", requested)


if __name__ == "__main__":
    main()
