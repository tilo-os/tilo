#!/usr/bin/env bash
# A type check of packages for a Mac, on a Linux node, so the code a Mac runs is kept compiling
# while no Mac is a build node:
#
#   macos-check.sh TARGET CARGO-ARGUMENTS...   e.g. aarch64-apple-darwin -p tilo-app
#
# Zig compiles the C the packages' build scripts build (ring, SQLite, zstd) against Darwin's C
# library headers, as for the `tilo` command's Mac archives (tools/ci/publish/cli-release.sh);
# install-cli-toolchain.sh sets it and the Rust target up. A check links nothing, so no framework
# is needed.
set -euo pipefail

target=${1:?usage: macos-check.sh TARGET CARGO-ARGUMENTS...}
shift
case $target in
  aarch64-apple-darwin | x86_64-apple-darwin) ;;
  *) echo "macos-check.sh checks for a Mac, not for $target" >&2; exit 2 ;;
esac
command -v zig >/dev/null || { echo 'no zig: install-cli-toolchain.sh sets it up' >&2; exit 1; }
repo_root=$(cd -- "$(dirname -- "$0")/../.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT

export MACOSX_DEPLOYMENT_TARGET=11.0 TILO_ZIG_TARGET=${target%%-*}-macos.11.0
export "CC_${target//-/_}=$repo_root/tools/ci/zig-cc.sh"
export ZIG_GLOBAL_CACHE_DIR=$scratch/zig-cache ZIG_LOCAL_CACHE_DIR=$scratch/zig-cache
cargo check --locked --keep-going --target "$target" "$@"
