#!/usr/bin/env bash
# Seal the optional native provider using an existing release identity. This
# does not create an identity, admit a publisher, install or publish a package.
set -euo pipefail
manifest=$1
provider=$2
identity=$3
bin_dir=$4
out=$5
bundle=$6
version=${7:-0.1.0}
[[ $version =~ ^[0-9A-Za-z][0-9A-Za-z.+-]*$ ]] || exit 1

test -s "$identity"
test -x "$provider"
"$bin_dir/tilo-runtime-bundle" verify "$bundle" --require-signature >&2
bundle_version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' "$bundle/bundle.toml" | head -1)
[[ $bundle_version =~ ^2026\.10\.1-signal\+native\.[0-9a-f]{32}$ ]] || exit 1
pubkey=$("$bin_dir/tilo-runtime-bundle" key pubkey --identity "$identity")
[[ $pubkey =~ ^[0-9a-f]{64}$ ]] || exit 1
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
mkdir -p "$work/payload/bin"
install -m 0755 "$provider" "$work/payload/bin/tilo-signal-provider"
sed "s/^identity = \"ed25519:0\{64\}\"$/identity = \"ed25519:$pubkey\"/" "$manifest" > "$work/manifest.toml"
sed -i "s/^version = \"0.1.0\"$/version = \"$version\"/" "$work/manifest.toml"
sed -i "s|^runtime = \"tilo-runtime/2026.10.1-signal\"$|runtime = \"tilo-runtime/$bundle_version\"|" "$work/manifest.toml"
file="signal-provider-$version-x86_64.tapp"
"$bin_dir/tilo-app-package" pack --manifest "$work/manifest.toml" --payload "$work/payload" \
    --identity "$identity" --output "$work/$file" >&2
"$bin_dir/tilo-app-package" verify --trusted-key "$pubkey" "$work/$file" >&2
mkdir -p "$out"
"$bin_dir/tilo-app-package" split "$work/$file" --out "$out" >&2
test -f "$out/$file"
printf '%s\n' "$out/$file"
