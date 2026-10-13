#!/usr/bin/env bash
# Offscreen acceptance captures; never connects to a user's compositor or builds.
set -euo pipefail
root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
binary="${CHAT_BINARY:-$root/target/debug/chat}"
out="${1:-$root/target/chat-task-shots}"
image="${TILO_CHAT_CAPTURE_IMAGE:-}"
[[ -x "$binary" ]] || { echo "Set CHAT_BINARY to an existing, compatible Chat executable (this script does not build)." >&2; exit 1; }
binary="$(realpath "$binary")"
mkdir -p "$out"
out="$(realpath "$out")"
if [[ -n "$image" ]]; then
    [[ "$(podman info --format '{{.Host.Security.Rootless}}')" == true ]] || {
        echo "Capture requires rootless podman." >&2; exit 1;
    }
    # Opt in to the existing image used to build this binary. In particular,
    # its fontconfig ABI must match; an arbitrary older CI image is not safe.
    podman run --rm --pull never --network none --security-opt label=disable \
        -v "$binary:/usr/local/bin/chat:ro" -v "$out:/capture:rw" \
        -e WGPU_BACKEND=vulkan \
        -e VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json \
        "$image" bash -euc '
            unset DISPLAY WAYLAND_DISPLAY
            runtime="$(mktemp -d)"
            trap '\''rm -rf "$runtime"'\'' EXIT
            export XDG_RUNTIME_DIR="$runtime" XDG_CACHE_HOME="$runtime/cache"
            mkdir -p "$XDG_CACHE_HOME"
            chat --shots /capture/normal.png --scenario agent-task
            chat --shots /capture/compact.png --scenario agent-task-compact
            chat --shots /capture/queued.png --scenario agent-task-queued
            chat --shots /capture/queued-compact.png --scenario agent-task-queued-compact
        '
else
    # Direct execution needs matching host libraries/fonts and an offscreen GPU
    # backend (software Vulkan is sufficient). Isolate caches and runtime state.
    runtime="$(mktemp -d)"
    trap 'rm -rf "$runtime"' EXIT
    mkdir -p "$runtime/cache"
    export XDG_RUNTIME_DIR="$runtime" XDG_CACHE_HOME="$runtime/cache"
    export WGPU_BACKEND="${WGPU_BACKEND:-vulkan}"
    unset DISPLAY WAYLAND_DISPLAY
    "$binary" --shots "$out/normal.png" --scenario agent-task
    "$binary" --shots "$out/compact.png" --scenario agent-task-compact
    "$binary" --shots "$out/queued.png" --scenario agent-task-queued
    "$binary" --shots "$out/queued-compact.png" --scenario agent-task-queued-compact
fi
[[ -s "$out/normal.png" && -s "$out/compact.png" && -s "$out/queued.png" && -s "$out/queued-compact.png" ]]
printf 'Captures: %s/{normal,compact,queued,queued-compact}.png\n' "$out"
