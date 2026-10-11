#!/usr/bin/env bash
# Contract-test the RPM system-store staging boundary without compiling 47 apps.
set -euo pipefail

export TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY=$(printf 'ab%.0s' {1..32})

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
work=$(mktemp -d)
cleanup() { rm -rf -- "$work"; }
trap cleanup EXIT

# Exercise the same native-architecture collection selection as the production
# staging helper. An x86-only fixture made every ARM packaging preflight fail.
arch=$(uname -m)
case "$arch" in
    x86_64|aarch64) ;;
    *) echo "unsupported fixture architecture: $arch" >&2; exit 1 ;;
esac
package="settings-0.1.0-$arch.tapp"
collection="tilo-apps-$arch.tar.gz"

release=$work/release
mkdir -p "$release"/{tapp,runtime/tilo-runtime/2026.07+native.0123456789abcdef0123456789abcdef,bin}
printf 'fixture package\n' > "$release/tapp/$package"
printf '\177ELFfixture\n' > "$release/runtime/tilo-runtime/2026.07+native.0123456789abcdef0123456789abcdef/libfixture.so"
cat > "$release/runtime/tilo-runtime/2026.07+native.0123456789abcdef0123456789abcdef/bundle.toml" <<'EOF'
version = "2026.07+native.0123456789abcdef0123456789abcdef"
publisher_hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
EOF
cat > "$release/index.json" <<'EOF'
{"apps":[{"runtime":"tilo-runtime/2026.07+native.0123456789abcdef0123456789abcdef"}]}
EOF
(cd "$release" && find tapp runtime -type f -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS \
    && sha256sum index.json >> SHA256SUMS \
    && tar -czf "$collection" tapp runtime index.json SHA256SUMS)

cat > "$release/bin/tilo-runtime-bundle" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ $1 == verify && -f $2/bundle.toml ]]
EOF
cat > "$release/bin/tilo-install" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == image ]]; then
    [[ $2 == add && $4 == --publisher && -n ${TILO_APPS_DATA_DIR:-} ]]
    image=$TILO_APPS_DATA_DIR/pkgs/$(printf 'cd%.0s' {1..32})
    mkdir -p "$image/usr/bin" "$image.pkg" "$TILO_APPS_DATA_DIR/content/data"
    printf '#!/bin/true\n' > "$image/usr/bin/bash"
    chmod 0555 "$image/usr/bin/bash"
    ln -s bash "$image/usr/bin/sh"
    echo "tapp:$(printf 'cd%.0s' {1..32}) env-runtime so.tilo.env-runtime 44.1.0"
    exit 0
fi
[[ ${1:-} == install && -n ${TILO_APPS_DATA_DIR:-} && -n ${TILO_RUNTIME_BUNDLE_DIR:-} ]]
root=$TILO_APPS_DATA_DIR
tree=$root/pkgs/$(printf 'ab%.0s' {1..32})
mkdir -p "$tree/bin" "$tree.pkg" "$root/bin" "$root/content/data" "$root/tmp"
printf '#!/bin/true\n' > "$tree/bin/settings"
printf 'blob\n' > "$root/content/data/blob.data"
touch "$root/.lock"
# Like the real installer, hold the runtime collector off in the runtime root.
touch "$TILO_RUNTIME_BUNDLE_DIR/.gc.lock"
[[ -z ${FIXTURE_RUNTIME_STATE:-} ]] || mkdir -p "$TILO_RUNTIME_BUNDLE_DIR/$FIXTURE_RUNTIME_STATE"
cp "$TILO_CLI_SHIM_PATH" "$root/bin/settings-cli"
printf 'so.tilo.settings\n' > "$root/bin/settings-cli.tilo-shim"
cat > "$root/registry.toml" <<TOML
[[entry]]
id = "so.tilo.settings"
payload_root = "$tree"
manifest_path = "$tree.pkg/manifest.toml"
TOML
touch "$tree.pkg/manifest.toml"
EOF
printf '#!/bin/true\n' > "$release/bin/tilo-cli-shim"
chmod 0755 "$release/bin"/*

env_runtime=$work/env-runtime
image_id=$(printf 'cd%.0s' {1..32})
publisher=$(printf 'ef%.0s' {1..32})
mkdir -p "$env_runtime"
printf 'fixture runtime\n' > "$env_runtime/env-runtime-$arch.tapp"
printf 'tapp:%s\n' "$image_id" > "$env_runtime/env-runtime-$arch.id"
printf '%s\n' "$publisher" > "$env_runtime/publisher.hex"
(cd "$env_runtime" && sha256sum ./*.tapp > SHA256SUMS)

stage=$work/stage
TILO_ENV_RUNTIME_DIR=$env_runtime \
    "$repo_root/dist/rpm/stage-app-store.sh" "$release" "$release/bin" "$stage"

trust=$stage/usr/share/tilo/trust/tilo-main-registry-owner.pub
grep -Fxq "$TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY" "$trust"
test "$(stat -c %a "$trust")" = 644
for owner in '' invalid "$(printf '0%.0s' {1..64})"; do
    if TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY=$owner \
        "$repo_root/dist/rpm/stage-app-store.sh" "$release" "$release/bin" "$work/no-owner" \
        >"$work/no-owner.log" 2>&1; then
        echo "staging accepted a missing or invalid official registry owner" >&2
        exit 1
    fi
    grep -Fq 'must be a non-placeholder 32-byte hex key' "$work/no-owner.log"
    test ! -e "$work/no-owner"
done

registry=$stage/usr/share/tilo-apps/registry.toml
grep -Fqx 'id = "so.tilo.settings"' "$registry"
tree_id=$(printf 'ab%.0s' {1..32})
grep -Fqx "payload_root = \"/usr/lib/tilo-apps/pkgs/$tree_id\"" "$registry"
grep -Fqx "manifest_path = \"/usr/lib/tilo-apps/pkgs/$tree_id.pkg/manifest.toml\"" "$registry"
! grep -Fq "$stage" "$registry"
test -x "$stage/usr/lib/tilo-apps/pkgs/$tree_id/bin/settings"
test ! -e "$stage/usr/share/tilo-apps/pkgs"
# The read-only system store ships trees, not the installer's content store.
test ! -e "$stage/usr/lib/tilo-apps/content"
test ! -e "$stage/usr/lib/tilo-apps/tmp"
test ! -e "$stage/usr/lib/tilo-apps/.lock"
# Nor the per-user runtime collector's lock; nothing locks the system root.
test ! -e "$stage/usr/lib/tilo-runtime/.gc.lock"
test -z "$(find "$stage/usr/lib/tilo-runtime" -mindepth 1 -maxdepth 1 -name '.*')"
# Any other hidden state in the runtime root is unexplained and fails staging.
if FIXTURE_RUNTIME_STATE=.leases TILO_ENV_RUNTIME_DIR=$env_runtime \
    "$repo_root/dist/rpm/stage-app-store.sh" "$release" "$release/bin" "$work/runtime-state" \
    >"$work/runtime-state.log" 2>&1; then
    echo "app-store staging shipped unexplained runtime collector state" >&2
    exit 1
fi
grep -Fq 'collector state left in the packaged runtime root' "$work/runtime-state.log"
test "$(stat -c %a "$stage/usr/share/tilo-apps/registry.toml")" = 644
test "$(stat -c %a "$stage/usr/lib/tilo-runtime/2026.07+native.0123456789abcdef0123456789abcdef/bundle.toml")" = 644
test "$(stat -c %a "$stage/usr/lib/tilo-runtime/2026.07+native.0123456789abcdef0123456789abcdef/libfixture.so")" = 755
test -x "$stage/usr/lib/tilo-apps/bin/settings-cli"
test "$(stat -c %a "$stage/usr/lib/tilo-apps/bin/settings-cli.tilo-shim")" = 644
grep -Fqx '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' \
    "$stage/etc/tilo/runtime-bundle-trusted-publishers.txt"
# Store updates use the official release runtime signer, independently of the
# runtime baked into this host RPM. Both authorities must survive staging.
grep -Fqx "$(cat "$repo_root/os/registry-runtime-publisher.hex")" \
    "$stage/etc/tilo/runtime-bundle-trusted-publishers.txt"
test "$(head -n 1 "$stage/etc/tilo/runtime-bundle-trusted-publishers.txt")" = \
    '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef'
test "$(wc -l < "$stage/etc/tilo/runtime-bundle-trusted-publishers.txt")" = 2
# An absent or malformed independently committed pin must fail, rather than
# silently shipping only the baked key and breaking Store updates again.
pin_repo=$work/pin-repo
mkdir -p "$pin_repo/dist/rpm" "$pin_repo/os"
cp "$repo_root/dist/rpm/stage-app-store.sh" "$pin_repo/dist/rpm/"
printf 'not-a-public-key\n' > "$pin_repo/os/registry-runtime-publisher.hex"
if "$pin_repo/dist/rpm/stage-app-store.sh" "$release" "$release/bin" "$work/bad-pin" \
    >"$work/bad-pin.log" 2>&1; then
    echo "app-store staging accepted a malformed registry runtime publisher" >&2
    exit 1
fi
grep -Fq 'registry runtime publisher pin is not a 64-character public key' "$work/bad-pin.log"
test ! -e "$work/bad-pin/etc/tilo/runtime-bundle-trusted-publishers.txt"
rm "$pin_repo/os/registry-runtime-publisher.hex"
if "$pin_repo/dist/rpm/stage-app-store.sh" "$release" "$release/bin" "$work/missing-pin" \
    >"$work/missing-pin.log" 2>&1; then
    echo "app-store staging accepted a missing registry runtime publisher" >&2
    exit 1
fi
grep -Fq 'registry runtime publisher pin unavailable' "$work/missing-pin.log"
test ! -e "$work/missing-pin/etc/tilo/runtime-bundle-trusted-publishers.txt"
# The env-runtime image ships beside the apps with its tree untouched by the
# mode normalisation, and its publisher becomes a trusted image publisher.
test "$(stat -c %a "$stage/usr/lib/tilo-apps/pkgs/$image_id/usr/bin/bash")" = 555
test "$(readlink "$stage/usr/lib/tilo-apps/pkgs/$image_id/usr/bin/sh")" = bash
grep -Fqx "$publisher" "$stage/usr/share/tilo-apps/image-publishers"
test "$(stat -c %a "$stage/usr/share/tilo-apps/image-publishers")" = 644
test ! -e "$stage/usr/lib/tilo-apps/content"
# A runtime that does not match its published checksum is refused.
printf 'tampered\n' >> "$env_runtime/env-runtime-$arch.tapp"
if TILO_ENV_RUNTIME_DIR=$env_runtime \
    "$repo_root/dist/rpm/stage-app-store.sh" "$release" "$release/bin" "$work/tampered-runtime" \
    >/dev/null 2>&1; then
    echo "app-store staging accepted a checksum-mismatched env-runtime" >&2
    exit 1
fi

# The collection checksum is part of the RPM's provenance boundary; do not
# materialise a system registry from a substituted archive.
printf 'tampered\n' >> "$release/tapp/$package"
(cd "$release" && tar -czf "$collection" tapp runtime index.json SHA256SUMS)
if "$repo_root/dist/rpm/stage-app-store.sh" "$release" "$release/bin" "$work/rejected" \
    >/dev/null 2>&1; then
    echo "app-store staging accepted a checksum-mismatched collection" >&2
    exit 1
fi

echo "RPM app-store staging contract passed ($arch)"
