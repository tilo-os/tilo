#!/usr/bin/env bash
# Run the saved headless scenarios of every example app among the selected
# packages through `tilolang scenario run --audit`: the production binary of
# each example, driven by its tests/headless-actions.json (and
# tests/scenarios/*.json), in private XDG state, with the native audit. When
# tilo-app-builder is selected (it owns the scenario runner and embeds the
# examples, so a change to either selects it), every example runs.
#
#   tools/ci/example-scenarios.sh -p example-counter -p tilo-app ...
#
# An example with no saved scenario fails: every example ships one.
set -euo pipefail
cd "$(dirname "$0")/../.."
repo=$PWD

selected=" $* "
apps=()
packages=()
for manifest in examples/apps/*/Cargo.toml; do
  package=$(sed -n 's/^name = "\(.*\)"$/\1/p' "$manifest" | head -n 1)
  if [[ "$selected" == *" -p tilo-app-builder "* || "$selected" == *" -p $package "* ]]; then
    apps+=("$(dirname "$manifest")")
    packages+=(-p "$package")
  fi
done
if [[ ${#apps[@]} -eq 0 ]]; then
  echo "no example app selected"
  exit 0
fi

# The builds get the caller's environment and the inputs every job's build shares
# (tools/test-env.sh --build); the scenarios get the test environment (a fresh HOME, lavapipe, the
# fonts, a private audio endpoint). A variable in a build's environment keys every build script's
# and proc macro's cache entry by it, and the test environment's differ per job, so a build that
# carried one would share no cache with any other job's. The second start names the pass the
# builds made as an argument, so the environment holds no marker that would key the scenario
# runner's own app builds apart from the builds.
if [[ ${1:-} != --built ]]; then
  build=(bash "$repo/tools/test-env.sh" --build)
  "${build[@]}" cargo build --locked --quiet -p tilolang-cli --bin tilolang
  # Examples share the same dependency feature set. Link the selected binaries
  # in one Cargo graph instead of serializing ten independent builds. Each
  # scenario still performs its normal Cargo freshness check and native audit.
  # A single selected example keeps the original path without an extra check.
  if [[ ${#apps[@]} -gt 1 ]]; then
    "${build[@]}" cargo build --locked --bins "${packages[@]}"
  fi
  exec bash "$repo/tools/test-env.sh" bash "$repo/tools/ci/example-scenarios.sh" --built "$@"
fi
shift

tilolang=${CARGO_TARGET_DIR:-$PWD/target}/debug/tilolang
failed=()
for app in "${apps[@]}"; do
  echo "== ${app}"
  "$tilolang" scenario run --audit --app "$app" || failed+=("$app")
done
if [[ ${#failed[@]} -gt 0 ]]; then
  echo "saved scenarios failed: ${failed[*]}" >&2
  exit 1
fi
