#!/usr/bin/env bash
# WPE source and buildroot inputs must be immutable before an OS build starts.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
container="$repo/os/Containerfile"
wpe_recipe="$repo/os/Containerfile.wpe"
buildroot="$repo/os/Containerfile.wpe-buildroot"
build_script="$repo/os/dev/build-wpe.sh"
image_script="$repo/os/dev/build-image.sh"
manifest="$repo/os/external-assets/github-assets.tsv"
pins="$repo/os/wpe-buildroot-snapshot.env"
snapshot_workflow="$repo/.forgejo/workflows/build-buildroots.yml"
snapshot_script="$repo/os/dev/buildroot-snapshot.sh"
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
has "$wpe_recipe" 'TILO_WPE_SOURCE_DIR=/tilo-assets TILO_WPE_OFFLINE=1' \
  'production WPE compilation uses only the verified external cache'
for archive in libwpe-1.16.3.tar.xz wpebackend-fdo-1.16.1.tar.xz wpewebkit-2.52.6.tar.xz; do
  has "$wpe_recipe" "COPY --from=github-assets /$archive /" \
    "$archive is the only kind of asset copied into the WPE stage"
done
if grep -Fq 'COPY --from=github-assets / /' "$wpe_recipe"; then
  bad 'unrelated external assets invalidate the WPE compile cache'
else
  ok 'unrelated external assets stay out of the WPE compile cache key'
fi
key_fixture="$(mktemp -d)"
mkdir -p "$key_fixture/os/dev" "$key_fixture/os/external-assets"
cp "$repo/os/dev/wpe-stage-key.sh" "$key_fixture/os/dev/"
cp "$repo/os/dev/build-wpe.sh" "$key_fixture/os/dev/"
cp "$wpe_recipe" "$key_fixture/os/Containerfile.wpe"
cp "$pins" "$key_fixture/os/wpe-buildroot-snapshot.env"
cp "$manifest" "$key_fixture/os/external-assets/github-assets.tsv"
key_before="$(bash "$key_fixture/os/dev/wpe-stage-key.sh" x86_64)"
legacy_before="$(bash "$key_fixture/os/dev/wpe-stage-key.sh" x86_64 --legacy)"
printf '%064d\tunrelated-test.tar.xz\thttps://example.invalid/unrelated\n' 0 \
  >> "$key_fixture/os/external-assets/github-assets.tsv"
key_after="$(bash "$key_fixture/os/dev/wpe-stage-key.sh" x86_64)"
legacy_after="$(bash "$key_fixture/os/dev/wpe-stage-key.sh" x86_64 --legacy)"
[[ $key_before == "$key_after" && $legacy_before != "$legacy_after" ]] \
  && ok 'unrelated asset rows do not invalidate the WPE stage key' \
  || bad 'unrelated asset row changed the WPE stage key'
sed -i 's/^b2bafef2751625b7fdf530f230ff0f542ff0eeba3590c3a989d931b2a55c858e\t/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\t/' \
  "$key_fixture/os/external-assets/github-assets.tsv"
key_changed="$(bash "$key_fixture/os/dev/wpe-stage-key.sh" x86_64)"
[[ $key_changed != "$key_after" ]] \
  && ok 'WPE archive digest changes invalidate the WPE stage key' \
  || bad 'WPE archive digest change failed to invalidate the stage key'
cp "$snapshot_script" "$key_fixture/os/dev/buildroot-snapshot.sh"
git -C "$key_fixture" init -q
git -C "$key_fixture" config user.name 'WPE key fixture'
git -C "$key_fixture" config user.email 'wpe-fixture@tilo.invalid'
git -C "$key_fixture" add .
git -C "$key_fixture" commit -qm baseline
fixture_before="$(git -C "$key_fixture" rev-parse HEAD)"
printf '%064d\tanother-unrelated.tar.xz\thttps://example.invalid/another\n' 0 \
  >> "$key_fixture/os/external-assets/github-assets.tsv"
git -C "$key_fixture" add .
git -C "$key_fixture" commit -qm unrelated
fixture_after="$(git -C "$key_fixture" rev-parse HEAD)"
stage_changes="$(cd "$key_fixture" && bash os/dev/buildroot-snapshot.sh changed "$fixture_before" "$fixture_after")"
[[ $stage_changes == '[]' ]] \
  && ok 'unrelated asset changes do not launch WPE buildroot or engine builds' \
  || bad "unrelated asset change launched WPE builds: $stage_changes"
sed -i 's/^aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\t/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\t/' \
  "$key_fixture/os/external-assets/github-assets.tsv"
git -C "$key_fixture" add .
git -C "$key_fixture" commit -qm wpe-source
fixture_wpe="$(git -C "$key_fixture" rev-parse HEAD)"
stage_changes="$(cd "$key_fixture" && bash os/dev/buildroot-snapshot.sh changed "$fixture_after" "$fixture_wpe")"
[[ $stage_changes == '["wpe-stage"]' ]] \
  && ok 'WPE archive changes launch the signed WPE stage build' \
  || bad "WPE archive change did not launch the signed stage build: $stage_changes"
rm -rf "$key_fixture"
has "$container" 'os/Containerfile.wpe' \
  'OS recipe points at the isolated WPE compile recipe'
if ! grep -Fq 'bash /build-wpe.sh build' "$container"; then ok 'OS Containerfile does not compile WPE'; else bad 'OS Containerfile still compiles WPE'; fi
if grep -Fq 'bash /build-wpe.sh build' "$wpe_recipe"; then ok 'isolated WPE recipe compiles the engine'; else bad 'isolated WPE recipe missing compile'; fi

wpe_stage="$(cat "$wpe_recipe")"
if [ -n "$wpe_stage" ] && ! grep -Eq '\bdnf\b|/etc/yum\.repos\.d' <<<"$wpe_stage"; then
  ok 'production WPE stage has no Fedora repository transaction'
else
  bad 'production WPE stage can still access Fedora repositories'
fi
has "$wpe_recipe" 'FROM ${WPE_BUILDROOT_REF} AS wpe-builder-1' \
  'production WPE stage starts from the promoted buildroot'
has "$buildroot" 'dnf builddep -y webkitgtk6.0' \
  'dedicated snapshot resolves the Fedora WebKit BuildRequires'
has "$buildroot" 'find /etc/yum.repos.d -type f -delete' \
  'promoted WPE buildroot disables Fedora repositories'
has "$buildroot" '/usr/lib/tilo/wpe-buildroot-packages.txt' \
  'promoted WPE buildroot records its exact installed package set'

published_pattern='^registry\.tilo\.so/tilo/tilo-wpe-buildroot@sha256:[0-9a-f]{64}$'
for consumer in "$image_script" "$image_workflow"; do
  has "$consumer" "$published_pattern" 'WPE verifier accepts the namespace actually published by the snapshot'
done
has "$snapshot_script" 'image="registry.tilo.so/tilo/tilo-$1-buildroot"' \
  'WPE producer and verifiers share the same repository'
has "$snapshot_workflow" 'tags: registry.tilo.so/tilo/tilo-${{ matrix.kind }}-buildroot:' \
  'WPE producer publishes into the repository its verifiers accept'
digest=$(printf 'a%.0s' {1..64})
[[ "registry.tilo.so/tilo/tilo-wpe-buildroot@sha256:$digest" =~ $published_pattern ]] \
  && ok 'published immutable WPE reference is accepted' || bad 'published WPE reference is rejected'
for invalid in "registry.tilo.so/other/tilo-wpe-buildroot@sha256:$digest" \
    'registry.tilo.so/tilo/tilo-wpe-buildroot:latest'; do
  [[ ! "$invalid" =~ $published_pattern ]] \
    && ok 'wrong namespace or mutable WPE reference is rejected' || bad 'invalid WPE reference accepted'
done

for arch in X86_64 AARCH64; do
  value="$(sed -n "s/^WPE_BUILDROOT_${arch}_REF=//p" "$pins")"
  if [ -z "$value" ] || [[ "$value" =~ ^registry\.tilo\.so/tilo/tilo-wpe-buildroot@sha256:[0-9a-f]{64}$ ]]; then
    ok "$arch buildroot pin is either explicit bootstrap-empty or digest-pinned"
  else
    bad "$arch WPE buildroot uses a mutable or malformed reference"
  fi
done

has "$snapshot_workflow" '"runner-arm64"' 'WPE snapshot builds on a native ARM64 runner'
has "$snapshot_workflow" 'else "runner" end' 'WPE snapshot builds on the native x86-64 runner'
has "$snapshot_workflow" '"runner-large-arm64"' 'the WPE engine stage compiles on the large native ARM64 runner'
has "$snapshot_workflow" 'target: wpe-stage' 'the WPE engine stage is published from os/Containerfile.wpe'
has "$repo/.forgejo/workflows/build-image.yml" "format('wpe-builder=docker-image://{0}', env.TILO_WPE_STAGE_REF)" 'OS builds take a pinned WPE engine stage'
has "$repo/.forgejo/workflows/build-image.yml" 'if [[ $label == "$key" || $label == "$legacy_key" ]]; then' 'OS builds take only a stage whose scoped or exact legacy inputs match'
has "$snapshot_workflow" './os/dev/assert-native-arch.sh "${{ matrix.arch }}"' \
  'WPE snapshot fails early when a native runner label points at the wrong architecture'
has "$snapshot_workflow" "if: github.ref == 'refs/heads/main'" \
  'WPE snapshot cannot publish from a feature branch'
has "$snapshot_workflow" 'no-cache: true' 'WPE snapshot refresh cannot reuse a stale Fedora transaction layer'
has "$snapshot_script" 'actual_arch="$(docker run --rm --entrypoint cat "$ref"' \
  'WPE snapshot proves each image was built on its declared native architecture'
has "$snapshot_workflow" 'enable-openid-connect: true' \
  'WPE snapshot signing jobs explicitly enable Forgejo OIDC for the KMS role'
has "$snapshot_workflow" 'run: os/dev/require-cosign-authority.sh' \
  'WPE snapshot refuses a KMS authority that differs from keys/cosign.pub'
has "$snapshot_workflow" '. os/dev/configure-cosign-kms.sh' \
  'WPE snapshot signs through the shared KMS configuration'
has "$snapshot_workflow" 'cosign sign --yes --key "$COSIGN_KEY_REF" "$TILO_BUILDROOT_REF"' \
  'WPE buildroots are signed with the non-exportable release key'
has "$repo/os/dev/configure-cosign-kms.sh" 'gcpkms://*|awskms://*|azurekms://*|hashivault://*' \
  'WPE snapshot accepts only a KMS/HSM signing key reference'
if grep -Eq 'COSIGN_PRIVATE_KEY|COSIGN_PASSWORD|WPE_SNAPSHOT_CREDENTIAL_BROKER' "$snapshot_workflow" "$snapshot_script"; then
  bad 'WPE snapshot retains an exportable key or retired credential broker path'
else
  ok 'WPE snapshot has no exportable signing key path'
fi
has "$snapshot_workflow" 'registry: registry.tilo.so' \
  'WPE snapshot authenticates to the registry like the other snapshot workflows'
has "$snapshot_workflow" 'token: ${{ github.token }}' \
  'WPE pin branch is pushed with the job-scoped Forgejo token'
has "$snapshot_workflow" 'FORGEJO_TOKEN: ${{ secrets.RELEASE_TOKEN }}' \
  'WPE pin pull request is opened with the repository-scoped release token'
has "$snapshot_script" 'branch="automation/$kind-buildroot-snapshot-${snapshot_digest:0:16}"' \
  'WPE pins land through a digest-named branch'
has "$snapshot_script" 'python3 os/dev/open-snapshot-pr.py' \
  'WPE pins land through a protected-main pull request'
if grep -Fq 'git push origin HEAD:main' "$snapshot_workflow" "$snapshot_script"; then
  bad 'WPE promotion still pushes directly to protected main'
else
  ok 'WPE promotion never pushes directly to protected main'
fi
has "$snapshot_script" 'cosign verify --key keys/cosign.pub "$ref"' \
  'WPE buildroot promotion verifies both signatures'
has "$snapshot_script" "x86_64) printf '%s_X86_64_REF' \"\$key\" ;;" \
  'WPE snapshot promotes the x86-64 digest pin'
has "$snapshot_script" "aarch64) printf '%s_AARCH64_REF' \"\$key\" ;;" \
  'WPE snapshot promotes the ARM64 digest pin'

# The script owns each buildroot's inputs: every one must trigger the workflow
# and block a stale promotion.
mapfile -t snapshot_inputs < <(
  sed -n '/^common_inputs=(/,/^)/p; /^wpe_inputs=(/,/^)/p; /^wpe_stage_inputs=(/,/^)/p' "$snapshot_script" | grep -Ev '^[a-z_]+=\(|^\)$' | tr -d ' '
)
[ "${#snapshot_inputs[@]}" -ge 16 ] && ok 'WPE snapshot declares its inputs' || bad 'WPE snapshot input list is incomplete'
trigger_block="$(sed -n '/^[[:space:]]*paths:/,/^$/p' "$snapshot_workflow")"
grep -Fq -- 'os/external-assets/github-assets.tsv' <<< "$trigger_block" \
  && ok 'asset manifest changes run the WPE-specific plan' \
  || bad 'asset manifest changes do not run the WPE-specific plan'
for input in "${snapshot_inputs[@]}"; do
  grep -Fq -- "$input" <<< "$trigger_block" \
    && ok "$input triggers a snapshot and blocks stale promotion" \
    || bad "$input is missing from the snapshot trigger"
done
if grep -Fq 'git diff --quiet "$GITHUB_SHA..origin/main" -- "${inputs[@]}"' "$snapshot_script" \
   && grep -Fq 'git checkout --detach origin/main' "$snapshot_script"; then
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
has "$snapshot_script" 'cmp -s pinned-packages.txt new-packages.txt' \
  'a WPE buildroot refresh compares its package set with the pinned buildroot'
has "$snapshot_script" 'echo "::notice::both buildroots install exactly the pinned packages; keeping the pins and the $label layer cache"' \
  'an unchanged WPE package set keeps the pins, so the engine layer cache stays valid'

exit "$fail"
