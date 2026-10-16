#!/usr/bin/env bash
set -euo pipefail

if [[ ${1:-} == "--help" ]]; then
  echo "Usage: crates/doc/format-host/tests/run-provider-integration.sh"
  echo "Build the Markdown format provider and run the format-host provider integration tests."
  exit 0
fi
if [[ $# -ne 0 ]]; then
  echo "Usage: crates/doc/format-host/tests/run-provider-integration.sh" >&2
  exit 2
fi

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

if ! rustup target list --installed | grep -Fxq wasm32-unknown-unknown; then
  echo "format-host provider tests require: rustup target add wasm32-unknown-unknown" >&2
  exit 1
fi

crates/doc/tilo-doc-codec-markdown-wasm/build.sh

cargo test -p format-host --test integration --locked -- \
  --include-ignored --nocapture --test-threads 1
