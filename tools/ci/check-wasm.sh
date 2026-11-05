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
# Run it with the browser crates. A lint, not
# a build: the link and the web runtime's post-processing need wasm-bindgen
# and binaryen.
#
# Every check runs even after one fails. A red in the first set used to be the
# only thing a run said about wasm32, so a red in a later check — a crate the
# browser build links, not one of the browser crates — stayed invisible until
# the first was fixed. The script still exits non-zero when any check fails.
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo=${CARGO:-cargo}
target=wasm32-unknown-unknown
# Lints that misfire on wasm32 alone (package_sets.WASM_CLIPPY_ALLOW, which the
# check and clippy families' wasm pass allows too).
allow=()
for lint in $(python3 -c 'import sys; sys.path.insert(0, "tools/ci"); import package_sets; print(*package_sets.WASM_CLIPPY_ALLOW)'); do
  allow+=(-A "$lint")
done

passed=()
failed=()
clippy() {
  local label=$1
  shift
  if bash tools/ci/measure.sh "wasm32 clippy: $label" -- \
    bash scripts/ci-run.sh "cargo clippy (wasm32, $label)" -- \
    "$cargo" clippy --locked --target "$target" "$@" -- -D warnings "${allow[@]}"; then
    passed+=("$label")
  else
    failed+=("$label")
  fi
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

printf 'wasm32 clippy passed: %s\n' "${passed[*]}"
if ((${#failed[@]})); then
  for label in "${failed[@]}"; do
    echo "::error::wasm32 clippy failed: $label"
  done
  printf 'wasm32 clippy failed: %s\n' "${failed[*]}" >&2
  exit 1
fi
