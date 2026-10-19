#!/usr/bin/env bash
# Build the native executables shipped by every host-session package.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
profile=${TILO_BUILD_PROFILE:-release}

cargo_args=(--profile "$profile" --locked)
if [[ -n ${TILO_CARGO_TARGET:-} ]]; then
    cargo_args+=(--target "$TILO_CARGO_TARGET")
fi

cd "$repo_root"
cargo build "${cargo_args[@]}" -p launcher -p tilo-unlock-helper -p tilo-session-record
# The host package installs Tilo and imports the old folders: the installation.
cargo build "${cargo_args[@]}" -p imager --bin imager --features installation
# The host package deliberately excludes the test-control socket. Video
# codecs come from the distribution's GStreamer at runtime.
# Package only the production compositor. Building the package without an
# explicit binary also compiles tools such as `shots`; under fat LTO each can
# retain 7-8 GiB and Cargo runs them concurrently even though they are never
# installed in a release package.
cargo build "${cargo_args[@]}" -p shell --bin shell --no-default-features

target_dir=${CARGO_TARGET_DIR:-$repo_root/target}
if [[ -n ${TILO_CARGO_TARGET:-} ]]; then
    target_dir=$target_dir/$TILO_CARGO_TARGET
fi
printf '%s/%s\n' "$target_dir" "$profile"
