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
cargo build "${cargo_args[@]}" -p launcher -p tilo-unlock-helper -p tilo-flight-log
# `tilo`, the command a terminal reaches the desktop's apps and actions with.
cargo build "${cargo_args[@]}" -p tilo-cli --bin tilo
# Beside it, the daemon `tilo login` starts, and the program a task's tilolang programs run in.
cargo build "${cargo_args[@]}" -p tilo-runtime --bin tilo-runtime --no-default-features
cargo build "${cargo_args[@]}" -p tilolang-process --bin tilolang-process
# The host package deliberately excludes the test-control socket. Video
# codecs come from the distribution's GStreamer at runtime.
# The session a login starts. Building the package without an explicit binary
# also compiles tools such as the benchmark harness; under fat LTO each can
# retain 7-8 GiB and Cargo runs them concurrently even though they are never
# installed in a release package.
cargo build "${cargo_args[@]}" -p tilo-session --bin tilo-session

target_dir=${CARGO_TARGET_DIR:-$repo_root/target}
if [[ -n ${TILO_CARGO_TARGET:-} ]]; then
    target_dir=$target_dir/$TILO_CARGO_TARGET
fi
printf '%s/%s\n' "$target_dir" "$profile"
