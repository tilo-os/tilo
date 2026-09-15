#!/usr/bin/env bash
# Contract-test the RPM system-store staging boundary without compiling 47 apps.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
work=$(mktemp -d)
cleanup() { rm -rf -- "$work"; }
trap cleanup EXIT

release=$work/release
mkdir -p "$release"/{qpkg,runtime/2026.07,bin}
printf 'fixture package\n' > "$release/qpkg/settings-0.1.0-x86_64.qpkg"
cat > "$release/runtime/2026.07/bundle.toml" <<'EOF'
version = "2026.07"
publisher_hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
EOF
cat > "$release/index.json" <<'EOF'
{"apps":[{"runtime":"tilo-runtime/2026.07"}]}
EOF
(cd "$release" && find qpkg runtime -type f -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS \
    && sha256sum index.json >> SHA256SUMS \
    && tar -czf tilo-apps-x86_64.tar.gz qpkg runtime index.json SHA256SUMS)

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
mkdir -p "$root/payloads/dev.tilo.settings/bin" "$root/integrity/dev.tilo.settings"
printf '#!/bin/true\n' > "$root/payloads/dev.tilo.settings/bin/settings"
cat > "$root/registry.toml" <<TOML
[[entry]]
id = "dev.tilo.settings"
payload_root = "$root/payloads/dev.tilo.settings"
manifest_path = "$root/payloads/dev.tilo.settings/manifest.toml"
TOML
touch "$root/payloads/dev.tilo.settings/manifest.toml"
EOF
printf '#!/bin/true\n' > "$release/bin/tilo-cli-shim"
chmod 0755 "$release/bin"/*

stage=$work/stage
"$repo_root/dist/rpm/stage-app-store.sh" "$release" "$release/bin" "$stage"

registry=$stage/usr/share/tilo-apps/registry.toml
grep -Fqx 'id = "dev.tilo.settings"' "$registry"
grep -Fqx 'payload_root = "/usr/share/tilo-apps/payloads/dev.tilo.settings"' "$registry"
grep -Fqx 'manifest_path = "/usr/share/tilo-apps/payloads/dev.tilo.settings/manifest.toml"' "$registry"
! grep -Fq "$stage" "$registry"
test -x "$stage/usr/share/tilo-apps/payloads/dev.tilo.settings/bin/settings"
grep -Fqx '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' \
    "$stage/etc/tilo/runtime-bundle-trusted-publishers.txt"

# The collection checksum is part of the RPM's provenance boundary; do not
# materialise a system registry from a substituted archive.
printf 'tampered\n' >> "$release/qpkg/settings-0.1.0-x86_64.qpkg"
(cd "$release" && tar -czf tilo-apps-x86_64.tar.gz qpkg runtime index.json SHA256SUMS)
if "$repo_root/dist/rpm/stage-app-store.sh" "$release" "$release/bin" "$work/rejected" \
    >/dev/null 2>&1; then
    echo "app-store staging accepted a checksum-mismatched collection" >&2
    exit 1
fi

echo "RPM app-store staging contract passed"
