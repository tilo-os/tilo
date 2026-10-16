#!/usr/bin/env bash
# Lint with clippy what builds for wasm32-unknown-unknown: the browser crates
# named on the command line, then what the browser builds, each with the
# packages and features of the script that builds it
# (crates/browser/tilo-browser-runtime/scripts/build.sh). A lint of each crate
# alone never compiles the page runtime with every web app linked into it.
#
#   tools/ci/check-wasm.sh [-p <browser crate>]...
#   CARGO=<any cargo drop-in> tools/ci/check-wasm.sh $(python3 tools/ci/package_sets.py browser)
#
# check.yml's `wasm` job runs it with the selected browser crates. A lint, not
# a build: the link and the web runtime's post-processing need wasm-bindgen
# and binaryen.
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo=${CARGO:-cargo}
target=wasm32-unknown-unknown
# Lints that misfire on wasm32 alone: browser handles are neither Send nor
# Sync, and std::fs::File there holds nothing to drop.
allow=(-A clippy::future_not_send -A clippy::arc_with_non_send_sync -A clippy::drop_non_drop)

clippy() {
  local label=$1
  shift
  bash tools/ci/measure.sh "wasm32 clippy: $label" -- \
    bash scripts/ci-run.sh "cargo clippy (wasm32, $label)" -- \
    "$cargo" clippy --locked --target "$target" "$@" -- -D warnings "${allow[@]}"
}

if [ "$#" -gt 0 ]; then
  clippy "browser crates" "$@"
fi
# scripts/build.sh desktop: the page runtime with every web app registered.
clippy "web desktop" --lib -p tilo-web-desktop
# scripts/build.sh runtime: the page runtime an app bundle links.
clippy "page runtime" --lib -p tilo-browser-runtime
# Private image-only worker kernel.
clippy "image compute worker" --lib -p tilo-browser-compute
