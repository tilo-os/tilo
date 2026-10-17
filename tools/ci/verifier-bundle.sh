#!/usr/bin/env bash
# Build the canonical verifier bundle of one architecture: the two native tools an app
# publication verifies with (tilo-app-package and tilo-runtime-bundle), packed with the
# verifier adapter by tools/canonical_verifier_bundle.py, and its descriptor. Platform
# maintenance only: it publishes nothing and changes no trust policy; someone reviews the
# bundle and uploads it.
#
#   tools/ci/verifier-bundle.sh ARCH SOURCE_SHA BUILDER_IMAGE OUT
#
# ARCH must be this machine's architecture, SOURCE_SHA the checked-out commit, BUILDER_IMAGE
# the immutable tilo-builder index the tools were built in. OUT receives ARCH.tar.gz and
# ARCH.tar.descriptor.json. The bundle is packed twice and both must be identical.
set -euo pipefail
[[ $# == 4 ]] || { echo "usage: $0 ARCH SOURCE_SHA BUILDER_IMAGE OUT" >&2; exit 2; }
arch=$1 source=$2 builder=$3 out=$4
target=${CARGO_TARGET_DIR:-target}
test "$(git rev-parse HEAD)" = "$source"
test "$(uname -m)" = "$arch"
cargo build --release --locked -p tilo-app-package-cli -p runtime-bundle
"$target/release/tilo-app-package" --help >/dev/null
"$target/release/tilo-runtime-bundle" --help >/dev/null
repeat=$(mktemp -d "${TMPDIR:-/tmp}/verifier-repeat.XXXXXX")
trap 'rm -rf -- "$repeat"' EXIT
rm -rf -- "$out"
mkdir -p "$out"
for directory in "$out" "$repeat"; do
  python3 -I tools/ci/python.py tools/canonical_verifier_bundle.py produce \
    --pkg-binary "$target/release/tilo-app-package" \
    --runtime-binary "$target/release/tilo-runtime-bundle" \
    --builder-image "$builder" --source-sha "$source" \
    --architecture "$arch" --output "$directory/$arch.tar.gz"
done
cmp "$out/$arch.tar.gz" "$repeat/$arch.tar.gz"
cmp "$out/$arch.tar.descriptor.json" "$repeat/$arch.tar.descriptor.json"
cat "$out/$arch.tar.descriptor.json"
