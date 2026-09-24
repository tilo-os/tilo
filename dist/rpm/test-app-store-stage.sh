#!/usr/bin/env bash
# Contract-test the RPM system-store staging boundary without compiling 47 apps.
set -euo pipefail

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
mkdir -p "$release"/{tapp,runtime/tilo-runtime/2026.07,bin}
printf 'fixture package\n' > "$release/tapp/$package"
printf '\177ELFfixture\n' > "$release/runtime/tilo-runtime/2026.07/libfixture.so"
cat > "$release/runtime/tilo-runtime/2026.07/bundle.toml" <<'EOF'
version = "2026.07"
publisher_hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
EOF
cat > "$release/index.json" <<'EOF'
{"apps":[{"runtime":"tilo-runtime/2026.07"}]}
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
[[ ${1:-} == install && -n ${TILO_APPS_DATA_DIR:-} && -n ${TILO_RUNTIME_BUNDLE_DIR:-} ]]
root=$TILO_APPS_DATA_DIR
mkdir -p "$root/payloads/so.tilo.settings/bin" "$root/integrity/so.tilo.settings" "$root/bin"
printf '#!/bin/true\n' > "$root/payloads/so.tilo.settings/bin/settings"
cp "$TILO_CLI_SHIM_PATH" "$root/bin/settings-cli"
printf 'so.tilo.settings\n' > "$root/bin/settings-cli.tilo-shim"
cat > "$root/registry.toml" <<TOML
[[entry]]
id = "so.tilo.settings"
payload_root = "$root/payloads/so.tilo.settings"
manifest_path = "$root/payloads/so.tilo.settings/manifest.toml"
TOML
touch "$root/payloads/so.tilo.settings/manifest.toml"
EOF
printf '#!/bin/true\n' > "$release/bin/tilo-cli-shim"
chmod 0755 "$release/bin"/*

stage=$work/stage
"$repo_root/dist/rpm/stage-app-store.sh" "$release" "$release/bin" "$stage"

registry=$stage/usr/share/tilo-apps/registry.toml
grep -Fqx 'id = "so.tilo.settings"' "$registry"
grep -Fqx 'payload_root = "/usr/lib/tilo-apps/payloads/so.tilo.settings"' "$registry"
grep -Fqx 'manifest_path = "/usr/lib/tilo-apps/payloads/so.tilo.settings/manifest.toml"' "$registry"
! grep -Fq "$stage" "$registry"
test -x "$stage/usr/lib/tilo-apps/payloads/so.tilo.settings/bin/settings"
test ! -e "$stage/usr/share/tilo-apps/payloads"
test "$(stat -c %a "$stage/usr/share/tilo-apps/registry.toml")" = 644
test "$(stat -c %a "$stage/usr/lib/tilo-runtime/2026.07/bundle.toml")" = 644
test "$(stat -c %a "$stage/usr/lib/tilo-runtime/2026.07/libfixture.so")" = 755
test -x "$stage/usr/lib/tilo-apps/bin/settings-cli"
test "$(stat -c %a "$stage/usr/lib/tilo-apps/bin/settings-cli.tilo-shim")" = 644
grep -Fqx '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' \
    "$stage/etc/tilo/runtime-bundle-trusted-publishers.txt"

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
