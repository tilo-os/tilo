#!/usr/bin/env bash
# The SDKs' native part: the Python extension `tilo._native` (sdk/python/native) and the Node
# addon `tilo-sdk-native` (sdk/typescript/native), the one Rust client of Tilo's API bound for
# each, built and then driven against a real Tilo host.
#
#   sdk-native.sh build <dir>   Build both with the api-on-relay harness, into <dir>:
#                                 <dir>/python/tilo/_native.abi3.so
#                                 <dir>/typescript/tilo-sdk-native.<platform>.node
#                                 <dir>/{python/tilo,typescript}/tilo-automerge-worker
#                                 <dir>/api-on-relay
#                                 <dir>/font-catalogs (the fonts its app workers set text in,
#                                   found above the program as in the build directory)
#                               Needs cargo (the sdk-native-build check, in the builder image).
#                               The dev profile: it shares every unit with the harness, which
#                               links the same client. The binaries a release ships are
#                               maturin's and napi's release builds of the same crates.
#   sdk-native.sh test <dir>    Run each SDK's native test with what `build` wrote: the harness
#                               serves a relay with its app workers on loopback and hands the
#                               tests a key file of a device of its person (TILO_KEY_FILE).
#                               Needs uv with the Python SDK's environment and bun (the
#                               sdk-native check, in the ci-sdk image; locally, the tools on
#                               PATH and the network uv may need).
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
usage='usage: sdk-native.sh build <dir> | test <dir>'
mode=${1:-}
out=${2:?$usage}

# The npm platform name of the binary this machine builds (sdk/typescript/native/npm/<it>/).
platform() {
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) echo linux-x64-gnu ;;
    *) echo "sdk-native.sh: no npm platform package for $(uname -s)-$(uname -m) yet" >&2; exit 1 ;;
  esac
}

build() {
  cd "$root"
  cargo build --locked -p tilo-sdk-python-native -p tilo-sdk-node-native
  cargo build --locked -p tilo-automerge-isolate --bin tilo-automerge-worker
  cargo build --locked -p tilo-agent-participant --example api-on-relay
  local target=${CARGO_TARGET_DIR:-$root/target}
  rm -rf "$out"
  mkdir -p "$out/python/tilo" "$out/typescript"
  cp "$target/debug/lib_native.so" "$out/python/tilo/_native.abi3.so"
  cp "$target/debug/libtilo_sdk_node_native.so" "$out/typescript/tilo-sdk-native.$(platform).node"
  cp "$target/debug/tilo-automerge-worker" "$out/python/tilo/"
  cp "$target/debug/tilo-automerge-worker" "$out/typescript/"
  cp "$target/debug/examples/api-on-relay" "$out/api-on-relay"
  cp -a "$target/debug/font-catalogs" "$out/font-catalogs"
}

scratch=
run_tests() {
  local work
  scratch=$(mktemp -d "${TMPDIR:-/tmp}/sdk-native.XXXXXX")
  trap 'rm -rf -- "${scratch:-}"' EXIT
  # The ci-sdk image's environments, copied since a job runs in it read-only.
  if [[ -d /opt/tilo-sdk/uv-cache ]]; then
    export UV_OFFLINE=1 UV_PYTHON_DOWNLOADS=never
    export UV_CACHE_DIR=$scratch/uv-cache UV_PROJECT_ENVIRONMENT=$scratch/python-venv
    cp -a /opt/tilo-sdk/uv-cache "$UV_CACHE_DIR"
    cp -a /opt/tilo-sdk/python-venv "$UV_PROJECT_ENVIRONMENT"
  fi
  # Each SDK in a copy of its tree, the built binary linked in where a package carries it.
  local binaries
  binaries=$(cd -- "$out" && pwd)
  work=$scratch/sdk
  mkdir -p "$work"
  tar -C "$root/sdk" --exclude=node_modules --exclude=.venv -cf - python typescript/native \
    | tar -C "$work" -xf -
  ln -s "$binaries"/python/tilo/* "$work/python/src/tilo/"
  ln -s "$binaries"/typescript/* "$work/typescript/native/"

  "$binaries/api-on-relay" -- bash -euo pipefail -c '
    cd "$1/python"
    uv run --frozen --extra dev python -m pytest -q -p no:cacheprovider tests/test_native.py
    cd "$1/typescript/native"
    bun test
  ' sdk-native "$work"
}

case $mode in
  build) build ;;
  test) run_tests ;;
  *) echo "$usage" >&2; exit 2 ;;
esac
