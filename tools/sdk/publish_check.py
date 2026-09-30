#!/usr/bin/env python3
"""What stands between the SDK crate set and crates.io.

The SDK crate set is every workspace crate in the dependency closure of
`tilo-app` and `tilo-browser-runtime`,
including the forks the SDK depends on under their own names (tilo-winit,
tilo-accesskit-winit, tilo-vello*, tilo-automerge-fork). For each one this
prints what publishing still needs:

- `publish = false` (the workspace default);
- a missing `description`, `license` or `repository`;
- path dependencies without a `version`;
- dependencies on crates the workspace patches whose crates.io original the
  SDK cannot build on (see UPSTREAM_SUFFICES): a published crate gets the
  crates.io original, so such a patch must first be published as a `tilo-`
  fork (or upstreamed);
- git dependencies, which crates.io refuses.

Crates marked `publish = true` must have no blockers, and are then packaged
together with `cargo publish --dry-run` (Cargo's local overlay registry
resolves their dependencies on each other): manifests, files and dependency
resolution against crates.io. `--verify` also builds every package from its
packaged sources, in dependency order, against crates.io and the packages
before it; `--report-only` skips the dry run. The packaged `.crate` files stay
in `target/package/tmp-registry/`, where tools/sdk/local_registry.py builds a
registry from them.

A dry run cannot see ownership, and a name another owner holds on crates.io
would resolve to their crate wherever they published our version. The SDK's
names are all `tilo-` names or free (2026-09-28); `--check-names` asks
crates.io again and fails on a taken one.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import pathlib
import subprocess
import sys
import tomllib
import urllib.error
import urllib.request

REPO = pathlib.Path(__file__).resolve().parents[2]
# The SDK's entry crates: apps depend on `tilo-app`, web bundles link
# `tilo-browser-runtime`. A root that is not in the workspace is skipped.
ROOTS = ("tilo-app", "tilo-browser-runtime")


def metadata(repo: pathlib.Path) -> dict:
    output = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked"],
        cwd=repo,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return json.loads(output)


def sdk_closure(meta: dict) -> set[str]:
    packages = {package["id"]: package for package in meta["packages"]}
    nodes = {node["id"]: node for node in meta["resolve"]["nodes"]}
    pending = [
        package_id
        for package_id, package in packages.items()
        if package["name"] in ROOTS and package["source"] is None
    ]
    seen: set[str] = set()
    while pending:
        package_id = pending.pop()
        if package_id in seen:
            continue
        seen.add(package_id)
        for dep in nodes[package_id]["deps"]:
            if any(kind["kind"] in (None, "build") for kind in dep["dep_kinds"]):
                pending.append(dep["pkg"])
    return {packages[package_id]["name"] for package_id in seen}

# Patched crates whose crates.io original a published SDK crate builds on
# unchanged. Every patch in the SDK closure must be listed here or be a
# blocker; the reason says why the original suffices.
UPSTREAM_SUFFICES = {
    "accesskit": "dependency trim (zbus's async-io backend follows a feature); same API",
    "accesskit_atspi_common": "dependency trim; the crates.io release, patched only to stay one copy",
    "accesskit_consumer": "dependency trim; the crates.io release, patched only to stay one copy",
    "accesskit_unix": "dependency trim (zbus's async-io backend follows a feature); same API",
    "atspi": "dependency trim (async-io or tokio selects zbus's backend); same API",
    "atspi-common": "dependency trim; same API",
    "atspi-proxies": "dependency trim; same API",
    "genawaiter-proc-macro": "drops the unmaintained proc-macro-error; same macro output",
    "libspa-sys": "Fedora 44 PipeWire header fix; only the optional `pipewire` feature builds it",
    "rustls-native-certs": "Tilo OS's single trust bundle; the original's discovery is right elsewhere",
    "wayland-backend": "a compositor (server) fix; SDK apps are Wayland clients",
    "web-transport-quinn": "adds a server entry point for the host's shared HTTP/3 endpoint "
    "and follows the workspace's kio; tilo-net's client uses neither",
    "wgpu": "a lost device is reported from `poll` instead of panicking; an app on the "
    "release panics on a GPU reset as before",
    "wgpu-hal": "Metal presentation observer (only the optional `presentation-feedback` "
    "feature uses it), the Windows DX12 shader cache and a Vulkan device-loss switch only tests "
    "set; SDK apps use none of them",
    "workspace-hack": "hakari's feature union for this workspace; crates.io's "
    "workspace-hack 0.1.0 is its empty placeholder",
}

# The forks the SDK depends on under their own names. They sit outside the
# workspace (vendored sources are neither linted nor formatted as Tilo code),
# so the dry run makes them members for its duration: Cargo packages several
# crates together only within one workspace.
FORKS = (
    "vendor/winit",
    "vendor/accesskit_winit",
    "vendor/vello",
    "vendor/vello_encoding",
    "vendor/vello_shaders",
    "vendor/automerge",
)


def blockers(package: dict, patched: set[str], local: set[str]) -> list[str]:
    found = []
    if package.get("publish") == []:
        found.append("publish = false")
    for field in ("description", "license", "repository"):
        if not package.get(field):
            found.append(f"no {field}")
    for dep in package["dependencies"]:
        if dep["kind"] == "dev":
            continue
        name = dep["name"]
        if dep.get("path") and dep["req"] == "*":
            found.append(f"path dependency {name} has no version")
        if name in patched and name not in UPSTREAM_SUFFICES:
            found.append(f"uses patched {name}")
        if dep.get("source", "") and dep["source"].startswith("git+"):
            found.append(f"git dependency {name}")
        if name in local and not dep.get("path"):
            found.append(f"workspace dependency {name} is not by path")
    return sorted(set(found))


def taken_names(names: list[str]) -> set[str]:
    taken = set()
    for name in names:
        request = urllib.request.Request(
            f"https://crates.io/api/v1/crates/{name}",
            headers={"User-Agent": "tilo-publish-check (https://code.tilo.so/tilo/tilo)"},
        )
        try:
            with urllib.request.urlopen(request, timeout=20) as response:
                owners = json.load(response)
        except urllib.error.HTTPError as error:
            if error.code == 404:
                continue
            raise
        if owners.get("crate"):
            taken.add(name)
    return taken


@contextlib.contextmanager
def forks_as_members():
    """Make FORKS workspace members until the block ends. As members their
    dev-dependencies join the lock file, which is restored afterwards too."""
    manifest = REPO / "Cargo.toml"
    lock = REPO / "Cargo.lock"
    original = manifest.read_text()
    original_lock = lock.read_text()
    text = original
    for fork in FORKS:
        entry = f'    "{fork}",\n'
        if entry not in text:
            raise SystemExit(f"{fork} is not in the workspace's exclude list")
        text = text.replace(entry, "", 1)
    added = "".join(f'    "{fork}",\n' for fork in FORKS)
    text = text.replace("\nmembers = [\n", "\nmembers = [\n" + added, 1)
    manifest.write_text(text)
    try:
        yield
    finally:
        manifest.write_text(original)
        lock.write_text(original_lock)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--report-only", action="store_true", help="skip the dry run")
    parser.add_argument(
        "--verify", action="store_true", help="also build every package (slow)"
    )
    parser.add_argument(
        "--check-names", action="store_true", help="ask crates.io which names are taken"
    )
    args = parser.parse_args()
    meta = metadata(REPO)
    closure = sdk_closure(meta)
    root = tomllib.loads((REPO / "Cargo.toml").read_text())
    patched = set(root.get("patch", {}).get("crates-io", {}))
    local_packages = {
        package["name"]: package
        for package in meta["packages"]
        if package["source"] is None and package["name"] in closure
    }
    # A published manifest names its optional path dependencies too, even
    # ones no SDK feature turns on here: each must be published as well.
    every_local = {p["name"]: p for p in meta["packages"] if p["source"] is None}
    pending = list(local_packages)
    while pending:
        for dep in every_local[pending.pop()]["dependencies"]:
            name = dep["name"]
            if dep["kind"] != "dev" and dep.get("path") and name not in local_packages:
                local_packages[name] = every_local[name]
                pending.append(name)
    sdk = {name: package for name, package in local_packages.items() if name not in patched}
    publishable, failing = [], []
    print(f"SDK crate set: {len(sdk)} workspace crates ({len(closure)} packages in the closure)")
    for name in sorted(sdk):
        package = sdk[name]
        found = blockers(package, patched, set(sdk))
        marked = package.get("publish") != []
        if marked and not found:
            publishable.append(name)
            print(f"  ready    {name}")
        elif marked:
            failing.append(name)
            print(f"  BLOCKED  {name} (marked publish): {'; '.join(found)}")
        else:
            print(f"  pending  {name}: {'; '.join(found)}")
    in_closure = sorted(name for name in patched if name in closure)
    unlisted = [name for name in in_closure if name not in UPSTREAM_SUFFICES]
    print("Patched crates a published SDK builds on the crates.io original of:")
    for name in in_closure:
        if name in UPSTREAM_SUFFICES:
            print(f"  {name}: {UPSTREAM_SUFFICES[name]}")
    if unlisted:
        print(f"Patched crates the SDK needs as published forks: {', '.join(unlisted)}")
    taken = taken_names(sorted(sdk)) if args.check_names else set()
    if taken:
        failing.extend(sorted(taken))
        print(
            f"Names taken on crates.io, to publish as tilo-* first ({len(taken)}): "
            + ", ".join(sorted(taken))
        )
    if failing:
        # A report lists what blocks publication; only a publishing run fails on it.
        level = "note" if args.report_only else "error"
        print(f"{level}: marked publishable but blocked: {', '.join(failing)}", file=sys.stderr)
        if not args.report_only:
            return 1
    if args.report_only or not publishable:
        return 0
    # Not --locked: the forks' dev-dependencies join the (restored) lock file.
    command = ["cargo", "publish", "--dry-run", "--allow-dirty"]
    if not args.verify:
        command.append("--no-verify")
    for name in publishable:
        command += ["-p", name]
    print("$ " + " ".join(command), flush=True)
    with forks_as_members():
        return subprocess.run(command, cwd=REPO, check=False).returncode


if __name__ == "__main__":
    sys.exit(main())
