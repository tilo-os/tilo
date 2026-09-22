#!/usr/bin/env python3
"""Write the dev image's identity: base.json, update policy and flavour marker.

    finalize-dev-image.py --base-image REF [--base-digest sha256:...] \
        --cargo-lock Cargo.lock [--root /]

Run by os/Containerfile.dev as its last content step:

* /usr/share/tilo-dev/base.json is what tilo-devd checks every imported layer
  against (tilo_dev_protocol::BaseInfo): the production base image's digest,
  plus the in-image toolchain (`rustc --version`), its target triple and the
  Cargo.lock hash, in the same form `tilo-dev` records in a layer's provenance.
* /etc/containers/policy.json lets the dev VM `bootc switch`/`upgrade` onto
  code.podesta.ai/qos/tilo-dev, which is deliberately unsigned. Production
  policy rejects that repository outright (os/etc/containers/policy.json), so
  this rewrites exactly that one scope and nothing else; the signed production
  repository keeps its sigstore requirement.
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

DEV_REPOSITORY = "code.podesta.ai/qos/tilo-dev"
PRODUCTION_REPOSITORY = "code.podesta.ai/qos/tilo"


def fail(message: str) -> None:
    raise SystemExit(f"finalize-dev-image: {message}")


def first_line(*command: str) -> str:
    return subprocess.run(command, check=True, capture_output=True, text=True).stdout.splitlines()[0].strip()


def base_info(base_image: str, base_digest: str | None, cargo_lock: Path) -> dict:
    digest = base_digest or None
    if digest is None:
        match = re.search(r"@(sha256:[0-9a-f]{64})$", base_image)
        digest = match.group(1) if match else None
    if digest is not None and not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        fail(f"base image digest {digest!r} is not sha256:<64 hex>")
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
    }


def dev_policy(policy: dict) -> dict:
    docker = policy.setdefault("transports", {}).setdefault("docker", {})
    current = docker.get(DEV_REPOSITORY)
    if current is not None and current != [{"type": "reject"}]:
        fail(f"the base image's policy already admits {DEV_REPOSITORY} ({current}); a production "
             "image must reject it")
    production = json.dumps(docker.get(PRODUCTION_REPOSITORY), sort_keys=True)
    docker[DEV_REPOSITORY] = [{"type": "insecureAcceptAnything"}]
    if json.dumps(docker.get(PRODUCTION_REPOSITORY), sort_keys=True) != production:
        fail(f"rewriting {DEV_REPOSITORY} changed the {PRODUCTION_REPOSITORY} policy")
    return policy


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=Path("/"))
    parser.add_argument("--base-image", required=True)
    parser.add_argument("--base-digest", default="")
    parser.add_argument("--cargo-lock", type=Path, required=True)
    args = parser.parse_args(argv)
    root = args.root

    flavour = root / "usr/lib/tilo/image-flavour"
    if flavour.exists():
        fail(f"the base already carries {flavour} ({flavour.read_text().strip()}); build from the "
             "production image")

    info = base_info(args.base_image, args.base_digest, args.cargo_lock)
    target = root / "usr/share/tilo-dev/base.json"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(json.dumps(info, indent=2) + "\n")

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
