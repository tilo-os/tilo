#!/usr/bin/env bash
# Refresh vendor/git: copies of the `[patch.crates-io]` git forks, which
# .cargo/config.toml substitutes for their private repositories.
#
# Run after changing a fork revision in Cargo.toml (and in .cargo/config.toml's
# `[source.fork-*]` entries). Fetching the forks needs access to code.tilo.so.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

mkdir -p target
scratch=$(mktemp -d target/vendor-git.XXXXXX)
trap 'rm -rf "$scratch"' EXIT

# `cargo vendor` ignores `[source]` replacement unless told otherwise, so this
# reads the forks themselves rather than the copies being refreshed.
cargo vendor --quiet --locked --versioned-dirs "$scratch" >/dev/null

rm -rf vendor/git
mkdir -p vendor/git
python3 - "$scratch" <<'PY'
import pathlib
import shutil
import sys
import tomllib

scratch = pathlib.Path(sys.argv[1])
lock = tomllib.loads(pathlib.Path("Cargo.lock").read_text())
for package in lock["package"]:
    if package.get("source", "").startswith("git+"):
        name = f"{package['name']}-{package['version']}"
        shutil.copytree(scratch / name, pathlib.Path("vendor/git") / name)
        print(f"vendor/git/{name}")
PY

# Apps outside the workspace use these copies by path or from Tilo's git
# repository, which follow the paths inside each manifest.
python3 tools/relink-vendored-forks.py vendor/git
