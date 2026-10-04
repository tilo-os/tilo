#!/usr/bin/env bash
# Static contract for the digest-pinned RPM snapshot. This catches package-set
# drift before a cold image build spends minutes resolving an incomplete repo.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="$repo/.forgejo/workflows/build-rpms.yml"
snapshot="$repo/os/Containerfile.rpms"
image_workflow="$repo/.forgejo/workflows/build-image.yml"
relay_workflow="$repo/.forgejo/workflows/release-relay.yml"
build_script="$repo/os/dev/build-image.sh"
asset_manifest="$repo/os/external-assets/github-assets.tsv"
derive="$repo/os/dev/derive-rpm-package-set.py"
fail=0

ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }
has() {
  local file="$1" needle="$2" message="$3"
  grep -Fq -- "$needle" "$file" && ok "$message" || bad "$message"
}

# Checks read job text from here-strings, not `printf | grep -q`: under
# pipefail, grep exiting at its first match can SIGPIPE the writer.

# Every image which consumes the frozen repository must contribute its package
# blocks to the workflow parser and retrigger the snapshot when it changes.
mapfile -t frozen_consumers < <(
  grep -lE 'from=rpms|enablerepo=frozen|RPMS_REF' \
    "$repo/os/Containerfile" \
    "$repo/os/Containerfile.dev" \
    "$repo/os/Containerfile.host" \
    "$repo/os/containers/tilo-relay/Containerfile" \
    "$repo/os/dev/composefs/Containerfile"
)

for file in "${frozen_consumers[@]}"; do
  relative="${file#"$repo/"}"
  grep -Fxq "$relative" <<<"$("$derive" --sources)" \
    && ok "$relative contributes packages to the snapshot parser" \
    || bad "$relative consumes frozen RPMs but is absent from the snapshot parser"
  has "$workflow" "- $relative" "$relative changes retrigger the RPM snapshot"
done

# Exercise the workflow's package grammar over its declared sources. Apart
# from proving virtio-win is captured, requiring output from every source keeps
# a syntax change from silently making one whole Containerfile invisible.
mapfile -t parser_sources < <("$derive" --sources)

for relative in "${parser_sources[@]}"; do
  [ -f "$repo/$relative" ] || { bad "snapshot parser source $relative exists"; continue; }
  packages="$(PYTHONDONTWRITEBYTECODE=1 python3 - "$derive" "$repo/$relative" <<'PY'
import importlib.util, pathlib, sys
spec = importlib.util.spec_from_file_location("rpm_package_set", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
print("\n".join(sorted(set(module.packages(pathlib.Path(sys.argv[2]))))))
PY
)"
  [ -n "$packages" ] \
    && ok "$relative yields packages through the workflow grammar" \
    || bad "$relative has no package captured by the workflow grammar"
done

all_packages="$("$derive" --arch x86_64)"
for package in dbus-tools harfbuzz-icu libatomic wine wine-dxvk qemu-kvm-core \
  qemu-ui-spice-core qemu-char-spice qemu-audio-spice qemu-device-display-virtio-vga \
  libvirt-client libvirt-daemon-driver-qemu libvirt-daemon-config-nwfilter swtpm swtpm-tools \
  edk2-ovmf virtio-win freerdp xwayland-satellite virt-viewer xorriso \
  genisoimage udftools; do
  grep -Fxq "$package" <<<"$all_packages" \
    && ok "$package is in the derived frozen package set" \
    || bad "$package is missing from the derived frozen package set"
done

arm_packages="$("$derive" --arch aarch64)"
if grep -Fxq edk2-aarch64 <<<"$arm_packages" \
  && ! grep -Fxq edk2-ovmf <<<"$arm_packages" \
  && ! grep -Fxq edk2-aarch64 <<<"$all_packages"; then
  ok 'each architecture selects its native Windows VM firmware'
else
  bad 'Windows VM firmware package selection crosses architectures'
fi
has "$workflow" "--arch '\${{ matrix.arch }}'" \
  'snapshot package selection uses the explicit native matrix architecture'

has "$workflow" './os/dev/derive-rpm-package-set.py' \
  'snapshot workflow uses the invariant-tested package parser'
has "$workflow" 'runner: runner-arm64' \
  'snapshot builds ARM64 dependencies on a native runner'
snapshot_job=$(awk '/^  snapshot:/{p=1} p && /^  promote-snapshot:/{exit} p' "$workflow")
if grep -Fq 'Install and check native snapshot host tools' <<<"$snapshot_job" \
  && grep -Fq 'for tool in skopeo jq curl python3 docker base64 sha256sum' <<<"$snapshot_job" \
  && [[ $(printf '%s\n' "$snapshot_job" | grep -nF 'Install and check native snapshot host tools' | head -1 | cut -d: -f1) -lt $(printf '%s\n' "$snapshot_job" | grep -nF 'Ensure the digest-pinned base is mirrored' | head -1 | cut -d: -f1) ]]; then
  ok 'native RPM snapshot installs and checks host tools before accessing the base'
else
  bad 'native RPM snapshot may access the base without its required host tools'
fi
has "$workflow" 'platforms: ${{ matrix.platform }}' \
  'snapshot explicitly selects each native OCI platform'
has "$workflow" 'needs: [fingerprint, snapshot]' \
  'snapshot promotion waits for every native build'
has "$workflow" 'needs: [fingerprint, signing-preflight]' \
  'expensive native snapshot builds wait for signing-authority preflight'
has "$workflow" 'python3 os/dev/promote-native-index.py' \
  'snapshot promotion verifies signed native children'
has "$workflow" 'python3 os/dev/rpm-snapshot-fingerprint.py' \
  'snapshot workflow computes the package-set fingerprint'
has "$workflow" './os/dev/resolve-rpm-snapshot.sh' \
  'snapshot workflow resolves the fingerprint tag to a verified digest'
if ! grep -Fq 'workflow_call:' "$workflow"; then
  ok 'snapshot signing runs only in standalone workflows with OIDC'
else
  bad 'snapshot signing is exposed through a reusable workflow without OIDC'
fi
has "$workflow" '--tag "$FINGERPRINT_TAG"' \
  'snapshot promotion publishes the set-<fingerprint> tag'
has "$workflow" 'needs: [fingerprint, signing-preflight]' \
  'expensive native snapshot builds wait for fingerprint and signing preflight'
if ! grep -Eq 'open-snapshot-pr\.py|reconcile-snapshot-pins\.py|rpms-snapshot\.env' "$workflow" \
   && grep -Fq 'set-' "$workflow"; then
  ok 'snapshot workflow publishes by fingerprint tag with no pin pull request'
else
  bad 'snapshot workflow still opens or reconciles a hand-kept pin'
fi
kernel_workflow="$repo/.forgejo/workflows/build-kernel.yml"
if ! grep -Eq 'RPMS_REF|rpms-snapshot\.env|resolve-rpm-snapshot' "$kernel_workflow" \
   && ! grep -Eq 'from=rpms|RPMS_REF' "$repo/os/kernel/Containerfile"; then
  ok 'the kernel remains independent of the frozen RPM snapshot'
else
  bad 'the kernel still depends on the frozen RPM snapshot'
fi
if grep -Fq 'actions/workflows/build-image.yml/dispatches' "$workflow"; then
  bad 'snapshot promotion dispatches an image build before resolve completes'
else
  ok 'snapshot promotion leaves image builds to their ensure-rpms dependency'
fi
has "$repo/os/dev/configure-cosign-kms.sh" 'COSIGN_KEY_REF must name a non-exportable KMS/HSM key' \
  'snapshot signing requires a non-exportable key'
if COSIGN_KEY_REF='cosign.key' sh -c '. "$1"' fixture "$repo/os/dev/configure-cosign-kms.sh" >/dev/null 2>&1; then
  bad 'a file-backed signing key is accepted'
else
  ok 'every signing step refuses a key outside a KMS/HSM'
fi
has "$workflow" '. os/dev/configure-cosign-kms.sh' \
  'snapshot signing configures the KMS provider from its trusted key reference'
oidc_fixture="$(mktemp -d)"
printf '{"value":"e30.eyJzdWIiOiJ0ZXN0LXN1YmplY3QifQ.signature"}\n' \
  > "$oidc_fixture/token&audience=tilo-release-signing"
aws_config="$(COSIGN_KEY_REF='awskms:///arn:aws:kms:eu-west-2:123456789012:key/example' \
  AWS_SIGNING_ROLE_ARN='arn:aws:iam::123456789012:role/release' \
  ACTIONS_ID_TOKEN_REQUEST_URL="file://$oidc_fixture/token" \
  ACTIONS_ID_TOKEN_REQUEST_TOKEN=request-token RUNNER_TEMP="$oidc_fixture" \
  AWS_ACCESS_KEY_ID=ambient-cache-user \
  sh -c '. "$1"; printf "%s:%s:%s:%s:%s:" "$AWS_REGION" "$AWS_DEFAULT_REGION" \
    "$AWS_ROLE_ARN" "${AWS_ACCESS_KEY_ID-unset}" \
    "$(stat -c %a "$AWS_WEB_IDENTITY_TOKEN_FILE")"; \
    cat "$AWS_WEB_IDENTITY_TOKEN_FILE"' fixture "$repo/os/dev/configure-cosign-kms.sh")"
rm -rf -- "$oidc_fixture"
[ "$aws_config" = 'Using Forgejo OIDC subject: test-subject
eu-west-2:eu-west-2:arn:aws:iam::123456789012:role/release:unset:600:e30.eyJzdWIiOiJ0ZXN0LXN1YmplY3QifQ.signature' ] \
  && ok 'AWS KMS setup derives the region and installs least-exposure web identity' \
  || bad 'AWS KMS setup does not configure Forgejo web identity safely'
if COSIGN_KEY_REF='awskms:///arn:aws:kms:eu-west-2:123456789012:key/example' \
   AWS_SIGNING_ROLE_ARN='arn:aws:iam::999999999999:role/release' \
   ACTIONS_ID_TOKEN_REQUEST_URL=unused ACTIONS_ID_TOKEN_REQUEST_TOKEN=unused \
   sh -c '. "$1"' fixture "$repo/os/dev/configure-cosign-kms.sh" >/dev/null 2>&1; then
  bad 'a signing role from outside the KMS account is accepted'
else
  ok 'AWS signing role is bound to the KMS key account'
fi
if COSIGN_KEY_REF='awskms:///not-an-arn' \
   sh -c '. "$1"' fixture "$repo/os/dev/configure-cosign-kms.sh" >/dev/null 2>&1; then
  bad 'malformed AWS KMS key references are accepted'
else
  ok 'malformed AWS KMS key references fail closed'
fi
if grep -Fq 'run: os/dev/require-cosign-authority.sh' "$workflow" \
   && grep -Fq 'needs: [fingerprint, signing-preflight]' "$workflow"; then
  ok 'snapshot construction starts only after KMS authority matches the pinned public key'
else
  bad 'snapshot construction can start before its signing authority is identity-bound'
fi
has "$workflow" 'https://tilo.sh/buildtypes/frozen-rpm-index/v1' \
  'snapshot provenance binds the package-set build type'
has "$workflow" 'cosign attest --yes --key "$COSIGN_KEY_REF" --type slsaprovenance1' \
  'snapshot publishes signed SLSA provenance'
has "$workflow" 'if type == "array" then .[] else . end' \
  'snapshot verification accepts current object and legacy array Cosign output'
resolve_script="$repo/os/dev/resolve-rpm-snapshot.sh"
has "$resolve_script" 'frozen RPM provenance does not bind its package set, digest, base, and architectures' \
  'fingerprint resolve verifies exact RPM snapshot provenance'
has "$resolve_script" 'derive-rpm-package-set.py" --arch x86_64' \
  'fingerprint resolve derives the explicit x86-64 RPM package set'
has "$resolve_script" 'derive-rpm-package-set.py" --arch aarch64' \
  'fingerprint resolve derives the explicit ARM64 RPM package set'
has "$resolve_script" '.buildDefinition.externalParameters.packageSetSha256ByArchitecture' \
  'fingerprint resolve verifies architecture-specific RPM package-set provenance'
has "$image_workflow" 'Resolve RPMS_REF (frozen package snapshot)' \
  'OS CI resolves RPMS_REF by fingerprint before building'
has "$repo/os/dev/await-rpm-snapshot.sh" 'resolve="$repo/os/dev/resolve-rpm-snapshot.sh"' \
  'waiting for an in-flight snapshot still resolves through the verified resolver'
for consumer in "$image_workflow" "$relay_workflow"; do
  resolver_job=$(awk '{sub(/\r$/, "")} /^  ensure-rpms:/{p=1;next} p && /^  [[:alnum:]_-]+:/{exit} p' "$consumer")
  if grep -Fq 'Resolve the exact signed RPM snapshot fingerprint' <<<"$resolver_job" \
    && grep -Fq './os/dev/await-rpm-snapshot.sh' <<<"$resolver_job" \
    && grep -Fq 'rpms_ref: ${{ steps.resolve.outputs.rpms_ref }}' <<<"$resolver_job" \
    && ! grep -Fq 'uses: ./.forgejo/workflows/build-rpms.yml' <<<"$resolver_job" \
    && ! grep -Fq 'enable-openid-connect: true' <<<"$resolver_job"; then
    ok "$(basename "$consumer") resolves the signed RPM fingerprint without reusable OIDC"
  else
    bad "$(basename "$consumer") may sign in a reusable job or accept an unverified RPM snapshot"
  fi
done
has "$build_script" 'frozen RPM provenance does not bind its package set, digest, base, and architectures' \
  'local OS builds verify exact RPM snapshot provenance'
has "$workflow" '- os/dev/derive-rpm-package-set.py' \
  'package parser changes retrigger the snapshot'

has "$workflow" '- os/external-assets/github-assets.tsv' \
  'pinned external asset changes retrigger the snapshot'
has "$workflow" 'uses: ./.forgejo/actions/cache-github-assets' \
  'snapshot materializes checksum-verified external inputs'
has "$workflow" 'github-assets=os/external-assets/cache' \
  'snapshot receives only the materialized external-input cache'
has "$snapshot" '/tilo-assets/virtio-win-0.1.302-1.noarch.rpm' \
  'snapshot caches the content-pinned virtio RPM for selective extraction'
has "$repo/os/Containerfile" '# tilo-rpm-snapshot-package: virtio-win' \
  'selectively extracted virtio media remains in the frozen package manifest'
if grep -q '^    virtio-win \\$' "$repo/os/Containerfile"; then
  bad 'the full duplicated virtio-win RPM payload is still installed in the OS'
else
  ok 'only the required virtio-win media is copied into the OS rootfs'
fi
has "$asset_manifest" $'2f58eea024e0221a873032761dc3ed3262409cd80860b7b27d3d5a0f206616c9\tvirtio-win-0.1.302-1.noarch.rpm\t' \
  'virtio RPM has a reviewed SHA-256 pin'
if grep -Eq 'fedorapeople.org/.*/(stable|latest)/' "$asset_manifest" "$snapshot"; then
  bad 'virtio RPM uses a moving upstream repository'
else
  ok 'virtio RPM uses an immutable archived version URL'
fi

packages_line="$(grep -F 'dnf install -y --best --store=/tmp/tilo-install' "$snapshot" || true)"
if [ -n "$packages_line" ] \
  && [[ "$packages_line" == *'${PACKAGES}'* ]] \
  && [[ "$packages_line" != *'--skip-unavailable'* ]]; then
  ok 'snapshot transaction resolves the complete package set without skipping unavailable RPMs'
else
  bad 'snapshot transaction can omit unavailable packages or does not consume PACKAGES'
fi

first_instruction="$(grep -m1 -E '^(ARG|FROM) ' "$snapshot")"
[ "$first_instruction" = 'ARG BASE_REF' ] \
  && ok 'BASE_REF is globally declared before the first snapshot stage' \
  || bad 'BASE_REF must be declared before the first FROM'
# --downloadonly skips RPMs the base already has. The final image can update a
# package whose matching dependency was omitted, or erase and reinstall a base
# package. --resolve --alldeps preserves that complete offline closure.
has "$snapshot" 'dnf download -y --resolve --alldeps --destdir=/repo --exclude='"'"'qemu-user-static*'"'"' ${PACKAGES}' \
  'snapshot keeps the dependency closure even when packages are base-installed'
has "$snapshot" '/repo/tilo-direct-package-specs.txt' \
  'snapshot publishes its direct package manifest'
has "$snapshot" 'dnf upgrade -y --best --store=/tmp/tilo-upgrade' \
  'snapshot includes upgrades of installed direct specs and their reverse dependencies'
has "$snapshot" 'dnf install -y --best --downloadonly --setopt=reposdir=/repo --setopt=install_weak_deps=True ${PACKAGES}' \
  'snapshot proves the best frozen transaction resolves offline without skipped specs'
has "$image_workflow" 'python3 os/dev/verify-rpm-snapshot-buildkit.py "$RPMS_REF"' \
  'production CI rejects a stale frozen package manifest before building'
has "$resolve_script" 'registry.tilo.so/tilo/rpms@$digest' \
  'fingerprint resolve returns a digest-pinned RPM registry path'
has "$relay_workflow" 'python3 os/dev/verify-rpm-snapshot-buildkit.py "$RPMS_REF"' \
  'relay CI rejects a stale frozen package manifest before building'
has "$build_script" 'verify-rpm-snapshot-package-set.sh" podman "$RPMS_REF"' \
  'local production builds reject a stale frozen package manifest'
has "$build_script" '^registry\.tilo\.so/tilo/rpms@sha256:[0-9a-f]{64}$' \
  'local builds accept only the real digest-pinned RPM registry path'

if grep -Rqs --exclude-dir=output --include='Containerfile*' 'DROP_RPMDB_WAL' \
  "$repo/os" || grep -Fq 'DROP_RPMDB_WAL' "$workflow"; then
  bad 'obsolete DROP_RPMDB_WAL workaround remains'
else
  ok 'obsolete DROP_RPMDB_WAL workaround is absent'
fi

python3 "$repo/os/tests/test_rpm_snapshot_fingerprint.py" \
  && ok "fingerprint resolution preserves package-set addressing without a hand-kept pin" \
  || bad "fingerprint resolution regression"

exit "$fail"
