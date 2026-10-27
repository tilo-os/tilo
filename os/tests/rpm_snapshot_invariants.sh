#!/usr/bin/env bash
# Static contract for the digest-pinned RPM snapshot. This catches package-set
# drift before a cold image build spends minutes resolving an incomplete repo.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# t builds, signs and publishes the snapshot (t.toml's image-rpms-<arch>, image-rpms,
# provenance-rpms, sign-rpms, publish-rpms, generated from tools/ci/products.toml).
t_toml="$repo/t.toml"
rpms_build="$repo/tools/ci/publish/rpms-build.sh"
rpms_provenance="$repo/tools/ci/publish/rpms-provenance.sh"
snapshot="$repo/os/Containerfile.rpms"
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
# A t check's field, one value per line (a string as itself, an array per element, a table as
# key=value); fails when
# t.toml declares no such check, so a renamed check cannot pass unobserved.
tcheck() {
  python3 - "$t_toml" "$1" "$2" <<'PY'
import sys, tomllib
config = tomllib.load(open(sys.argv[1], "rb"))
for check in config.get("check", []):
    if check.get("name") == sys.argv[2]:
        value = check.get(sys.argv[3], [])
        if isinstance(value, dict):
            value = [f"{key}={item}" for key, item in value.items()]
        print("\n".join(map(str, value)) if isinstance(value, list) else value)
        sys.exit(0)
sys.exit(f"t.toml declares no check {sys.argv[2]}")
PY
}
# has_t CHECK FIELD LINE MESSAGE: the field has LINE as one of its values (an array) or one of
# its lines (a run script).
has_t() {
  local values
  values="$(tcheck "$1" "$2")" && grep -Fxq -- "$3" <<<"$values" && ok "$4" || bad "$4"
}
for check in image-rpms-{arch} image-rpms provenance-rpms sign-rpms publish-rpms rpm-snapshot; do
  tcheck "$check" name >/dev/null || bad "t.toml declares $check"
done

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
  has_t image-rpms-{arch} inputs "$relative" "$relative changes rebuild the RPM snapshot"
  has_t provenance-rpms inputs "$relative" "$relative changes the snapshot's fingerprint and provenance"
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
  edk2-ovmf virtio-win freerdp xorg-x11-server-Xwayland virt-viewer xorriso \
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
has "$rpms_build" 'packages=$(python3 os/dev/derive-rpm-package-set.py --arch "$arch"' \
  'snapshot package selection uses the invariant-tested parser for the explicit native architecture'
# Each architecture's snapshot is built on a node of that architecture, and refuses any other.
has_t image-rpms-{arch} requires 'arch={arch}' \
  'snapshot builds each architecture on a native node'
has "$rpms_build" '[[ $(uname -m) == "$arch" ]] || die' \
  'snapshot build refuses a foreign architecture'
has "$rpms_build" '--platform "$platform"' \
  'snapshot explicitly selects each native OCI platform'
has_t image-rpms needs 'image-rpms-x86_64' 'the snapshot index waits for the x86-64 build'
has_t image-rpms needs 'image-rpms-aarch64' 'the snapshot index waits for the ARM64 build'
for need in image-rpms image-rpms-x86_64 image-rpms-aarch64 provenance-rpms; do
  has_t sign-rpms needs "$need" "snapshot signing waits for $need"
done
has_t publish-rpms needs sign-rpms 'snapshot publication waits for its signatures'
# The signing check signs both native children and the index, each against the digest its
# build recorded, and verifies each signature against keys/cosign.pub before anything publishes.
for out in x86_64 aarch64 index; do
  has_t sign-rpms run "bash tools/ci/publish/oci-sign.sh registry.tilo.so/tilo/rpms /target/publish/rpms/$out" \
    "snapshot signing signs the $out image"
done
has "$repo/tools/ci/publish/oci-sign.sh" '[[ $actual == "$expected" ]] || die' \
  'signing refuses any digest but the one the build produced'
has "$repo/tools/ci/publish/oci-sign.sh" 'public_key=$repo_root/keys/cosign.pub' \
  'release signatures are verified against the pinned public key'
has "$rpms_provenance" 'fingerprint=$(python3 os/dev/rpm-snapshot-fingerprint.py)' \
  'snapshot provenance computes the package-set fingerprint'
has_t rpm-snapshot run 'bash tools/ci/publish/rpm-snapshot-ref.sh /target/publish/rpm-snapshot' \
  'snapshot consumers resolve it through rpm-snapshot-ref.sh'
has "$repo/tools/ci/publish/rpm-snapshot-ref.sh" 'ref=$(bash "$repo_root/os/dev/resolve-rpm-snapshot.sh")' \
  'snapshot consumers resolve the fingerprint tag to a verified digest'
# Signing holds the identity token only on stable, never on main or a working tree.
has_t sign-rpms on stable 'snapshot signing runs on stable only'
[ "$(tcheck sign-rpms on)" = stable ] && ok 'snapshot signing runs on no other reference' \
  || bad 'snapshot signing runs on a reference other than stable'
has_t sign-rpms id_token sts.amazonaws.com 'snapshot signing exchanges its own identity token for the signing role'
has_t publish-rpms run "bash tools/ci/publish/oci-publish.sh release registry.tilo.so/tilo/rpms /target/publish/rpms/index 'once:set-{fingerprint}'" \
  'snapshot publication publishes the set-<fingerprint> tag once'
rpms_chain="$(for check in image-rpms-{arch} image-rpms provenance-rpms sign-rpms publish-rpms; do tcheck "$check" run; done)"
if ! grep -Eq 'open-snapshot-pr\.py|reconcile-snapshot-pins\.py|rpms-snapshot\.env' \
     <<<"$rpms_chain" "$rpms_build" "$rpms_provenance" \
   && grep -Fq 'once:set-{fingerprint}' <<<"$rpms_chain"; then
  ok 'snapshot publishes by fingerprint tag with no pin pull request'
else
  bad 'snapshot still opens or reconciles a hand-kept pin'
fi
kernel_sources=(
  "$repo/tools/ci/publish/kernel.sh"
  "$repo/tools/ci/publish/kernel-buildroot.sh"
  "$repo/tools/ci/publish/kernel-acceptance.sh"
  "$repo/os/kernel/Containerfile"
)
kernel_needs=""
for check in image-kernel-{arch} image-kernel-buildroot-{arch} kernel-acceptance-{arch} provenance-kernel; do
  kernel_needs+="$(tcheck "$check" needs)"$'\n' || bad "t.toml declares $check"
done
if ! grep -Eq 'from=rpms|RPMS_REF|rpms-snapshot\.env|resolve-rpm-snapshot' "${kernel_sources[@]}" \
   && ! grep -Eq '^(rpm-snapshot|image-rpms.*|publish-rpms)$' <<<"$kernel_needs"; then
  ok 'the kernel remains independent of the frozen RPM snapshot'
else
  bad 'the kernel still depends on the frozen RPM snapshot'
fi
has "$repo/os/dev/configure-cosign-kms.sh" 'COSIGN_KEY_REF must name a non-exportable KMS/HSM key' \
  'snapshot signing requires a non-exportable key'
if COSIGN_KEY_REF='cosign.key' sh -c '. "$1"' fixture "$repo/os/dev/configure-cosign-kms.sh" >/dev/null 2>&1; then
  bad 'a file-backed signing key is accepted'
else
  ok 'every signing step refuses a key outside a KMS/HSM'
fi
has "$repo/tools/ci/publish/oci-sign.sh" '. "$repo_root/os/dev/configure-cosign-kms.sh"' \
  'snapshot signing configures the KMS provider from its trusted key reference'
grep -q '^COSIGN_KEY_REF=awskms:///' <<<"$(tcheck sign-rpms env)" \
  && ok 'snapshot signing names a KMS key' || bad 'snapshot signing names no KMS key'
oidc_fixture="$(mktemp -d)"
printf '{"value":"e30.eyJpc3MiOiJodHRwczovL2ZvcmdlLmV4YW1wbGUvYXBpL2FjdGlvbnMiLCJhdWQiOiJ0aWxvLXJlbGVhc2Utc2lnbmluZyIsInN1YiI6InRlc3Qtc3ViamVjdCJ9.signature"}\n' \
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
[ "$aws_config" = 'Using OIDC subject: test-subject
OIDC issuer: https://forge.example/api/actions audience: tilo-release-signing
eu-west-2:eu-west-2:arn:aws:iam::123456789012:role/release:unset:600:e30.eyJpc3MiOiJodHRwczovL2ZvcmdlLmV4YW1wbGUvYXBpL2FjdGlvbnMiLCJhdWQiOiJ0aWxvLXJlbGVhc2Utc2lnbmluZyIsInN1YiI6InRlc3Qtc3ViamVjdCJ9.signature' ] \
  && ok 'AWS KMS setup derives the region and installs least-exposure web identity' \
  || bad 'AWS KMS setup does not configure Forgejo web identity safely'
# Run the KMS setup against a Forgejo token whose payload is $1 (base64url).
oidc_identity() {
  identity_fixture="$(mktemp -d)"
  printf '{"value":"e30.%s.signature"}\n' "$1" > "$identity_fixture/token&audience=tilo-release-signing"
  COSIGN_KEY_REF='awskms:///arn:aws:kms:eu-west-2:123456789012:key/example' \
    AWS_SIGNING_ROLE_ARN='arn:aws:iam::123456789012:role/release' \
    ACTIONS_ID_TOKEN_REQUEST_URL="file://$identity_fixture/token" \
    ACTIONS_ID_TOKEN_REQUEST_TOKEN=request-token RUNNER_TEMP="$identity_fixture" \
    sh -c '. "$1"' fixture "$repo/os/dev/configure-cosign-kms.sh" 2>&1
  identity_status=$?
  rm -rf -- "$identity_fixture"
  return "$identity_status"
}
# {"iss":"https://forge.example/api/actions","aud":["tilo-release-signing"],"sub":"test-subject"}
if identity="$(oidc_identity eyJpc3MiOiJodHRwczovL2ZvcmdlLmV4YW1wbGUvYXBpL2FjdGlvbnMiLCJhdWQiOlsidGlsby1yZWxlYXNlLXNpZ25pbmciXSwic3ViIjoidGVzdC1zdWJqZWN0In0)" \
   && printf '%s\n' "$identity" | grep -Fxq 'OIDC issuer: https://forge.example/api/actions audience: tilo-release-signing'; then
  ok 'AWS KMS setup names the issuer and list-form audience STS validates'
else
  bad 'AWS KMS setup does not name the issuer and list-form audience STS validates'
fi
# {"iss":"https://forge.example/api/actions","sub":"test-subject"}
if oidc_identity eyJpc3MiOiJodHRwczovL2ZvcmdlLmV4YW1wbGUvYXBpL2FjdGlvbnMiLCJzdWIiOiJ0ZXN0LXN1YmplY3QifQ >/dev/null; then
  bad 'a Forgejo token without an audience is accepted'
else
  ok 'a Forgejo token without an audience fails closed'
fi
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
has "$rpms_provenance" '"buildType": "https://tilo.sh/buildtypes/frozen-rpm-index/v1"' \
  'snapshot provenance binds the package-set build type'
has "$rpms_provenance" '"internalParameters": {"expectedReleaseDigest": digest}' \
  'snapshot provenance binds the index digest'
has_t sign-rpms run 'bash tools/ci/publish/oci-attest.sh registry.tilo.so/tilo/rpms /target/publish/rpms/index' \
  'snapshot signing attests the index provenance'
has "$repo/tools/ci/publish/oci-attest.sh" 'type=${4:-slsaprovenance1}' \
  'snapshot provenance is attested as SLSA provenance'
has "$repo/tools/ci/publish/oci-attest.sh" 'args=(--yes --type "$type" --predicate "$predicate"' \
  'snapshot publishes signed SLSA provenance'
has "$repo/tools/ci/publish/oci-attest.sh" 'die "the provenance predicate is not for $expected"' \
  'attestation refuses a predicate for another digest'
resolve_script="$repo/os/dev/resolve-rpm-snapshot.sh"
has "$resolve_script" 'if type == "array" then .[] else . end' \
  'snapshot verification accepts current object and legacy array Cosign output'
has "$resolve_script" 'frozen RPM provenance does not bind its package set, digest, base, and architectures' \
  'fingerprint resolve verifies exact RPM snapshot provenance'
has "$resolve_script" 'derive-rpm-package-set.py" --arch x86_64' \
  'fingerprint resolve derives the explicit x86-64 RPM package set'
has "$resolve_script" 'derive-rpm-package-set.py" --arch aarch64' \
  'fingerprint resolve derives the explicit ARM64 RPM package set'
has "$resolve_script" '.buildDefinition.externalParameters.packageSetSha256ByArchitecture' \
  'fingerprint resolve verifies architecture-specific RPM package-set provenance'
# The relay image installs only from the snapshot rpm-snapshot resolved and verified.
has_t image-relay needs rpm-snapshot 'the relay image builds from the verified RPM snapshot'
# It takes the published snapshot only as a digest-pinned reference; a build of a package set
# stable has not published (the run's own image-rpms) records so and is never signed.
has "$repo/tools/ci/publish/relay-build.sh" 'snapshot_for_build "$snapshot" "$rpms" x86_64' \
  'the relay image installs from the snapshot rpm-snapshot named'
has "$repo/tools/ci/publish/lib.sh" '[[ $rpms_ref =~ ^registry\.tilo\.so/tilo/rpms@sha256:[0-9a-f]{64}$ ]]' \
  'the relay image accepts only a digest-pinned published RPM snapshot'
has "$repo/tools/ci/publish/oci-sign.sh" 'require_published_snapshot "$out/rpms-source"' \
  'an image built from an unpublished RPM snapshot is never signed'
has "$build_script" 'frozen RPM provenance does not bind its package set, digest, base, and architectures' \
  'local OS builds verify exact RPM snapshot provenance'
has_t image-rpms-{arch} inputs 'os/dev/derive-rpm-package-set.py' \
  'package parser changes rebuild the snapshot'
has_t provenance-rpms inputs 'os/dev/derive-rpm-package-set.py' \
  'package parser changes the snapshot fingerprint'

has_t image-rpms-{arch} inputs 'os/external-assets/github-assets.tsv' \
  'pinned external asset changes rebuild the snapshot'
has "$rpms_build" 'assets=$(pinned_assets virtio-win-0.1.302-1.noarch.rpm)' \
  'snapshot takes the checksum-verified external input from os-assets'
has "$rpms_build" '--build-context "github-assets=$assets"' \
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
has "$resolve_script" 'registry.tilo.so/tilo/rpms@$digest' \
  'fingerprint resolve returns a digest-pinned RPM registry path'
has "$build_script" 'verify-rpm-snapshot-package-set.sh" podman "$RPMS_REF"' \
  'local production builds reject a stale frozen package manifest'
has "$build_script" '^registry\.tilo\.so/tilo/rpms@sha256:[0-9a-f]{64}$' \
  'local builds accept only the real digest-pinned RPM registry path'

if grep -Rqs --exclude-dir=output --include='Containerfile*' 'DROP_RPMDB_WAL' \
  "$repo/os" || grep -Fq 'DROP_RPMDB_WAL' "$rpms_build"; then
  bad 'obsolete DROP_RPMDB_WAL workaround remains'
else
  ok 'obsolete DROP_RPMDB_WAL workaround is absent'
fi

exit "$fail"
