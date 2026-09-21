#!/usr/bin/env bash
set -euo pipefail

if [[ ${1:-} == "--help" ]]; then
  echo "Usage: crates/doc/format-host/tests/run-component-integration.sh"
  echo "Build the Markdown WASM component and run format-host component integration tests."
  exit 0
fi
if [[ $# -ne 0 ]]; then
  echo "Usage: crates/doc/format-host/tests/run-component-integration.sh" >&2
  exit 2
fi

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

if ! rustup target list --installed | grep -Fxq wasm32-wasip2; then
  echo "format-host component tests require: rustup target add wasm32-wasip2" >&2
  exit 1
fi

cargo build \
  --manifest-path crates/doc/qdoc-codec-markdown-wasm/Cargo.toml \
  --target wasm32-wasip2 \
  --release \
  --locked

cargo test -p format-host --test integration -- \
  --ignored --nocapture --test-threads 1
