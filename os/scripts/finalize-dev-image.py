#!/usr/bin/env python3
"""Write the dev image's identity: base.json, update policy and flavour marker.

    finalize-dev-image.py --base-image REF [--base-digest sha256:...] \
        --cargo-lock Cargo.lock [--root /]

Run by os/Containerfile.dev as its last content step:

* /usr/share/tilo-vm/base.json is what tilo-vmd checks every imported layer
  against (the protocol crate's BaseInfo): the production base image's digest,
  plus the in-image toolchain (`rustc --version`), its target triple and the
  Cargo.lock hash, in the same form `tilo-vm` records in a layer's provenance.
  `base_created` is the base image's creation time, which production builds
  clamp to their commit's time; `tilo-vm vm upgrade` refuses to move a VM onto
  a dev image whose base is older than the one it runs.
* /etc/containers/policy.json lets the dev image `bootc switch`/`upgrade` onto
  registry.tilo.so/tilo/tilo-vm, the dev channel, and registry.tilo.so/tilo/tilo-vm-ref,
  branches' own builds, each with the development key's
  signature (keys/cosign-dev.pub, installed as DEV_KEY): only t's checks on main
  sign with it. Production policy rejects that repository outright
  (os/etc/containers/policy.json), so this rewrites exactly those scopes and
  nothing else; the signed production repository keeps its sigstore requirement.
* /usr/lib/tilo/image-flavour holds `dev`, so tools and humans can tell a dev
  deployment from a production one, and a dev image cannot be a dev base.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path

DEV_REPOSITORY = "registry.tilo.so/tilo/tilo-vm"
# Branches' own builds, which a device follows only while one is pinned (tilo-update-channel).
REF_REPOSITORY = "registry.tilo.so/tilo/tilo-vm-ref"
PRODUCTION_REPOSITORY = "registry.tilo.so/tilo/tilo"
DEV_KEY = "/usr/share/tilo/cosign-dev.pub"


def fail(message: str) -> None:
    raise SystemExit(f"finalize-dev-image: {message}")


def first_line(*command: str) -> str:
    return subprocess.run(command, check=True, capture_output=True, text=True).stdout.splitlines()[0].strip()


def base_info(base_image: str, base_digest: str | None, cargo_lock: Path,
              base_created: str | None = None) -> dict:
    digest = base_digest or None
    if digest is None:
        match = re.search(r"@(sha256:[0-9a-f]{64})$", base_image)
        digest = match.group(1) if match else None
    if digest is not None and not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        fail(f"base image digest {digest!r} is not sha256:<64 hex>")
    created = base_created or None
    if created is not None and not re.fullmatch(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})", created):
        fail(f"base image creation time {created!r} is not an RFC 3339 timestamp")
    verbose = subprocess.run(["rustc", "-vV"], check=True, capture_output=True, text=True).stdout
    target = next((line.removeprefix("host: ").strip() for line in verbose.splitlines()
                   if line.startswith("host: ")), None)
    if target is None:
        fail("`rustc -vV` names no host target")
    return {
        "image_digest": digest,
        # No separate builder image: the dev image is its own builder, and its
        # toolchain is identified by `toolchain`, `target` and the RPM snapshot.
        "builder_image_digest": None,
        "toolchain": first_line("rustc", "--version"),
        "target": target,
        "cargo_lock_hash": hashlib.sha256(cargo_lock.read_bytes()).hexdigest(),
        "base_image": base_image,
        "base_created": created,
    }


def dev_policy(policy: dict) -> dict:
    docker = policy.setdefault("transports", {}).setdefault("docker", {})
    production = json.dumps(docker.get(PRODUCTION_REPOSITORY), sort_keys=True)
    for repository in (DEV_REPOSITORY, REF_REPOSITORY):
        current = docker.get(repository)
        if current is not None and current != [{"type": "reject"}]:
            fail(f"the base image's policy already admits {repository} ({current}); a production "
                 "image must reject it")
        docker[repository] = [{"type": "sigstoreSigned", "keyPath": DEV_KEY,
                               "signedIdentity": {"type": "matchRepository"}}]
    if json.dumps(docker.get(PRODUCTION_REPOSITORY), sort_keys=True) != production:
        fail(f"rewriting {DEV_REPOSITORY} changed the {PRODUCTION_REPOSITORY} policy")
    return policy


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=Path("/"))
    parser.add_argument("--base-image", required=True)
    parser.add_argument("--base-digest", default="")
    parser.add_argument("--base-created", default="",
                        help="the base image's creation time, RFC 3339 (podman image inspect .Created)")
    parser.add_argument("--cargo-lock", type=Path, required=True)
    args = parser.parse_args(argv)
    root = args.root

    flavour = root / "usr/lib/tilo/image-flavour"
    if flavour.exists():
        fail(f"the base already carries {flavour} ({flavour.read_text().strip()}); build from the "
             "production image")

    info = base_info(args.base_image, args.base_digest, args.cargo_lock, args.base_created)
    target = root / "usr/share/tilo-vm/base.json"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(json.dumps(info, indent=2) + "\n")

    if not (root / DEV_KEY.lstrip("/")).is_file():
        fail(f"{DEV_KEY} is missing; the dev image must install keys/cosign-dev.pub there")
    policy_path = root / "etc/containers/policy.json"
    if not policy_path.is_file():
        fail(f"{policy_path} is missing; the base is not a Tilo image")
    policy = dev_policy(json.loads(policy_path.read_text()))
    policy_path.write_text(json.dumps(policy, indent=2) + "\n")

    flavour.parent.mkdir(parents=True, exist_ok=True)
    flavour.write_text("dev\n")
    print(json.dumps(info, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
