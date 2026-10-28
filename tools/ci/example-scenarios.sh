#!/usr/bin/env bash
# Run the saved headless scenarios of every example app among the selected
# packages through `tilo-app scenario run --audit`: the production binary of
# each example, driven by its tests/headless-actions.json (and
# tests/scenarios/*.json), in private XDG state, with the native audit. When
# tilo-app-cli is selected (it owns the scenario runner and embeds the
# examples, so a change to either selects it), every example runs.
#
#   tools/ci/example-scenarios.sh -p example-counter -p tilo-app ...
#
# t.toml's `example-scenarios` check runs it under tools/test-env.sh. An example with no saved scenario fails: every example
# ships one (docs/examples.md).
set -euo pipefail
cd "$(dirname "$0")/../.."

selected=" $* "
apps=()
packages=()
for manifest in examples/apps/*/Cargo.toml; do
  package=$(sed -n 's/^name = "\(.*\)"$/\1/p' "$manifest" | head -n 1)
  if [[ "$selected" == *" -p tilo-app-cli "* || "$selected" == *" -p $package "* ]]; then
    apps+=("$(dirname "$manifest")")
    packages+=(-p "$package")
  fi
done
if [[ ${#apps[@]} -eq 0 ]]; then
  echo "no example app selected"
  exit 0
fi

cargo build --locked --quiet -p tilo-app-cli --bin tilo-app
tilo_app=${CARGO_TARGET_DIR:-$PWD/target}/debug/tilo-app
# Examples share the same dependency feature set. Link the selected binaries
# in one Cargo graph instead of serializing ten independent builds. Each
# scenario still performs its normal Cargo freshness check and native audit.
# A single selected example keeps the original path without an extra check.
if [[ ${#apps[@]} -gt 1 ]]; then
  cargo build --locked --bins "${packages[@]}"
fi
failed=()
for app in "${apps[@]}"; do
  echo "== ${app}"
  "$tilo_app" scenario run --audit --app "$app" || failed+=("$app")
done
if [[ ${#failed[@]} -gt 0 ]]; then
  echo "saved scenarios failed: ${failed[*]}" >&2
  exit 1
fi
