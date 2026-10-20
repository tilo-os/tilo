#!/usr/bin/env bash
# Seal a signed runtime bundle as the registry package that offers it.
#
#   pack-runtime-package.sh <bundle-dir> <package-id> <name> <package-version> \
#       <publisher-identity.json> <bin-dir> <out-dir>
#
# <bin-dir> holds tilo-app-package and tilo-runtime-bundle. The package is
# `kind = "runtime"`; its payload is the bundle directory (bundle.toml and the
# files it signs) and `package.provides` names the runtime the bundle is,
# `tilo-runtime/<bundle version>` (`tilolang/<bundle version>` for the
# tilolang runtime, apps/tilolang/tilo-runtime.toml). A runtime's libraries are hundreds of
# megabytes, so <out-dir> receives the package's thin form together with its
# content-addressed payload objects (`<hex>.blob`, and a `<hex>.chunks` list
# for each blob larger than one chunk), to be uploaded side by side; the
# registry serves the objects to devices that install it. Prints the thin
# package's path.
set -euo pipefail
bundle=$1
package_id=$2
name=$3
version=$4
identity=$5
bin_dir=$6
out=$7

"$bin_dir/tilo-runtime-bundle" verify "$bundle" --require-signature >&2
bundle_version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' "$bundle/bundle.toml" | head -1)
arch=$(sed -n 's/^arch = "\([^"]*\)"$/\1/p' "$bundle/bundle.toml" | head -1)
case "$arch" in
    x86_64) isa='x86_64 = "v3"' ;;
    aarch64) isa='aarch64 = "aarch64"' ;;
    *) printf 'pack-runtime-package: unsupported arch %s\n' "${arch:-none}" >&2; exit 1 ;;
esac
[[ $bundle_version =~ ^[0-9A-Za-z][0-9A-Za-z.+-]*$ ]] || {
    printf 'pack-runtime-package: unexpected bundle version %s\n' "${bundle_version:-none}" >&2
    exit 1
}
runtime_name=tilo-runtime
[[ $package_id == so.tilo.runtime.tilolang ]] && runtime_name=tilolang
pubkey=$("$bin_dir/tilo-runtime-bundle" key pubkey --identity "$identity")
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
cat > "$work/manifest.toml" <<TOML
[package]
id = "$package_id"
name = "$name"
category = "System"
version = "$version"
kind = "runtime"
provides = "$runtime_name/$bundle_version"

[package.isa]
$isa

[publisher]
identity = "ed25519:$pubkey"
signature = ""

[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"
TOML
if [[ $package_id == so.tilo.runtime.chromium ]]; then
    test -x "$bundle/lib/chromium/tilo-chromium-host"
    cat >> "$work/manifest.toml" <<'TOML'

[entrypoint]
type = "cli"
bin = "lib/chromium/tilo-chromium-host"
TOML
fi
if [[ $package_id == so.tilo.runtime.tilolang ]]; then
    # The binary every tilolang app's entrypoint executes.
    test -x "$bundle/bin/tilolang-app"
    cat >> "$work/manifest.toml" <<'TOML'

[entrypoint]
type = "cli"
bin = "bin/tilolang-app"
TOML
fi
if [[ $package_id == so.tilo.runtime.signal ]]; then
    test -x "$bundle/lib/lib64/ld-linux-x86-64.so.2"
    # The declaration is part of the signed native payload identity, preventing
    # reuse of an older installed bundle whose loader materialized as 0644.
    printf 'lib/lib64/ld-linux-x86-64.so.2\n' > "$work/runtime-entrypoint.txt"
    cmp "$work/runtime-entrypoint.txt" "$bundle/lib/signal-provider/runtime-entrypoint.txt"
    cat >> "$work/manifest.toml" <<'TOML'

[entrypoint]
type = "cli"
bin = "lib/lib64/ld-linux-x86-64.so.2"
TOML
fi
file="${package_id##*.}-$version-$arch.tapp"
"$bin_dir/tilo-app-package" pack --manifest "$work/manifest.toml" --payload "$bundle" \
    --identity "$identity" --output "$work/$file" >&2
"$bin_dir/tilo-app-package" verify --trusted-key "$pubkey" "$work/$file" >&2
mkdir -p "$out"
"$bin_dir/tilo-app-package" split "$work/$file" --out "$out" >&2
test -f "$out/$file"
printf '%s\n' "$out/$file"
