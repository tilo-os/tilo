#!/usr/bin/env bash
# Build the three native executables shipped by every host-session package.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
profile=${QOS_BUILD_PROFILE:-release}

cargo_args=(--profile "$profile" --locked)
if [[ -n ${QOS_CARGO_TARGET:-} ]]; then
    cargo_args+=(--target "$QOS_CARGO_TARGET")
fi

cd "$repo_root"
cargo build "${cargo_args[@]}" -p launcher -p qos-unlock-helper
# The host package deliberately excludes the test-control socket and ffmpeg.
# This avoids a distro-specific libavcodec ABI in a login-session binary.
cargo build "${cargo_args[@]}" -p shell --no-default-features

target_dir=${CARGO_TARGET_DIR:-$repo_root/target}
if [[ -n ${QOS_CARGO_TARGET:-} ]]; then
    target_dir=$target_dir/$QOS_CARGO_TARGET
fi
printf '%s/%s\n' "$target_dir" "$profile"
