#!/usr/bin/env python3
"""The workspace's package sets that CI lints and builds.

  package_sets.py android   apps with an `<app>-android` example
  package_sets.py browser   crates that build for the browser (wasm-bindgen)
  package_sets.py wasm      those, and the crates the page runtime builds for the
                            browser through them

prints the `-p NAME` arguments of one set, over every workspace package.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import re
import subprocess
import sys

# Example apps (tools/ci/build-web-examples.sh builds the web-capable ones).
EXAMPLE_APPS = "examples/apps/"
# Patched vendored crates with test suites of their own. The workspace builds
# them only as path dependencies, so its tests never run theirs;
# tools/ci/test-vendored-crates.sh does.
VENDORED_TEST_CRATES = ("vendor/automerge", "vendor/vello_encoding")


def android_apps(packages: dict[str, dict]) -> set[str]:
    """Apps that opt into the Android host by declaring an `<app>-android` example."""
    return {
        name for name, package in packages.items()
        if any(target.get("name") == f"{name}-android" and "example" in target.get("kind", ())
               for target in package.get("targets", ()))
    }


def browser_crates(packages: dict[str, dict]) -> set[str]:
    """Crates that build for the browser: they depend on `wasm-bindgen`."""
    return {
        name for name, package in packages.items()
        if any(dependency.get("name") == "wasm-bindgen" and dependency.get("kind") is None
               for dependency in package.get("dependencies", ()))
    }


def example_apps(packages: dict[str, dict], root: Path) -> set[str]:
    """Packages under examples/apps/."""
    base = root.resolve()
    return {
        name for name, package in packages.items()
        if Path(package["manifest_path"]).resolve().parent.relative_to(base).as_posix()
        .startswith(EXAMPLE_APPS)
    }


# Crates the page runtime compiles for the browser that reach wasm-bindgen only
# through others, with the features the web build gives them: the arguments
# that build each as tilo-browser-runtime's dependency on it does. channel-crypto
# is here for its own wasm target table: the getrandom backend that makes a peer
# built without tilo-ui compile belongs to it, so it is built alone to hold that.
# tilo-peer-state is the peer core a page holds its workspaces through, as
# native devices do: its admission and replica core keep building for the
# browser.
WASM_ALSO = {
    "tilo-browser-peer": ("--no-default-features", "--features", "toolkit"),
    "channel-crypto": (),
    "tilo-peer-state": (),
}
# Lints that misfire on wasm32 alone: browser handles are neither Send nor
# Sync, and std::fs::File there holds nothing to drop.
WASM_CLIPPY_ALLOW = ("clippy::future_not_send", "clippy::arc_with_non_send_sync", "clippy::drop_non_drop")


def wasm_crates(packages: dict[str, dict]) -> set[str]:
    """Crates whose check and clippy also build their library for wasm32-unknown-unknown:
    the browser crates, and those the page runtime compiles for the browser through them."""
    return browser_crates(packages) | (WASM_ALSO.keys() & packages.keys())


# What makes a package's code differ between x86_64 and aarch64, so that a check's result on one
# says nothing about the other:
#
#   * a cfg naming a CPU architecture: `cfg(target_arch = "aarch64")`, `cfg!(...)`, `cfg_attr`,
#     `not(...)`. wasm is not one (the wasm checks build its paths, no node runs them), nor is a
#     cfg that also names another OS than Linux (`all(windows, target_arch = "aarch64")`): t's
#     Linux nodes compile neither side of it;
#   * a build script reading CARGO_CFG_TARGET_ARCH for anything but telling wasm32 apart;
#   * `std::env::consts::ARCH` compared with a name or matched: a branch like `cfg!`. Passing it
#     on as data (a record's architecture field, a path component, a log line, a comparison
#     with another record's) is not: the same code runs on both, with the node's name in it;
#   * the architecture's intrinsics or assembly (`std::arch::`, `core::arch::`, `asm!`).
#
# Only a package's own sources count, not those of the crates it depends on: each package's
# checks cover its own code on both, and the standard library's and external crates' per-arch
# code is theirs to test. Nor does a manifest's `[target.'cfg(target_arch = ...)']` table: it
# picks dependencies, which the families build for whichever node they run on.
CPU_ARCH = re.compile(r'\btarget_arch\s*=\s*"(?!wasm)[\w]+"')
OTHER_OS = re.compile(r'\bwindows\b|\btarget_os\s*=\s*"(?!linux")|\btarget_vendor\s*=\s*"apple"')
CFG_START = re.compile(r'\bcfg(?:_attr)?!?\s*\(')
ARCH_ENV = re.compile(r'CARGO_CFG_TARGET_ARCH')
ARCH_BRANCH = re.compile(r'\bconsts::ARCH\s*[!=]=\s*"|"\s*[!=]=\s*(?:std::env::)?consts::ARCH\b'
                         r'|\bmatch\s+(?:std::env::)?consts::ARCH\b')
ARCH_CODE = re.compile(r'\b(?:std|core)::arch::|\basm!\s*\(')


def cfg_predicates(text: str) -> list[str]:
    """The predicate of every `cfg(...)`, `cfg!(...)` and `cfg_attr(...)` in `text`."""
    found = []
    for match in CFG_START.finditer(text):
        depth, start = 1, match.end()
        for index in range(start, len(text)):
            depth += {"(": 1, ")": -1}.get(text[index], 0)
            if depth == 0:
                found.append(text[start:index])
                break
    return found


def wasm_only_arch_env(text: str) -> bool:
    """Whether a build script reads CARGO_CFG_TARGET_ARCH only to compare it with wasm32: right
    away (`.as_deref() == Ok("wasm32")`), or through a `let` whose every use does."""
    for match in ARCH_ENV.finditer(text):
        line_start = text.rfind("\n", 0, match.start()) + 1
        statement = text[line_start:text.find(";", match.end()) + 1]
        if re.search(r'[!=]=\s*Ok\("wasm32"\)', statement):
            continue
        binding = re.match(r'\s*let\s+(\w+)\s*=', statement)
        if not binding:
            return False
        rest = text[line_start + len(statement):]
        uses = re.findall(rf'\b{binding[1]}\b(\s*[!=]=\s*"\w+")?', rest)
        if not uses or any(compared.split('"')[1:2] != ["wasm32"] for compared in uses):
            return False
    return True


def arch_dependent(text: str) -> bool:
    """Whether one Rust file has a code path that differs between x86_64 and aarch64 (see above)."""
    # The substrings first: the patterns alone take seconds over the workspace.
    if "consts::ARCH" in text and ARCH_BRANCH.search(text):
        return True
    if ("arch::" in text or "asm!" in text) and ARCH_CODE.search(text):
        return True
    if "CARGO_CFG_TARGET_ARCH" in text and not wasm_only_arch_env(text):
        return True
    return "target_arch" in text and CPU_ARCH.search(text) is not None and any(
        CPU_ARCH.search(predicate) and not OTHER_OS.search(predicate) for predicate in cfg_predicates(text))


def own_rust_files(directory: Path, others: set[Path]):
    """The Rust files under a package's directory, but those of packages nested in it and of
    build output."""
    for current, subdirectories, files in os.walk(directory):
        here = Path(current)
        subdirectories[:] = [name for name in subdirectories
                             if name != "target" and not name.startswith(".") and here / name not in others]
        for name in files:
            if name.endswith(".rs"):
                yield here / name


def per_arch(packages: dict[str, dict]) -> set[str]:
    """Packages whose own sources differ between x86_64 and aarch64 (see above)."""
    dirs = {name: Path(package["manifest_path"]).resolve().parent for name, package in packages.items()}
    every = set(dirs.values())
    found = set()
    for name, directory in dirs.items():
        targets = {Path(target["src_path"]).resolve() for target in packages[name].get("targets", ())}
        files = {*own_rust_files(directory, every - {directory}), *targets}
        if any(arch_dependent(path.read_text(encoding="utf-8", errors="replace")) for path in files):
            found.add(name)
    return found


SETS = {"android": android_apps, "browser": browser_crates, "wasm": wasm_crates, "per-arch": per_arch}


def main(argv: list[str]) -> int:
    if len(argv) != 1 or argv[0] not in SETS:
        print(f"usage: package_sets.py {'|'.join(SETS)}", file=sys.stderr)
        return 2
    metadata = json.loads(subprocess.run(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        check=True, capture_output=True, text=True).stdout)
    packages = {package["name"]: package for package in metadata["packages"]}
    names = sorted(SETS[argv[0]](packages))
    if not names:
        print(f"no {argv[0]} packages in the workspace", file=sys.stderr)
        return 1
    print(" ".join(f"-p {name}" for name in names))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
