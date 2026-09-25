#!/usr/bin/env bash
# Build the Markdown format provider. Cargo reads `.cargo/config.toml` from the
# working directory, so the build runs from this crate's directory.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
cargo build --target wasm32-unknown-unknown --release --locked "$@"
