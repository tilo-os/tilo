#!/usr/bin/env bash
# The env-runtime is published as env-runtime:set-<fingerprint>, and the
# fingerprint covers the package format its .tapp is packed in as well as
# os/Containerfile.env-runtime: a device reads packages of exactly one format,
# so a format change must resolve a new runtime (and ship none until it is
# published) rather than the old one, which no device can install.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

layout() { # format
  rm -rf "$work/repo"
  mkdir -p "$work/repo/os/dev" "$work/repo/crates/packaging/registry-types/src"
  cp "$repo/os/dev/resolve-env-runtime.sh" "$work/repo/os/dev/"
  cp "$repo/os/Containerfile.env-runtime" "$work/repo/os/"
  printf 'pub const PACKAGE_FORMAT_VERSION: u32 = %s;\n' "$1" \
    > "$work/repo/crates/packaging/registry-types/src/lib.rs"
}
tag() { bash "$work/repo/os/dev/resolve-env-runtime.sh" --tag; }

layout 2; two="$(tag)"
layout 2; [ "$(tag)" = "$two" ] || { echo "the same definition and format resolve another runtime" >&2; exit 1; }
layout 3; three="$(tag)"
[ "$three" != "$two" ] || { echo "a package format change resolves the same runtime" >&2; exit 1; }
[[ $two =~ ^set-[0-9a-f]{64}$ ]] || { echo "unexpected tag $two" >&2; exit 1; }
# The script reads the live constant, so the repository's own format counts.
grep -Eq '^pub const PACKAGE_FORMAT_VERSION: u32 = [0-9]+;$' "$repo/crates/packaging/registry-types/src/lib.rs"
# Without the constant it refuses rather than resolving a runtime of no known format.
printf '\n' > "$work/repo/crates/packaging/registry-types/src/lib.rs"
! tag 2>/dev/null
echo 'env-runtime fingerprint covers the package format: ok'
