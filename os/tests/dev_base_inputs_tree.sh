#!/bin/bash
# Outside a git checkout (such as a worktree copied to a build machine),
# os/dev/build-dev-image.sh fingerprints the base's inputs from the files
# themselves: a change to a production input changes the fingerprint, a change
# to a dev-image-only source or build output does not.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# The script's own definitions, from `_base_prune=` to the end of tree_inputs.
sed -n '/^_base_prune=(/,/^}/p' "$repo/os/dev/build-dev-image.sh" >"$work/functions.sh"
grep -q '^tree_inputs()' "$work/functions.sh" || { echo "FAIL: no tree_inputs in build-dev-image.sh" >&2; exit 1; }

tree="$work/tree"
mkdir -p "$tree/crates/a/src" "$tree/os/dev-image/etc" "$tree/target/release" \
  "$tree/os/external-assets/cache" "$tree/crates/dev/tilo-vm/src"
printf 'fn a() {}\n' >"$tree/crates/a/src/lib.rs"
printf 'dev\n' >"$tree/os/dev-image/etc/x"
printf 'bin\n' >"$tree/target/release/bin"
printf 'asset\n' >"$tree/os/external-assets/cache/asset"
printf 'fn vm() {}\n' >"$tree/crates/dev/tilo-vm/src/main.rs"

fingerprint() {
  (TILO_REPO="$tree"; source "$work/functions.sh"; tree_inputs | sha256sum | cut -c1-64)
}

before="$(fingerprint)"
printf 'more dev\n' >>"$tree/os/dev-image/etc/x"
printf 'rebuilt\n' >>"$tree/target/release/bin"
printf 'fetched\n' >>"$tree/os/external-assets/cache/asset"
printf '// host tool\n' >>"$tree/crates/dev/tilo-vm/src/main.rs"
[ "$(fingerprint)" = "$before" ] \
  || { echo "FAIL: a dev-image-only source or a build output changed the base's fingerprint" >&2; exit 1; }
printf 'fn b() {}\n' >>"$tree/crates/a/src/lib.rs"
[ "$(fingerprint)" != "$before" ] \
  || { echo "FAIL: a production source changed and the base's fingerprint did not" >&2; exit 1; }

printf 'ok: outside git the base is fingerprinted from its production inputs only\n'
