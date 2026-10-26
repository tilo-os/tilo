#!/usr/bin/env python3
"""The workspace's package sets that t's checks lint and build.

  package_sets.py android   apps with an `<app>-android` example
  package_sets.py browser   crates that build for the browser (wasm-bindgen)
  package_sets.py wasm      those, and the crates the page runtime builds for the
                            browser through them (t_cargo_compile.py's wasm pass)

prints the `-p NAME` arguments of one set, over every workspace package.
tools/ci/t_toml.py keys the checks of t.toml by the same sets.
"""
from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys

# Example apps (tools/ci/build-web-examples.sh builds the web-capable ones).
EXAMPLE_APPS = "examples/apps/"
# Patched vendored crates with test suites of their own. The workspace builds
# them only as path dependencies, so its tests never run theirs;
# tools/ci/test-vendored-crates.sh does (t.toml's `vendored` check).
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
# that build each as tilo-browser-runtime's dependency on it does.
WASM_ALSO = {
    "tilo-browser-peer": ("--no-default-features", "--features", "toolkit"),
    # The frame contract is shared by the browser call path; check it for wasm32
    # even when no browser crate is selected.
    "encoded-frame": (),
}
# Lints that misfire on wasm32 alone: browser handles are neither Send nor
# Sync, and std::fs::File there holds nothing to drop.
WASM_CLIPPY_ALLOW = ("clippy::future_not_send", "clippy::arc_with_non_send_sync", "clippy::drop_non_drop")


def wasm_crates(packages: dict[str, dict]) -> set[str]:
    """Crates whose check and clippy also build their library for wasm32-unknown-unknown:
    the browser crates, and those the page runtime compiles for the browser through them."""
    return browser_crates(packages) | (WASM_ALSO.keys() & packages.keys())


SETS = {"android": android_apps, "browser": browser_crates, "wasm": wasm_crates}


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
