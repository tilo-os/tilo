#!/usr/bin/env bash
# Assemble the room service image context and build the image.
#
#   build-image.sh CONTEXT REGISTRY_JSON [IMAGE]
#
# CONTEXT (absolute) receives bin/ (tilo-relay with the web feature,
# tilo-room-service), assets/ (the web runtime's page modules, fonts and the
# registry verifier from build-web-dev.sh, plus web/*, plus REGISTRY_JSON as
# registry.json) and the Dockerfile. The core and the apps are not in the
# image: pages load them from the registry REGISTRY_JSON names
# ({baseUrl, ownerKeyHex, runtimeAbi, relayRuntimePackage}), which must
# publish the relay core (build-web-core.sh --features relay) as
# relayRuntimePackage. With IMAGE, the image is built with docker, or podman
# when docker is absent. SKIP_ASSETS=1 reuses CONTEXT/assets; SKIP_BINARIES=1
# CONTEXT/bin.
set -euo pipefail
service=$(cd "$(dirname "$0")/.." && pwd)
repo=$(cd "$service/../../.." && pwd)
context=${1:?usage: build-image.sh CONTEXT REGISTRY_JSON [IMAGE]}
registry=${2:?usage: build-image.sh CONTEXT REGISTRY_JSON [IMAGE]}
image=${3:-}
[[ "$context" == /* ]] || { echo 'CONTEXT must be absolute' >&2; exit 2; }
mkdir -p "$context"
target=${CARGO_TARGET_DIR:-$repo/target}
if [[ "${SKIP_BINARIES:-0}" != 1 ]]; then
  (cd "$repo" && cargo build --locked --release -p tilo-relay --features web --bin tilo-relay)
  (cd "$repo" && cargo build --locked --release -p tilo-room-service)
  rm -rf "$context/bin" && mkdir -p "$context/bin"
  cp "$target/release/tilo-relay" "$target/release/tilo-room-service" "$context/bin/"
fi
if [[ "${SKIP_ASSETS:-0}" != 1 ]]; then
  build=$(mktemp -d)
  "$repo/crates/browser/tilo-browser-runtime/scripts/build-web-dev.sh" "$build" word-processor >/dev/null
  rm -rf "$context/assets" && mkdir -p "$context/assets"
  (cd "$build" && cp -r ./*.mjs ./*.js ./*.wasm fonts fonts.json "$context/assets/")
  rm -f "$context/assets"/*.test.mjs
  rm -rf "$build"
fi
cp "$service"/web/* "$context/assets/"
cp "$registry" "$context/assets/registry.json"
cp "$service/Dockerfile" "$context/Dockerfile"
if [[ -n "$image" ]]; then
  engine=$(command -v docker || command -v podman)
  "$engine" build -t "$image" "$context"
fi
