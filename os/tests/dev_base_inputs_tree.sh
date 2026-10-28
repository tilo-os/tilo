#!/bin/bash
# Outside a git checkout (such as a worktree copied to a build machine),
# os/dev/build-dev-image.sh fingerprints the base's inputs from the files
# themselves: a change to a file the base build reads changes the fingerprint, a
# change to a dev-image-only source, a build output or a file no base build reads
# (docs, CI and harness scripts) does not.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# The script's own definitions, from the base input paths to the end of tree_inputs.
sed -n '/^mapfile -t _base_paths/,/^}/p' "$repo/os/dev/build-dev-image.sh" >"$work/functions.sh"
grep -q '^tree_inputs()' "$work/functions.sh" || { echo "FAIL: no tree_inputs in build-dev-image.sh" >&2; exit 1; }

tree="$work/tree"
mkdir -p "$tree/crates/a/src" "$tree/os/dev-image/etc" "$tree/target/release" \
  "$tree/os/external-assets/cache" "$tree/crates/dev/tilo-vm/src" "$tree/docs" "$tree/tools/ci"
cp "$repo/os/dev/base-input-paths.py" "$work/base-input-paths.py"
printf 'FROM scratch\nCOPY crates /build/crates\nRUN --mount=type=bind,source=os/scripts/x.sh,target=/x true\n' \
  >"$tree/os/Containerfile"
printf 'notes\n' >"$tree/docs/notes.md"
printf 'echo harness\n' >"$tree/tools/ci/harness.sh"
printf 'fn a() {}\n' >"$tree/crates/a/src/lib.rs"
printf 'dev\n' >"$tree/os/dev-image/etc/x"
printf 'bin\n' >"$tree/target/release/bin"
printf 'asset\n' >"$tree/os/external-assets/cache/asset"
printf 'fn vm() {}\n' >"$tree/crates/dev/tilo-vm/src/main.rs"

fingerprint() {
  (TILO_REPO="$tree"; TILO_OS_DIR="$work/os"; mkdir -p "$work/os/dev"
   cp "$work/base-input-paths.py" "$work/os/dev/"
   die() { echo "FAIL: $*" >&2; exit 1; }
   source "$work/functions.sh"; tree_inputs | sha256sum | cut -c1-64)
}

before="$(fingerprint)"
printf 'more dev\n' >>"$tree/os/dev-image/etc/x"
printf 'rebuilt\n' >>"$tree/target/release/bin"
printf 'fetched\n' >>"$tree/os/external-assets/cache/asset"
printf '// host tool\n' >>"$tree/crates/dev/tilo-vm/src/main.rs"
printf 'more notes\n' >>"$tree/docs/notes.md"
printf 'echo changed\n' >>"$tree/tools/ci/harness.sh"
[ "$(fingerprint)" = "$before" ] \
  || { echo "FAIL: a dev-image-only source, a build output or a file no base build reads changed the base's fingerprint" >&2; exit 1; }
printf 'fn b() {}\n' >>"$tree/crates/a/src/lib.rs"
[ "$(fingerprint)" != "$before" ] \
  || { echo "FAIL: a production source changed and the base's fingerprint did not" >&2; exit 1; }

printf 'FROM scratch\nCOPY crates /build/crates\nCOPY docs /docs\n' >"$tree/os/Containerfile"
before="$(fingerprint)"
printf 'read now\n' >>"$tree/docs/notes.md"
[ "$(fingerprint)" != "$before" ] \
  || { echo "FAIL: a file os/Containerfile copies changed and the base's fingerprint did not" >&2; exit 1; }

printf 'ok: outside git the base is fingerprinted from the files its build reads only\n'
