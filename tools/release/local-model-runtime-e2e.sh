#!/usr/bin/env bash
# The signed llama.cpp runtime end to end (crates/agent/local-model-service/tests/runtime_e2e.rs):
# a tiny GGUF model, pinned by hash, installed through the service with the CPU runtime archive
# RUNTIME (a local-model-runtime build's OUT) and answering a chat completion through the model
# gateway built here.
#
# usage: local-model-runtime-e2e.sh RUNTIME
set -euo pipefail
repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
source "$repo_root/tools/ci/publish/lib.sh"
cd "$repo_root"
runtime=${1:?usage: local-model-runtime-e2e.sh RUNTIME}

# llama.cpp's own test model: a 260K-parameter story model, at a pinned commit and hash.
model=$scratch/stories260K.gguf
curl --fail --silent --show-error --location --proto '=https' --retry 3 --output "$model" \
  https://huggingface.co/ggml-org/models-moved/resolve/499bc8821c6b12b4e53c5bffcb21ec206f212d81/tinyllamas/stories260K.gguf
echo "270cba1bd5109f42d03350f60406024560464db173c0e387d91f0426d3bd256d  $model" | sha256sum --check --quiet

cargo build --locked -p tilo-model-gateway --features local-models,host-backend --bin tilo-model-gateway
TILO_E2E_RUNTIME=$runtime TILO_E2E_MODEL=$model \
  TILO_E2E_GATEWAY=${CARGO_TARGET_DIR:-target}/debug/tilo-model-gateway \
  cargo test --locked -p local-model-service --test runtime_e2e -- --ignored --nocapture
