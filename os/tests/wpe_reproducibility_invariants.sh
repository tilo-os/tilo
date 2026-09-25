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
arch_helper="$repo/os/dev/assert-native-arch.sh"
image_workflow="$repo/.forgejo/workflows/build-image.yml"
fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }
has() { grep -Fq -- "$2" "$1" && ok "$3" || bad "$3"; }

bash -n "$build_script" "$image_script" "$repo/os/dev/build-wpe-buildroot.sh" \
  "$arch_helper" \
  && ok 'WPE build and bootstrap scripts parse' \
  || bad 'a WPE build or bootstrap script does not parse'

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
has "$build_script" 'TILO_WPE_OFFLINE' 'WPE build supports a fail-closed offline source mode'
budget_tmp="$(mktemp -d)"
for jobs in 0 00 -1 invalid; do
  if JOBS="$jobs" TILO_WPE_CACHE="$budget_tmp/cache" TILO_WPE_OFFLINE=1 \
      bash "$build_script" verify-sources >"$budget_tmp/log" 2>&1; then
    bad "WPE accepted invalid job budget: $jobs"
  elif grep -Fq 'JOBS must be a positive integer' "$budget_tmp/log" \
      && [ ! -e "$budget_tmp/cache" ]; then
    ok "WPE rejects job budget $jobs before touching build inputs"
  else
    bad "WPE job budget $jobs did not fail before build setup"
  fi
done
rm -f "$budget_tmp/log"
rmdir "$budget_tmp"
has "$image_script" 'WPE_BUILD_JOBS=${TILO_WPE_BUILD_JOBS:-8}' \
  'image builds forward the explicit browser job budget'
has "$container" 'TILO_WPE_SOURCE_DIR=/tilo-assets TILO_WPE_OFFLINE=1' \
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
has "$buildroot" '/usr/lib/tilo/wpe-buildroot-packages.txt' \
  'promoted WPE buildroot records its exact installed package set'

published_pattern='^code\.podesta\.ai/qos/tilo-wpe-buildroot@sha256:[0-9a-f]{64}$'
for consumer in "$image_script" "$image_workflow" "$snapshot_workflow"; do
  has "$consumer" "$published_pattern" 'WPE verifier accepts the namespace actually published by the snapshot'
done
has "$snapshot_workflow" 'ref="code.podesta.ai/qos/tilo-wpe-buildroot@$DIGEST"' \
  'WPE producer and verifiers share the same repository'
digest=$(printf 'a%.0s' {1..64})
[[ "code.podesta.ai/qos/tilo-wpe-buildroot@sha256:$digest" =~ $published_pattern ]] \
  && ok 'published immutable WPE reference is accepted' || bad 'published WPE reference is rejected'
for invalid in "code.podesta.ai/tilo/tilo-wpe-buildroot@sha256:$digest" \
    'code.podesta.ai/qos/tilo-wpe-buildroot:latest'; do
  [[ ! "$invalid" =~ $published_pattern ]] \
    && ok 'wrong namespace or mutable WPE reference is rejected' || bad 'invalid WPE reference accepted'
done

for arch in X86_64 AARCH64; do
  value="$(sed -n "s/^WPE_BUILDROOT_${arch}_REF=//p" "$pins")"
  if [ -z "$value" ] || [[ "$value" =~ ^code\.podesta\.ai/qos/tilo-wpe-buildroot@sha256:[0-9a-f]{64}$ ]]; then
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
has "$snapshot_workflow" 'enable-openid-connect: true' \
  'WPE snapshot signing jobs explicitly enable Forgejo OIDC for the KMS role'
has "$snapshot_workflow" 'cmp --silent keys/cosign.pub "$resolved"' \
  'WPE snapshot refuses a KMS authority that differs from keys/cosign.pub'
has "$snapshot_workflow" '. os/dev/configure-cosign-kms.sh' \
  'WPE snapshot signs through the shared KMS configuration'
has "$snapshot_workflow" 'cosign sign --yes --key "$COSIGN_KEY_REF" "$TILO_WPE_REF"' \
  'WPE buildroots are signed with the non-exportable release key'
has "$snapshot_workflow" 'gcpkms://*|awskms://*|azurekms://*|hashivault://*' \
  'WPE snapshot accepts only a KMS/HSM signing key reference'
if grep -Eq 'COSIGN_PRIVATE_KEY|COSIGN_PASSWORD|WPE_SNAPSHOT_CREDENTIAL_BROKER' "$snapshot_workflow"; then
  bad 'WPE snapshot retains an exportable key or retired credential broker path'
else
  ok 'WPE snapshot has no exportable signing key path'
fi
has "$snapshot_workflow" 'REGISTRY_PASSWORD: ${{ secrets.REGISTRY_TOKEN }}' \
  'WPE snapshot authenticates to the registry like the other snapshot workflows'
has "$snapshot_workflow" 'token: ${{ github.token }}' \
  'WPE pin branch is pushed with the job-scoped Forgejo token'
has "$snapshot_workflow" 'FORGEJO_TOKEN: ${{ secrets.RELEASE_TOKEN }}' \
  'WPE pin pull request is opened with the repository-scoped release token'
has "$snapshot_workflow" 'branch="automation/wpe-buildroot-snapshot-${snapshot_digest:0:16}"' \
  'WPE pins land through a digest-named branch'
has "$snapshot_workflow" 'python3 os/dev/open-snapshot-pr.py' \
  'WPE pins land through a protected-main pull request'
if grep -Fq 'git push origin HEAD:main' "$snapshot_workflow"; then
  bad 'WPE promotion still pushes directly to protected main'
else
  ok 'WPE promotion never pushes directly to protected main'
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
  os/dev/configure-cosign-kms.sh
  os/dev/login-registry-with-retry.sh
  os/dev/open-snapshot-pr.py
  .forgejo/actions/install-cosign
  keys/cosign.pub
  .forgejo/workflows/snapshot-wpe-buildroot.yml
)
trigger_block="$(sed -n '/^[[:space:]]*paths:/,/^$/p' "$snapshot_workflow")"
input_block="$(sed -n '/^[[:space:]]*WPE_SNAPSHOT_INPUTS:/,/^[[:space:]]*steps:/p' "$snapshot_workflow")"
has "$snapshot_workflow" "mapfile -t snapshot_inputs < <(printf '%s' \"\$WPE_SNAPSHOT_INPUTS\")" \
  'WPE promotion parses the authoritative input list without an empty pathspec'
for input in "${snapshot_inputs[@]}"; do
  grep -Fq -- "$input" <<< "$trigger_block" \
    && grep -Fq -- "$input" <<< "$input_block" \
    && ok "$input triggers a snapshot and blocks stale promotion" \
    || bad "$input is missing from the snapshot trigger or stale-promotion guard"
done
if grep -Fq 'git diff --quiet "$GITHUB_SHA..origin/main" -- "${snapshot_inputs[@]}"' "$snapshot_workflow" \
   && grep -Fq 'git checkout --detach origin/main' "$snapshot_workflow"; then
  ok 'WPE promotion refuses stale inputs and commits the pins on the checked main tip'
else
  bad 'WPE promotion must refuse stale inputs and commit on the checked main tip'
fi

has "$image_workflow" 'cosign verify --key keys/cosign.pub "$ref"' \
  'OS CI verifies the promoted WPE buildroot signature'
has "$image_workflow" 'WPE_BUILDROOT_REF=${{ env.WPE_BUILDROOT_REF }}' \
  'OS CI passes the verified buildroot into every image build'
if grep -Fq 'TILO_WPE_BOOTSTRAP=1' "$image_workflow"; then
  bad 'OS CI enables the live-repository WPE bootstrap'
else
  ok 'OS CI never enables local WPE bootstrap mode'
fi
has "$image_script" 'TILO_WPE_BOOTSTRAP:=0' 'local WPE bootstrap is opt-in and off by default'
has "$image_script" 'build-wpe-buildroot.sh' 'local bootstrap has an explicit implementation'
if grep -Fq './os/dev/use-published-image.sh "$TILO_PUBLISHED_OS_IMAGE"' \
     "$repo/.forgejo/workflows/release-imager.yml" \
   && ! grep -Fq './os/dev/build-image.sh' "$repo/.forgejo/workflows/release-imager.yml" \
   && grep -Fq 'cosign verify --key "$verify_key" "$image"' \
     "$repo/os/dev/use-published-image.sh"; then
  ok 'installation-medium release consumes the signed OS whose build verified WPE'
else
  bad 'installation-medium release can rebuild with or consume an unverified WPE input'
fi
has "$image_script" 'cosign verify --key "$TILO_REPO/keys/cosign.pub"' \
  'release helper verifies the WPE buildroot signature'

exit "$fail"
