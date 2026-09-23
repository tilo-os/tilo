#!/usr/bin/env bash
# Contract-test that RPM packaging follows Cargo's externally selected target.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
builder="$repo_root/dist/rpm/build-rpm.sh"
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT

# Exercise the real lookup without compiling or entering the staging path:
# --no-cargo reaches the binary check immediately, so its diagnostic exposes
# the directory selected for Cargo's externally supplied target.
target_dir=$work/build-tmp/target
set +e
output=$(CARGO_TARGET_DIR="$target_dir" TILO_BIN_DIR= "$builder" --no-cargo 2>&1)
status=$?
set -e
(( status != 0 )) || {
    echo "RPM builder unexpectedly succeeded without fixture binaries" >&2
    exit 1
}
grep -Fqx "missing $target_dir/release/shell — run without --no-cargo" <<<"$output"

# An explicit binary directory remains an escape hatch for --no-cargo callers.
override_dir=$work/prebuilt
set +e
output=$(CARGO_TARGET_DIR="$target_dir/ignored" TILO_BIN_DIR="$override_dir" \
    "$builder" --no-cargo 2>&1)
status=$?
set -e
(( status != 0 )) || {
    echo "RPM builder unexpectedly succeeded without override fixture binaries" >&2
    exit 1
}
grep -Fqx "missing $override_dir/shell — run without --no-cargo" <<<"$output"

# Keep this check build-free: the regression happens after a successful Cargo
# build, and the packaging script's contract is visible in its target lookup.
grep -Fqx 'target_dir=${CARGO_TARGET_DIR:-$repo_root/target}' \
    <(sed -n '1,85p' "$builder")
grep -Fqx '[[ $target_dir = /* ]] || target_dir=$repo_root/$target_dir' \
    <(sed -n '1,85p' "$builder")
grep -Fqx 'target_dir=$(realpath -m "$target_dir")' \
    <(sed -n '1,85p' "$builder")
grep -Fqx 'rel=${TILO_BIN_DIR:-$target_dir/release}' \
    <(sed -n '1,85p' "$builder")
if grep -Fq 'rel=${TILO_BIN_DIR:-$repo_root/target/release}' "$builder"; then
    echo "RPM builder still ignores CARGO_TARGET_DIR" >&2
    exit 1
fi

echo "RPM build target-directory contract passed"
