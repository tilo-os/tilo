#!/usr/bin/env python3
"""The wrapper of t's `check` and `clippy` families (t.toml): runs the cargo
command t hands it, and first builds the browser crates among its packages for
wasm32-unknown-unknown as the web build does.

  t_cargo_compile.py cargo check|clippy [OPTIONS] -p NAME ... [-- TRAILING]

Code under `cfg(target_arch = "wasm32")` compiles only for that target, so a
native check of a browser crate passed while the wasm build broke. For each
package of tools/ci/package_sets.py's `wasm` set it names, this runs the same
command for wasm32 on the package's library (its tests are native) with the
features the page runtime gives it, then the native command unchanged. The
wasm pass goes first: t answers a package's check as soon as its native units
have compiled. Any other command passes through unchanged.
"""
from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import package_sets  # noqa: E402

TARGET = "wasm32-unknown-unknown"
COMPILE = ("check", "clippy")
# Target selectors of the native command that the wasm pass replaces with `--lib`.
TARGET_SELECTORS = {"--all-targets", "--lib", "--bins", "--examples", "--tests", "--benches"}


def split(argv: list[str]) -> tuple[list[str], list[str], list[str]]:
    """The named packages, the other options, and what follows `--`."""
    packages: list[str] = []
    options: list[str] = []
    trailing: list[str] = []
    index = 0
    while index < len(argv):
        arg = argv[index]
        if arg == "--":
            trailing = argv[index + 1:]
            break
        if arg in ("-p", "--package") and index + 1 < len(argv):
            packages.append(argv[index + 1])
            index += 2
            continue
        if arg.startswith("--package="):
            packages.append(arg.removeprefix("--package="))
        else:
            options.append(arg)
        index += 1
    return packages, options, trailing


def wasm_passes(argv: list[str], wasm: set[str]) -> list[list[str]]:
    """The wasm32 commands to run before `argv` (`cargo <tool> ...`): one for the browser
    crates it names that build with their own features, one for each that needs others."""
    if len(argv) < 2 or argv[0] != "cargo" or argv[1] not in COMPILE:
        return []
    packages, options, trailing = split(argv[2:])
    chosen = [name for name in packages if name in wasm]
    if not chosen:
        return []
    options = [arg for arg in options if arg not in TARGET_SELECTORS]
    if argv[1] == "clippy":
        trailing = [*trailing, *(arg for lint in package_sets.WASM_CLIPPY_ALLOW for arg in ("-A", lint))]
    groups: list[tuple[list[str], tuple[str, ...]]] = []
    plain = [name for name in chosen if name not in package_sets.WASM_ALSO]
    if plain:
        groups.append((plain, ()))
    groups += [([name], package_sets.WASM_ALSO[name]) for name in chosen if name in package_sets.WASM_ALSO]
    return [
        [*argv[:2], *options, "--target", TARGET, "--lib",
         *(arg for name in names for arg in ("-p", name)), *features,
         *(["--", *trailing] if trailing else [])]
        for names, features in groups
    ]


def wasm_set() -> set[str]:
    metadata = json.loads(subprocess.run(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        check=True, capture_output=True, text=True).stdout)
    return package_sets.wasm_crates({package["name"]: package for package in metadata["packages"]})


def main(argv: list[str]) -> int:
    if not argv:
        print("usage: t_cargo_compile.py cargo <command> [ARGS]...", file=sys.stderr)
        return 2
    passes = wasm_passes(argv, wasm_set()) if len(argv) > 1 and argv[1] in COMPILE else []
    # Every pass runs, as `--keep-going` does: one failing still reports the others.
    codes = [subprocess.run(command, check=False).returncode for command in [*passes, argv]]
    return next((code for code in codes if code), 0)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
