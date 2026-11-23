#!/usr/bin/env python3
"""Write the dev image's identity: base.json, update policy and flavour marker.

    finalize-dev-image.py --base-image REF [--base-digest sha256:...] \
        --cargo-lock Cargo.lock [--root /]
    finalize-dev-image.py --base-only --base-image REF --base-digest sha256:... \
        --builder-image REF@sha256:... --built-binary PATH... --cargo-lock Cargo.lock

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

With --base-only (os/Containerfile.host-dev, the development server image) it
writes base.json alone: the server stays a server, flavour and update policy
included. Its binaries were compiled outside the image, by t's check image
(tools/ci/publish/server-binaries.sh), which carries no toolchain into it, so
`toolchain` and `target` are read off the built binaries the way
`tilo-vm layer-pack` reads them off a layer's (the ELF machine and the
`.comment` section rustc writes), and `builder_image_digest` names that check
image: a layer compiled by another image, or for another target, is refused.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import struct
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


ELF_ARCHES = {0x3E: "x86_64", 0xB7: "aarch64", 0xF3: "riscv64gc"}


def elf_facts(path: Path) -> tuple[str, str]:
    """(target, toolchain) of a Rust ELF binary, as `tilo-vm layer-pack` reads them
    (tools/tilo-vm/src/build.rs, elf_facts)."""
    data = path.read_bytes()
    if data[:6] != b"\x7fELF\x02\x01":
        fail(f"{path} is not a 64-bit little-endian ELF file")
    arch = ELF_ARCHES.get(struct.unpack_from("<H", data, 0x12)[0])
    if arch is None:
        fail(f"{path} is built for a machine tilo-vm does not know")
    section_offset = struct.unpack_from("<Q", data, 0x28)[0]
    entry_size, count, names_index = struct.unpack_from("<HHH", data, 0x3A)

    def section(index: int) -> tuple[int, int, int]:
        header = section_offset + index * entry_size
        (name,) = struct.unpack_from("<I", data, header)
        offset, size = struct.unpack_from("<QQ", data, header + 0x18)
        return name, offset, size

    _, names_at, names_size = section(names_index)
    names = data[names_at:names_at + names_size]
    for index in range(count):
        name, offset, size = section(index)
        if names[name:].split(b"\0", 1)[0] != b".comment":
            continue
        for line in data[offset:offset + size].split(b"\0"):
            text = line.decode("utf-8", "replace")
            if text.startswith("rustc version "):
                return f"{arch}-unknown-linux-gnu", "rustc " + text.removeprefix("rustc version ")
    fail(f"{path} names no rustc in its .comment section")


def built_toolchain(binaries: list[Path]) -> tuple[str, str]:
    """The one (target, toolchain) every binary was built with."""
    facts = {path: elf_facts(path) for path in binaries}
    if len(set(facts.values())) != 1:
        fail("the binaries were built by different toolchains: " +
             "; ".join(f"{path}: {target}, {toolchain}" for path, (target, toolchain) in facts.items()))
    return next(iter(facts.values()))


def base_info(base_image: str, base_digest: str | None, cargo_lock: Path,
              base_created: str | None = None, builder_image: str | None = None,
              built: list[Path] | None = None) -> dict:
    digest = base_digest or None
    if digest is None:
        match = re.search(r"@(sha256:[0-9a-f]{64})$", base_image)
        digest = match.group(1) if match else None
    if digest is not None and not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        fail(f"base image digest {digest!r} is not sha256:<64 hex>")
    created = base_created or None
    if created is not None and not re.fullmatch(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})", created):
        fail(f"base image creation time {created!r} is not an RFC 3339 timestamp")
    builder_digest = None
    if built:
        match = re.fullmatch(r"[^@\s]+@(sha256:[0-9a-f]{64})", builder_image or "")
        if match is None:
            fail(f"the builder image {builder_image!r} is not pinned by digest")
        builder_digest = match.group(1)
        target, toolchain = built_toolchain(built)
    else:
        verbose = subprocess.run(["rustc", "-vV"], check=True, capture_output=True, text=True).stdout
        target = next((line.removeprefix("host: ").strip() for line in verbose.splitlines()
                       if line.startswith("host: ")), None)
        if target is None:
            fail("`rustc -vV` names no host target")
        toolchain = first_line("rustc", "--version")
    return {
        "image_digest": digest,
        # A dev image is its own builder, and its toolchain is identified by
        # `toolchain`, `target` and the RPM snapshot; a dev server's binaries
        # come from the check image named here.
        "builder_image_digest": builder_digest,
        "toolchain": toolchain,
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
    parser.add_argument("--base-only", action="store_true",
                        help="write base.json alone, from --built-binary and --builder-image")
    parser.add_argument("--builder-image", default="",
                        help="the image, pinned by digest, the --built-binary programs were compiled in")
    parser.add_argument("--built-binary", type=Path, action="append", default=[],
                        help="a program of the image compiled outside it; repeatable, all must agree")
    args = parser.parse_args(argv)
    root = args.root
    if args.base_only != bool(args.built_binary):
        fail("--base-only and --built-binary go together")

    target = root / "usr/share/tilo-vm/base.json"
    flavour = root / "usr/lib/tilo/image-flavour"
    if args.base_only:
        if target.exists():
            fail(f"the base already carries {target}; build from the server image")
        info = base_info(args.base_image, args.base_digest, args.cargo_lock, args.base_created,
                         args.builder_image, [root / path.relative_to("/") for path in args.built_binary])
        if info["image_digest"] is None:
            fail("--base-only needs the base image's digest")
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(json.dumps(info, indent=2) + "\n")
        print(json.dumps(info, indent=2))
        return 0

    if flavour.exists():
        fail(f"the base already carries {flavour} ({flavour.read_text().strip()}); build from the "
             "production image")

    info = base_info(args.base_image, args.base_digest, args.cargo_lock, args.base_created)
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
