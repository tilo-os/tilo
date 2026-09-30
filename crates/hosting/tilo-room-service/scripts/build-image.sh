#!/usr/bin/env bash
# Assemble the room service image context and build the image.
#
#   build-image.sh CONTEXT [IMAGE]
#
# CONTEXT (absolute) receives bin/ (tilo-relay with the web feature,
# tilo-room-service), assets/ (the browser runtime's desktop bundle from
# build.sh --bundle desktop: the runtime with its certified peer, every web
# app, the page modules, fonts and data, plus web/*) and the Dockerfile.
# With IMAGE, the image is built with docker, or podman when docker is
# absent. SKIP_ASSETS=1 reuses CONTEXT/assets; SKIP_BINARIES=1 CONTEXT/bin.
set -euo pipefail
service=$(cd "$(dirname "$0")/.." && pwd)
repo=$(cd "$service/../../.." && pwd)
context=${1:?usage: build-image.sh CONTEXT [IMAGE]}
image=${2:-}
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
  "$repo/crates/browser/tilo-browser-runtime/scripts/build.sh" "$build/bundle" --bundle desktop >/dev/null
  rm -rf "$context/assets"
  mv "$build/bundle" "$context/assets"
  rm -rf "$build"
fi
cp "$service"/web/* "$context/assets/"
cp "$service/Dockerfile" "$context/Dockerfile"
if [[ -n "$image" ]]; then
  engine=$(command -v docker || command -v podman)
  "$engine" build -t "$image" "$context"
fi
