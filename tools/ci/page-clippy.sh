#!/usr/bin/env bash
# The Clippy the page's crates are linted with: writes the page's configuration and runs the cargo
# command it is given under it.
#
#   page-clippy.sh cargo clippy --target wasm32-unknown-unknown ...
#
# t's `clippy-page` family names it as its wrapper. The configuration bans std's clocks, which
# panic on wasm32-unknown-unknown: the root clippy.toml cannot ban them for every target, so the
# page's pass reads its own (tools/ci/wasm_clippy_conf.py says why). The directory is fixed, so an
# unchanged configuration keeps Cargo's lint cache; it lies in the build directory, because no
# other job shares it.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
conf_dir=$(realpath -m "${CARGO_TARGET_DIR:-$root/target}/wasm-clippy")
python3 "$root/tools/ci/wasm_clippy_conf.py" "$conf_dir"
export CLIPPY_CONF_DIR=$conf_dir
exec "$@"
