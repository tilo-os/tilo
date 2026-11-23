#!/usr/bin/env python3
"""Which components a retry of the server install proof rebuilds, and what it cannot take.

    server-plan-retry.py select --base COMMIT [--root DIR]

A retry (tools/ci/server-plan-retry.sh) installs the Tilo Server from a base an earlier
dispatch built: the installer medium and its bundle, the test Imager and the owner's dev
disk, all of COMMIT. The tree under test differs from COMMIT; this decides how each
difference reaches the proof, and prints it as one JSON document:

  components  rebuilt as tilo-vm layers (os/components.toml) from this tree
  server      of those, the server image's (`images = ["host"]`): the live installer copies
              their layers into the installed server's tilo-vm state, and its first boot merges them
  owner       of those, the OS image's, which the owner's dev disk takes before its session starts
  direct      files the proof reads straight from this tree (its scripts)
  outside     files no part of this proof builds or reads
  refused     differences the base would have to be rebuilt for, each with the reason

A difference is refused when it would change what the base holds and a layer cannot
replace: a program the live installer runs (the medium's root is a sealed image, which
merges no layer), a program of the encrypted root's early boot (the initramfs), or any
other part of an OS image (os/: units, image files, Containerfiles, the kernel). A session
component is refused too: it merges through a machine's own session, and the owner's
session here is one the proof's driver starts, not a tilo-vm account.

A change is attributed per program, not per package: a file that is one program's own
source (its `[[bin]]` path, or the directory a `main.rs` bin owns) reaches only that
program; any other file of a package reaches every program it and its dependants build.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import sys
import tomllib

TOOLS = Path(__file__).resolve().parents[1]
if str(TOOLS) not in sys.path:
    sys.path.append(str(TOOLS))
from ci.workspace_graph import cargo_metadata, file_at, lockfile_change, owner_for, package_maps

# What this proof runs from the tree itself, so a change to it needs no rebuild.
DIRECT = (
    "os/dev/test-server-plan-install-cycle.sh",
    "os/dev/server-plan-owner.sh",
    "os/dev/vm.sh",
    "tools/ci/server-plan-retry.sh",
    "tools/ci/server-plan-retry.py",
    "tools/ci/server-plan-base.env",
    "tools/ci/install-packages.sh",
    "tools/ci/packages/",
    # The image the proof's guests boot in, not one the base holds.
    "os/Containerfile.os-vm",
)
# The OS images' own sources; a change there is part of the base.
OS_BASE = ("os/", "keys/")
# Read by the layer packer, not by the base: a change applies through the layers it packs.
LAYER_INPUTS = ("os/components.toml", "os/scripts/install-components.py")
# Components whose programs the live installer runs: the medium is sealed and takes no layer.
MEDIUM_COMPONENTS = {"installer", "install-helpers"}
# Programs of other components the live installer runs too (the login a ticket makes, and
# the unlock from another of the person's devices).
MEDIUM_BINS = {"tilo", "tilo-crypto", "tilo-auth"}
# Programs of the encrypted root's early boot: in the initramfs, which no layer reaches.
EARLY_BOOT_BINS = {"tilo-auth", "tilo-boot-signin", "tilo-rollback"}


def git(root: Path, *args: str) -> bytes:
    return subprocess.check_output(["git", *args], cwd=root)


def changed_paths(root: Path, base: str) -> list[str]:
    """Every path of this working tree that differs from BASE, committed or not."""
    tracked = git(root, "diff", "--no-renames", "--name-only", "-z", base, "--")
    untracked = git(root, "ls-files", "--others", "--exclude-standard", "-z")
    paths = {p.decode("utf-8", "surrogateescape") for p in (tracked + b"\0" + untracked).split(b"\0") if p}
    return sorted(paths)


def unlisted_host_bins(root: Path) -> set[str]:
    """The server image's programs no os/components.toml entry places: what
    tools/ci/publish/server-binaries.sh builds for it beside its `images = ["host"]` components."""
    def listed(*command: str) -> set[str]:
        return set(subprocess.check_output(command, cwd=root, text=True).split())
    every = listed("bash", "tools/ci/publish/server-binaries.sh", "--list", "host")
    placed = listed("python3", "os/scripts/install-components.py", "--dry-run", "--format", "bins",
                    "--all", "--image", "host")
    if not every or not placed <= every:
        raise SystemExit("server-plan-retry: server-binaries.sh does not build the server image's components")
    return every - placed


def bin_roots(package: dict, root: Path) -> list[tuple[str, Path, bool]]:
    """Each program of PACKAGE: its name, its source root, and whether it owns that directory."""
    out = []
    for target in package["targets"]:
        if "bin" not in target["kind"]:
            continue
        src = Path(target["src_path"]).resolve().relative_to(root.resolve())
        package_src = Path(package["manifest_path"]).resolve().parent.relative_to(root.resolve()) / "src"
        owns_dir = src.name == "main.rs" and src.parent != package_src
        out.append((target["name"], src, owns_dir))
    return out


def link_dependants(packages: dict[str, dict]) -> dict[str, set[str]]:
    """Package -> the workspace packages that link it (normal and build dependencies)."""
    by_manifest = {str(Path(p["manifest_path"]).resolve()): n for n, p in packages.items()}
    reverse: dict[str, set[str]] = {n: set() for n in packages}
    for consumer, pkg in packages.items():
        for dep in pkg.get("dependencies", []):
            if not dep.get("path") or dep.get("kind") == "dev":
                continue
            provider = by_manifest.get(str(Path(dep["path"]).resolve() / "Cargo.toml"))
            if provider:
                reverse[provider].add(consumer)
    return reverse


def every_bin(names: set[str], packages: dict[str, dict], reverse: dict[str, set[str]]) -> set[str]:
    seen, pending = set(names), list(names)
    while pending:
        for dependant in reverse.get(pending.pop(), ()):
            if dependant not in seen:
                seen.add(dependant)
                pending.append(dependant)
    return {t["name"] for n in seen for t in packages[n]["targets"] if "bin" in t["kind"]}


def select(root: Path, base: str) -> dict:
    metadata = cargo_metadata(root)
    packages, by_dir = package_maps(metadata, root)
    reverse = link_dependants(packages)
    components = tomllib.loads((root / "os/components.toml").read_text(encoding="utf-8"))["component"]
    unlisted = unlisted_host_bins(root)

    affected: set[str] = set()
    direct, outside, refused = [], [], []
    for path in changed_paths(root, base):
        if path.startswith(DIRECT):
            direct.append(path)
        elif path in LAYER_INPUTS:
            direct.append(path)
        elif path == "Cargo.lock":
            reached = lockfile_change(file_at(root, base, path), (root / path).read_bytes(), metadata)
            if reached is None:
                refused.append({"path": path, "reason": "a lockfile change this cannot attribute"})
            else:
                affected |= every_bin(set(reached) & set(packages), packages, reverse)
        elif path.startswith(OS_BASE):
            refused.append({"path": path, "reason": "part of the OS images the base holds"})
        elif (owner := owner_for(path, by_dir)) is not None:
            own = [name for name, src, owns_dir in bin_roots(packages[owner], root)
                   if Path(path) == src or (owns_dir and Path(path).is_relative_to(src.parent))]
            affected |= set(own) if own else every_bin({owner}, packages, reverse)
        elif path in ("Cargo.toml", ".cargo/config.toml", "rust-toolchain.toml"):
            refused.append({"path": path, "reason": "builds every program again"})
        else:
            outside.append(path)

    chosen, server, owner_side = [], [], []
    if affected & unlisted:
        refused.append({"component": "server image", "programs": sorted(affected & unlisted),
                        "reason": "the server image builds it outside os/components.toml, so no layer replaces it"})
    for component in components:
        bins = set(component.get("bins", ()))
        hit = bins & affected
        if not hit:
            continue
        name = component["name"]
        if name in MEDIUM_COMPONENTS or hit & MEDIUM_BINS:
            refused.append({"component": name, "programs": sorted(hit & (MEDIUM_BINS | bins)),
                            "reason": "the live installer runs it, and the medium is sealed"})
            continue
        if component.get("kind") == "boot" or hit & EARLY_BOOT_BINS:
            refused.append({"component": name, "programs": sorted(hit),
                            "reason": "it runs in early boot, which no layer reaches"})
            continue
        if any(f.get("layer", component.get("layer", "")).startswith("dev-")
               for f in component.get("files", [])) or component.get("layer", "").startswith("dev-"):
            # The dev tools themselves: the swap mechanism, not what the proof tests.
            refused.append({"component": name, "programs": sorted(hit),
                            "reason": "it is the layer mechanism itself"})
            continue
        if component.get("scope") != "vm":
            # A session component merges through a machine's own session (the device
            # account); neither guest of this proof runs one the device's steps know.
            refused.append({"component": name, "programs": sorted(hit),
                            "reason": "a session component, and neither guest's session is a tilo-vm account"})
            continue
        chosen.append(name)
        # Each image takes the components os/components.toml places in it (`images`): the
        # installed server the server image's, the owner's dev disk the OS image's.
        images = component.get("images", ["os"])
        if "host" in images:
            server.append(name)
        if "os" in images:
            owner_side.append(name)
    return {"base": base, "components": chosen, "server": server, "owner": owner_side,
            "programs": sorted(affected), "direct": direct, "outside": outside, "refused": refused}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    s = sub.add_parser("select")
    s.add_argument("--base", required=True)
    s.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args()
    print(json.dumps(select(args.root, args.base), indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
