#!/usr/bin/env bash
# Build the FUTO swipe stack (libswipe_decoder.a + ExecuTorch) and fetch the
# models, so `qos-swipe` can link the real ML swipe decoder. Prints the two
# env vars to export for the cargo build.
#
#   ./qos-swipe/setup-futo.sh [DEST]
#
# DEST defaults to ./.futo-swipe (gitignored). Requires: git, git-lfs, cmake
# 3.29+, a C++17 compiler, python3 + python3-venv, ninja, curl. Run inside the
# build container (`distrobox enter my-distrobox`) where these are present.
set -euo pipefail

DEST="${1:-$(pwd)/.futo-swipe}"
LIB="$DEST/swipe-library"
MODELS="$DEST/models"

mkdir -p "$DEST"

if [ ! -d "$LIB/.git" ]; then
    echo "==> cloning swipe-library (+ ExecuTorch submodule, large)"
    git clone https://gitlab.futo.org/keyboard/swipe-library.git "$LIB"
fi
git -C "$LIB" submodule update --init --recursive

if [ ! -f "$LIB/build/libswipe_decoder.a" ]; then
    echo "==> building ExecuTorch + libswipe_decoder.a (make host — slow)"
    make -C "$LIB" host
fi

if [ ! -f "$MODELS/honorable_sturgeon/model_fp32.pte" ]; then
    echo "==> fetching FUTO swipe models from HuggingFace"
    git clone https://huggingface.co/futo-org/futo-swipe "$MODELS"
fi
if [ ! -f "$MODELS/en_w2.combined" ]; then
    echo "==> fetching English dictionary vocab"
    curl -fsSL -o "$MODELS/en_w2.combined" https://dl.keyboard.futo.org/en_w2.combined
fi

echo
echo "FUTO swipe ready. Export these for the cargo build:"
echo "  export QOS_SWIPE_LIB=$LIB"
echo "  export QOS_SWIPE_MODELS=$MODELS"
echo
echo "Then e.g.:  cargo run -p osk"
echo "For the OS image, install \$QOS_SWIPE_MODELS to /usr/share/qos/swipe-models."
