#!/usr/bin/env python3
"""The wrapper of t's `check` and `clippy` families (t.toml): runs the cargo
command t hands it, and first builds the browser crates among its packages for
wasm32-unknown-unknown as the web build does. A clippy run also lints each
package with the features its own tests are verified with, and the default
build of those whose tested features are not what they ship.

  t_cargo_compile.py cargo check|clippy [OPTIONS] -p NAME ... [-- TRAILING]
  t_cargo_compile.py cargo clippy [OPTIONS] --workspace [-- TRAILING]

Code under `cfg(target_arch = "wasm32")` compiles only for that target, so a
native check of a browser crate passed while the wasm build broke. For each
package of tools/ci/package_sets.py's `wasm` set it names, this runs the same
command for wasm32 on the package's library (its tests are native) with the
features the page runtime gives it, then the native command unchanged. The
wasm pass goes first: t answers a package's check as soon as its native units
have compiled. Any other command passes through unchanged.

The native command also selects tools/ci/workspace_unify.py's package, which builds nothing and
makes every member resolve its features as in the whole workspace, so that jobs of different
packages build each member as the same unit; the wasm and default-build passes don't, as they
build what ships.

Clippy gets tools/ci/package_features.py's selection: each named package builds
with its NATIVE_TEST_FEATURES, so code behind those features is linted at all.
Named packages get them through workspace-unify, which carries them; `--workspace`
gets them as `--features` (all of them). Never both: `--features tilo-media/decode`
also turns on every selected package's optional dependency named tilo-media, and
gives tilo-media those features where it is built for the host as well, so a
member built beside tilo-media would be another unit. Each DEFAULT_BUILDS_ALSO_CHECKED
package named is linted once more as it ships, before the native command. That
pass's errors fail the package as any other's; its artifacts are not passed on,
since t answers a package as passed once each of its units has an artifact, and
the default build's would answer it before the tested build ran.
"""
from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import package_features  # noqa: E402
import package_sets  # noqa: E402
import workspace_unify  # noqa: E402

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


def is_clippy(argv: list[str]) -> bool:
    return len(argv) >= 2 and argv[0] == "cargo" and argv[1] == "clippy"


def linted(argv: list[str]) -> tuple[list[str], bool]:
    """The packages a clippy command names, and whether it names the whole workspace."""
    packages, options, _ = split(argv[2:])
    return packages, "--workspace" in options


def with_test_features(argv: list[str]) -> list[str]:
    """The clippy command `argv` with the features its packages' own tests are verified with."""
    if not is_clippy(argv):
        return argv
    packages, workspace = linted(argv)
    features = package_features.arguments(list(package_features.NATIVE_TEST_FEATURES) if workspace else packages)
    if not features:
        return argv
    end = argv.index("--") if "--" in argv else len(argv)
    return [*argv[:end], *features, *argv[end:]]


def unified(argv: list[str]) -> list[str]:
    """The native command `argv` with tools/ci/workspace_unify.py's package selected too, so every
    member it builds resolves its features as in the whole workspace, its tests' included: one
    unit for every job. The whole workspace's gets its tests' features as `--features` instead."""
    if len(argv) < 2 or argv[0] != "cargo" or argv[1] not in COMPILE:
        return argv
    packages, workspace = linted(argv)
    if workspace or not packages:
        return with_test_features(argv)
    end = argv.index("--") if "--" in argv else len(argv)
    return [*argv[:end], "-p", workspace_unify.NAME, *argv[end:]]


def default_build_passes(argv: list[str]) -> list[list[str]]:
    """The clippy commands over the default build of each package `argv` names whose tested
    features are not the build it ships."""
    if not is_clippy(argv):
        return []
    packages, workspace = linted(argv)
    _, options, trailing = split(argv[2:])
    options = [arg for arg in options if arg != "--workspace"]
    chosen = sorted(package_features.DEFAULT_BUILDS_ALSO_CHECKED) if workspace else package_features.default_builds(packages)
    return [[*argv[:2], *options, "-p", name, *(["--", *trailing] if trailing else [])] for name in chosen]


def without_artifacts(command: list[str]) -> int:
    """Run `command`, passing on its output but cargo's artifact messages."""
    sys.stdout.flush()
    with subprocess.Popen(command, stdout=subprocess.PIPE, text=True) as process:
        assert process.stdout is not None
        for line in process.stdout:
            if line.startswith("{"):
                try:
                    if json.loads(line).get("reason") == "compiler-artifact":
                        continue
                except json.JSONDecodeError:
                    pass
            sys.stdout.write(line)
    sys.stdout.flush()
    return process.returncode


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
    codes = [subprocess.run(command, check=False).returncode for command in passes]
    sys.stdout.flush()
    codes += [without_artifacts(command) for command in default_build_passes(argv)]
    codes.append(subprocess.run(unified(argv), check=False).returncode)
    return next((code for code in codes if code), 0)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
