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
#                                 <dir>/tilo-launcher (beside the daemon, which starts apps
#                                   through it)
#                                 <dir>/app-kit: what the TypeScript suite packs and installs
#                                   an app of its own from (app_kit, below)
#                               Needs cargo (the sdk-native-build check, in the builder image),
#                               and the network for bun's pinned release asset.
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
  cargo build --locked -p launcher --bin tilo-launcher
  cargo build --locked -p tilo-runtime-bundle -p tilo-app-package-cli
  cargo build --locked -p tilolang-cli --bin tilolang -p tilolang-process --bin tilolang-process \
    -p tilolang --bin tilolang-std
  local target=${CARGO_TARGET_DIR:-$root/target}
  rm -rf "$out"
  mkdir -p "$out/python/tilo" "$out/typescript"
  cp "$target/debug/lib_native.so" "$out/python/tilo/_native.abi3.so"
  cp "$target/debug/libtilo_sdk_node_native.so" "$out/typescript/tilo-sdk-native.$(platform).node"
  cp "$target/debug/tilo-automerge-worker" "$out/python/tilo/"
  cp "$target/debug/tilo-automerge-worker" "$out/typescript/"
  cp "$target/debug/tilo-runtime" "$out/tilo-runtime"
  cp "$target/debug/examples/sdk-daemon" "$out/sdk-daemon"
  cp "$target/debug/tilo-launcher" "$out/tilo-launcher"
  app_kit "$target/debug" "$out/app-kit"
}

# The runtime an app of the suite's links its interpreter's libraries against (`[package]
# runtime`): this machine's loader set, as the launcher mounts any app's.
APP_RUNTIME=0.0.1

# What the TypeScript suite makes an app of its own from, a process the daemon launches through
# tilo-launcher and hands its connection, into <kit>:
#   publisher.json, trusted-publishers  a throwaway publisher that signs the runtimes here and the
#                                       suite's app, and the list a debug daemon and launcher
#                                       trust it by (TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE); it
#                                       signs nothing outside this check
#   runtimes/<version>/                 the bun runtime's signed bundle and the app runtime's,
#                                       where TILO_RUNTIME_BUNDLE_DIR finds them installed
#   bun.provides, app.runtime           what an app's manifest names them by
#   tilo-app-package                    what the suite packs and signs the app with
#   runtimes/<tilolang version>/        the tilolang runtime bundle, which a tilolang app's
#                                       entrypoint executes
#   apps/<name>.tapp                    the tilolang packages the suite installs as they ship:
#                                       Chat, and Discord's bridge with its bun program
app_kit() {
  local bin=$1 kit=$2 work provides
  mkdir -p "$kit/runtimes"
  "$bin/tilo-runtime-bundle" key generate --out "$kit/publisher.json"
  "$bin/tilo-runtime-bundle" key pubkey --identity "$kit/publisher.json" > "$kit/trusted-publishers"
  cp "$bin/tilo-app-package" "$kit/"
  "$bin/tilo-runtime-bundle" build --version "$APP_RUNTIME" --out "$kit/runtimes/$APP_RUNTIME" \
    --identity "$kit/publisher.json" >/dev/null
  printf 'tilo-runtime/%s\n' "$APP_RUNTIME" > "$kit/app.runtime"
  # bun from its pinned release (os/external-assets/github-assets.tsv), baked into a runtime
  # bundle exactly as its registry package is, with the SDK's addon an app imports from it.
  work=$(mktemp -d "${TMPDIR:-/tmp}/sdk-native-kit.XXXXXX")
  (
    export TILO_GITHUB_ASSET_CACHE=$work/assets TILO_ASSET_STORE=
    # shellcheck source=/dev/null
    . tools/apps/runtime_packages.sh
    runtime_package_build bun "$bin/tilo-runtime-bundle" "$bin" "$kit/publisher.json" \
      "$work/packages" 0.0.0 > "$work/provides"
  )
  provides=$(cat "$work/provides")
  test -f "$work/packages/runtime/bun/${provides#*/}/bundle.toml"
  mv "$work/packages/runtime/bun/${provides#*/}" "$kit/runtimes/${provides#*/}"
  printf '%s\n' "$provides" > "$kit/bun.provides"
  rm -rf -- "$work"
  # The tilolang runtime and packages, built the way a release and an image build them.
  (
    # shellcheck source=/dev/null
    . tools/apps/tilolang_packages.sh
    local version app
    version=$(tilolang_runtime_version "$root")
    tilolang_bake_runtime "$bin/tilo-runtime-bundle" "$bin/tilolang-process" \
      "$bin/tilolang-std" "$root" "$kit/publisher.json" "$kit/runtimes/$version" "$version"
    mkdir -p "$kit/apps"
    for app in chat discord; do
      tilolang_build_package "$bin/tilolang" "$root" "apps/tl/$app" "$app" "$kit/apps" \
        "$kit/publisher.json" 0.1.0 "tilo-runtime/$APP_RUNTIME"
    done
  )
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

  # The daemon starts an app in its sandbox through tilo-launcher, beside it, which runs bwrap.
  if ! command -v bwrap >/dev/null; then
    echo "sdk-native.sh: no bwrap here, so the daemon starts no app" >&2
    exit 1
  fi
  # The TypeScript suite installs an app of its own, signed by the kit's throwaway publisher,
  # which this debug daemon and its launcher trust, on runtimes installed where they look. A copy,
  # since the publisher's key is its owner's alone to read.
  cp -a "$binaries/app-kit" "$scratch/app-kit"
  export TILO_SDK_APP_KIT=$scratch/app-kit
  export TILO_RUNTIME_BUNDLE_DIR=$TILO_SDK_APP_KIT/runtimes
  export TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE=$TILO_SDK_APP_KIT/trusted-publishers

  # The daemon answers root as the device itself, never as its person: a job that runs as root
  # runs the daemon and the suites as an ordinary account, which owns the scratch they work in,
  # its runtime directory included: the job's own is root's.
  local as_person=()
  if [[ $(id -u) == 0 ]]; then
    mkdir -p "$scratch/home" "$scratch/tmp" "$scratch/run"
    chmod 0700 "$scratch/run"
    chown -R 1000:1000 "$scratch"
    chmod 0755 "$scratch"
    as_person=(setpriv --reuid=1000 --regid=1000 --clear-groups
      env HOME="$scratch/home" TMPDIR="$scratch/tmp" XDG_RUNTIME_DIR="$scratch/run")
  fi
  RUST_LOG=${RUST_LOG:-info,tilo_runtime=debug} "${as_person[@]}" "$binaries/sdk-daemon" "$binaries/tilo-runtime" -- bash -euo pipefail -c '
    # Both suites run, so one that fails does not hide what the other says.
    failed=0
    cd "$1/typescript"
    timeout -k 10 400 bun test --timeout 120000 test/daemon.test.ts || failed=1
    cd "$1/python"
    # A suite that stops answering prints where each of its threads waits, and then ends.
    timeout -k 10 300 uv run --frozen --extra dev python -m pytest -v -s -p no:cacheprovider \
      -o faulthandler_timeout=240 tests/test_daemon.py || failed=1
    exit "$failed"
  ' sdk-native "$work"
}

case $mode in
  build) build ;;
  test) run_tests ;;
  *) echo "$usage" >&2; exit 2 ;;
esac
