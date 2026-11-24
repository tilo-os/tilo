#!/usr/bin/env bash
# The SDKs' native part: the Python extension `tilo._native` (sdk/python/native) and the Node
# addon `tilo-sdk-native` (sdk/typescript/native), each the line wire's client of this computer's
# daemon bound for its language, built and then driven against a real daemon.
#
#   sdk-native.sh build <dir>   Build both with the daemon and its harness, into <dir>:
#                                 <dir>/python/tilo/_native.abi3.so
#                                 <dir>/typescript/tilo-sdk-native.<platform>.node
#                                 <dir>/{python/tilo,typescript}/tilo-automerge-worker
#                                 <dir>/tilo-runtime (the daemon)
#                                 <dir>/sdk-daemon (tilo-runtime's sdk-daemon example)
#                               Needs cargo (the sdk-native-build check, in the builder image).
#                               The dev profile; the binaries a release ships are maturin's and
#                               napi's release builds of the same crates.
#   sdk-native.sh test <dir>    Run each SDK's end-to-end suite with what `build` wrote:
#                               sdk-daemon starts the daemon in a profile of its own and runs the
#                               suites with TILO_RUNTIME_PROFILE naming it, so each SDK's
#                               connection reaches it as the person.
#                               Needs uv with the Python SDK's environment and bun with the
#                               TypeScript SDK's packages (the sdk-native check, in the ci-sdk
#                               image; locally, the tools on PATH and the network they may need).
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
  cargo build --locked -p tilo-daemon --bin tilo-runtime -p tilo-runtime --example sdk-daemon
  local target=${CARGO_TARGET_DIR:-$root/target}
  rm -rf "$out"
  mkdir -p "$out/python/tilo" "$out/typescript"
  cp "$target/debug/lib_native.so" "$out/python/tilo/_native.abi3.so"
  cp "$target/debug/libtilo_sdk_node_native.so" "$out/typescript/tilo-sdk-native.$(platform).node"
  cp "$target/debug/tilo-automerge-worker" "$out/python/tilo/"
  cp "$target/debug/tilo-automerge-worker" "$out/typescript/"
  cp "$target/debug/tilo-runtime" "$out/tilo-runtime"
  cp "$target/debug/examples/sdk-daemon" "$out/sdk-daemon"
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
  tar -C "$root/sdk" --exclude=node_modules --exclude=.venv -cf - python typescript \
    | tar -C "$work" -xf -
  ln -s "$binaries"/python/tilo/* "$work/python/src/tilo/"
  ln -s "$binaries"/typescript/* "$work/typescript/native/"
  if [[ -d /opt/tilo-sdk/bun-cache ]]; then
    export BUN_INSTALL_CACHE_DIR=/opt/tilo-sdk/bun-cache
    (cd "$work/typescript" && bun install --frozen-lockfile --offline)
  else
    (cd "$work/typescript" && bun install --frozen-lockfile)
  fi
  # The SDK loads its addon as the package tilo-sdk-native: this build's, beside it.
  ln -s "$work/typescript/native" "$work/typescript/node_modules/tilo-sdk-native"

  # The daemon answers root as the device itself, never as its person: a job that runs as root
  # runs the daemon and the suites as an ordinary account, which owns the scratch they work in.
  local as_person=()
  if [[ $(id -u) == 0 ]]; then
    mkdir -p "$scratch/home" "$scratch/tmp"
    chown -R 1000:1000 "$scratch"
    chmod 0755 "$scratch"
    as_person=(setpriv --reuid=1000 --regid=1000 --clear-groups
      env HOME="$scratch/home" TMPDIR="$scratch/tmp")
  fi
  "${as_person[@]}" "$binaries/sdk-daemon" "$binaries/tilo-runtime" -- bash -euo pipefail -c '
    cd "$1/python"
    # A suite that stops answering prints where each of its threads waits, and then ends.
    timeout -k 10 300 uv run --frozen --extra dev python -m pytest -v -s -p no:cacheprovider \
      -o faulthandler_timeout=240 tests/test_daemon.py
    cd "$1/typescript"
    timeout -k 10 300 bun test --timeout 120000 test/daemon.test.ts
  ' sdk-native "$work"
}

case $mode in
  build) build ;;
  test) run_tests ;;
  *) echo "$usage" >&2; exit 2 ;;
esac
