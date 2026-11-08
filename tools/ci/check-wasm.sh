#!/usr/bin/env bash
# Lint (or, with --check, type-check) what builds for wasm32-unknown-unknown:
# the browser crates named on the command line, then what the browser builds,
# each with the packages and features of the script that builds it
# (crates/browser/tilo-browser-runtime/scripts/build.sh). A lint of each crate
# alone never compiles the page runtime with every web app linked into it.
#
#   tools/ci/check-wasm.sh [--check] [-p <browser crate>]...
#   CARGO=<any cargo drop-in> tools/ci/check-wasm.sh $(python3 tools/ci/package_sets.py browser)
#
# `--check` type-checks the same set instead of linting it. check.yml's `web`
# job runs the page builds on every head of main, where the cheaper verdict is
# what fits; the lint of every crate stays with the window's `wasm` job and
# nightly. Warnings still fail a check through the job's
# CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS.
#
# Run it with the browser crates. Not a build: the link and the web runtime's
# post-processing need wasm-bindgen and binaryen.
#
# Every check runs even after one fails. A red in the first set used to be the
# only thing a run said about wasm32, so a red in a later check — a crate the
# browser build links, not one of the browser crates — stayed invisible until
# the first was fixed. The script still exits non-zero when any check fails.
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo=${CARGO:-cargo}
target=wasm32-unknown-unknown
mode=clippy
if [ "${1:-}" = --check ]; then
  mode=check
  shift
fi
passed=()
failed=()
verify() {
  local label=$1
  shift
  local command=("$cargo" "$mode" --locked --target "$target" "$@")
  if [ "$mode" = clippy ]; then
    command+=(-- -D warnings)
  fi
  if bash tools/ci/measure.sh "wasm32 $mode: $label" -- \
    bash scripts/ci-run.sh "cargo $mode (wasm32, $label)" -- \
    "${command[@]}"; then
    passed+=("$label")
  else
    failed+=("$label")
  fi
}

if [ "$#" -gt 0 ]; then
  verify "browser crates" "$@"
fi
# scripts/build.sh desktop: the page runtime with every web app registered.
verify "web desktop" --lib -p tilo-web-desktop
# scripts/build.sh runtime: the page runtime an app bundle links.
verify "page runtime" --lib -p tilo-browser-runtime
# Private image-only worker kernel.
verify "image compute worker" --lib -p tilo-browser-compute

printf 'wasm32 %s passed: %s\n' "$mode" "${passed[*]}"
if ((${#failed[@]})); then
  for label in "${failed[@]}"; do
    echo "::error::wasm32 $mode failed: $label"
  done
  printf 'wasm32 %s failed: %s\n' "$mode" "${failed[*]}" >&2
  exit 1
fi
