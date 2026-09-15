#!/usr/bin/env bash
# Validate actual Rust-rendered domains against the selected libvirt release.
# Use that release's complete schema directory, not an isolated domain.rng copy.
set -euo pipefail
schema=$(realpath "${1:?usage: windows_vm_schema.sh PATH_TO_LIBVIRT_DOMAIN_RNG}")
test -f "$schema"
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
out=$(mktemp -d)
trap 'rm -rf -- "$out"' EXIT
cd "$repo"
for arch in x86_64 aarch64; do
    cargo run -p windows-vm --example domain-xml --locked -- "$arch" > "$out/$arch.xml"
    xmllint --noout --relaxng "$schema" "$out/$arch.xml"
done
