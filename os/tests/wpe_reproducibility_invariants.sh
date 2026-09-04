#!/usr/bin/env bash
# WPE source and buildroot inputs must be immutable before an OS build starts.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
container="$repo/os/Containerfile"
buildroot="$repo/os/Containerfile.wpe-buildroot"
build_script="$repo/os/dev/build-wpe.sh"
image_script="$repo/os/dev/build-image.sh"
manifest="$repo/os/external-assets/github-assets.tsv"
pins="$repo/os/wpe-buildroot-snapshot.env"
snapshot_workflow="$repo/.forgejo/workflows/snapshot-wpe-buildroot.yml"
credential_helper="$repo/os/dev/acquire-wpe-snapshot-credentials.sh"
arch_helper="$repo/os/dev/assert-native-arch.sh"
image_workflow="$repo/.forgejo/workflows/build-image.yml"
fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }
has() { grep -Fq -- "$2" "$1" && ok "$3" || bad "$3"; }

bash -n "$build_script" "$image_script" "$repo/os/dev/build-wpe-buildroot.sh" \
  "$arch_helper" \
  "$credential_helper" "$repo/os/tests/wpe_snapshot_credentials.sh" \
  && ok 'WPE build and bootstrap scripts parse' \
  || bad 'a WPE build or bootstrap script does not parse'
"$repo/os/tests/wpe_snapshot_credentials.sh" \
  || bad 'WPE snapshot credential acquisition invariants failed'

while IFS='|' read -r name digest; do
  grep -Fq "$digest"$'\t'"$name"$'\t'https://wpewebkit.org/releases/"$name" "$manifest" \
    && grep -Fq "${name%.tar.xz}" "$build_script" \
    && grep -Fq "$digest" "$build_script" \
    && ok "$name is versioned and checksum-pinned" \
    || bad "$name is not consistently pinned in the cache and build script"
done <<'EOF'
libwpe-1.16.3.tar.xz|c880fa8d607b2aa6eadde7d6d6302b1396ebc38368fe2332fa20e193c7ee1420
wpebackend-fdo-1.16.1.tar.xz|544ae14012f8e7e426b8cb522eb0aaaac831ad7c35601d1cf31d37670e0ebb3b
wpewebkit-2.52.6.tar.xz|b2bafef2751625b7fdf530f230ff0f542ff0eeba3590c3a989d931b2a55c858e
EOF

if grep -Eq 'latest\(\)|curl[^\n]*"?\$REL/"?([[:space:]]|$)' "$build_script"; then
  bad 'WPE source selection still scrapes a mutable release index'
else
  ok 'WPE source selection has no latest-release scrape'
fi
has "$build_script" 'QOS_WPE_OFFLINE' 'WPE build supports a fail-closed offline source mode'
has "$container" 'QOS_WPE_SOURCE_DIR=/qos-assets QOS_WPE_OFFLINE=1' \
  'production WPE compilation uses only the verified external cache'

wpe_stage="$(sed -n '/^FROM ${WPE_BUILDROOT_REF} AS wpe-builder-1/,/^FROM ${BASE_REF} AS wpe-builder-0/p' "$container")"
if [ -n "$wpe_stage" ] && ! grep -Eq '\bdnf\b|/etc/yum\.repos\.d' <<<"$wpe_stage"; then
  ok 'production WPE stage has no Fedora repository transaction'
else
  bad 'production WPE stage can still access Fedora repositories'
fi
has "$container" 'FROM ${WPE_BUILDROOT_REF} AS wpe-builder-1' \
  'production WPE stage starts from the promoted buildroot'
has "$buildroot" 'dnf builddep -y webkitgtk6.0' \
  'dedicated snapshot resolves the Fedora WebKit BuildRequires'
has "$buildroot" 'find /etc/yum.repos.d -type f -delete' \
  'promoted WPE buildroot disables Fedora repositories'
has "$buildroot" '/usr/lib/qos/wpe-buildroot-packages.txt' \
  'promoted WPE buildroot records its exact installed package set'

for arch in X86_64 AARCH64; do
  value="$(sed -n "s/^WPE_BUILDROOT_${arch}_REF=//p" "$pins")"
  if [ -z "$value" ] || [[ "$value" =~ ^code\.podesta\.ai/qos/qos-wpe-buildroot@sha256:[0-9a-f]{64}$ ]]; then
    ok "$arch buildroot pin is either explicit bootstrap-empty or digest-pinned"
  else
    bad "$arch WPE buildroot uses a mutable or malformed reference"
  fi
done

has "$snapshot_workflow" 'runner: runner-arm64' 'WPE snapshot builds on a native ARM64 runner'
has "$snapshot_workflow" 'runner: runner' 'WPE snapshot builds on the native x86-64 runner'
has "$snapshot_workflow" './os/dev/assert-native-arch.sh "${{ matrix.arch }}"' \
  'WPE snapshot fails early when a native runner label points at the wrong architecture'
has "$snapshot_workflow" "if: github.ref == 'refs/heads/main'" \
  'WPE snapshot cannot publish from a feature branch'
has "$snapshot_workflow" 'no-cache: true' 'WPE snapshot refresh cannot reuse a stale Fedora transaction layer'
has "$snapshot_workflow" 'actual_arch="$(docker run --rm --entrypoint cat "$ref"' \
  'WPE snapshot proves each image was built on its declared native architecture'
has "$snapshot_workflow" 'WPE_SNAPSHOT_CREDENTIAL_BROKER' \
  'WPE snapshots use an OIDC credential broker in steady state'
has "$snapshot_workflow" 'enable-openid-connect: true' \
  'WPE snapshot jobs explicitly enable Forgejo OIDC'
has "$snapshot_workflow" '"$QOS_WPE_SIGNER" "$QOS_WPE_REF"' \
  'WPE buildroots are signed through a leased non-exportable signer'
has "$snapshot_workflow" "vars.WPE_ALLOW_STATIC_CREDENTIAL_BOOTSTRAP == '1'" \
  'exportable signing keys are isolated to an explicit bootstrap step'
has "$credential_helper" "bounded_expiry \"\$lease_expiry\" 900" \
  'brokered WPE credentials expire within fifteen minutes'
has "$credential_helper" "bounded_expiry \"\${WPE_STATIC_BOOTSTRAP_EXPIRES_AT:-}\" 86400" \
  'static WPE bootstrap has an absolute expiry no more than one day away'
has "$credential_helper" "trusted_root_executable \"\$signer\" 'WPE signer'" \
  'brokered WPE signer and its path are controlled by root'
has "$snapshot_workflow" 'token: ${{ github.token }}' \
  'WPE pin promotion uses only the job-scoped Forgejo repository token'
if grep -Fq 'token: ${{ secrets.REGISTRY_TOKEN }}' "$snapshot_workflow"; then
  bad 'WPE promotion still uses the persistent registry PAT for checkout'
else
  ok 'persistent registry PAT is not used for repository checkout or promotion'
fi
has "$snapshot_workflow" 'cosign verify --key keys/cosign.pub "$ref"' \
  'WPE buildroot promotion verifies both signatures'
has "$snapshot_workflow" 'WPE_BUILDROOT_X86_64_REF=' \
  'WPE snapshot promotes the x86-64 digest pin'
has "$snapshot_workflow" 'WPE_BUILDROOT_AARCH64_REF=' \
  'WPE snapshot promotes the ARM64 digest pin'

snapshot_inputs=(
  os/Containerfile.wpe-buildroot
  os/base-snapshot.env
  os/external-assets/github-assets.tsv
  os/dev/assert-native-arch.sh
  os/dev/cache-github-assets.sh
  os/dev/install-cached-cosign.sh
  os/dev/acquire-wpe-snapshot-credentials.sh
  keys/cosign.pub
  .forgejo/workflows/snapshot-wpe-buildroot.yml
)
trigger_block="$(sed -n '/^[[:space:]]*paths:/,/^$/p' "$snapshot_workflow")"
input_block="$(sed -n '/^[[:space:]]*WPE_SNAPSHOT_INPUTS:/,/^[[:space:]]*enable-openid-connect:/p' "$snapshot_workflow")"
has "$snapshot_workflow" "mapfile -t snapshot_inputs < <(printf '%s' \"\$WPE_SNAPSHOT_INPUTS\")" \
  'WPE promotion parses the authoritative input list without an empty pathspec'
for input in "${snapshot_inputs[@]}"; do
  grep -Fq -- "$input" <<< "$trigger_block" \
    && grep -Fq -- "$input" <<< "$input_block" \
    && ok "$input triggers a snapshot and blocks stale promotion" \
    || bad "$input is missing from the snapshot trigger or stale-promotion guard"
done
if [ "$(grep -Fc -- '"${snapshot_inputs[@]}"' "$snapshot_workflow")" -eq 2 ]; then
  ok 'both WPE promotion race checks use the complete authoritative input set'
else
  bad 'both WPE promotion race checks must use the complete authoritative input set'
fi

has "$image_workflow" 'cosign verify --key keys/cosign.pub "$ref"' \
  'OS CI verifies the promoted WPE buildroot signature'
has "$image_workflow" 'WPE_BUILDROOT_REF=${{ env.WPE_BUILDROOT_REF }}' \
  'OS CI passes the verified buildroot into every image build'
if grep -Fq 'QOS_WPE_BOOTSTRAP=1' "$image_workflow"; then
  bad 'OS CI enables the live-repository WPE bootstrap'
else
  ok 'OS CI never enables local WPE bootstrap mode'
fi
has "$image_script" 'QOS_WPE_BOOTSTRAP:=0' 'local WPE bootstrap is opt-in and off by default'
has "$image_script" 'build-wpe-buildroot.sh' 'local bootstrap has an explicit implementation'
has "$repo/.forgejo/workflows/release-imager.yml" 'QOS_REQUIRE_WPE_SIGNATURE=1' \
  'installation-medium release requires WPE buildroot signature verification'
has "$image_script" 'cosign verify --key "$QOS_REPO/keys/cosign.pub"' \
  'release helper verifies the WPE buildroot signature'

exit "$fail"
