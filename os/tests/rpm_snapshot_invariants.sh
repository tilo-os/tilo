#!/usr/bin/env bash
# Static contract for the digest-pinned RPM snapshot. This catches package-set
# drift before a cold image build spends minutes resolving an incomplete repo.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="$repo/.forgejo/workflows/snapshot-rpms.yml"
development_workflow="$repo/.forgejo/workflows/development-rpms-arm64.yml"
snapshot="$repo/os/Containerfile.rpms"
image_workflow="$repo/.forgejo/workflows/build-image.yml"
relay_workflow="$repo/.forgejo/workflows/tilo-relay.yaml"
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
  "$derive" --sources | grep -Fxq "$relative" \
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
for package in dbus-tools harfbuzz-icu libatomic wine wine-dxvk qemu-kvm libvirt-client \
  libvirt-daemon-kvm libvirt-daemon-config-nwfilter swtpm swtpm-tools \
  edk2-ovmf virtio-win freerdp xwayland-satellite virt-viewer xorriso \
  genisoimage udftools; do
  printf '%s\n' "$all_packages" | grep -Fxq "$package" \
    && ok "$package is in the derived frozen package set" \
    || bad "$package is missing from the derived frozen package set"
done

arm_packages="$("$derive" --arch aarch64)"
if printf '%s\n' "$arm_packages" | grep -Fxq edk2-aarch64 \
  && ! printf '%s\n' "$arm_packages" | grep -Fxq edk2-ovmf \
  && ! printf '%s\n' "$all_packages" | grep -Fxq edk2-aarch64; then
  ok 'each architecture selects its native Windows VM firmware'
else
  bad 'Windows VM firmware package selection crosses architectures'
fi
has "$workflow" "--arch '\${{ matrix.arch }}'" \
  'snapshot package selection uses the explicit native matrix architecture'
has "$development_workflow" "workflow_dispatch:" \
  'development RPM candidate runs only by explicit dispatch'
has "$development_workflow" "startsWith(github.ref, 'refs/heads/codex/')" \
  'development RPM candidate accepts reviewed codex branches'
has "$development_workflow" "github.ref == 'refs/heads/main'" \
  'development RPM candidate accepts main'
has "$development_workflow" 'runs-on: runner-arm64' \
  'development RPM candidate uses only a native ARM64 runner'
has "$development_workflow" './os/dev/assert-native-arch.sh aarch64' \
  'development RPM candidate requires native ARM64 execution'
has "$development_workflow" './os/dev/derive-rpm-package-set.py --arch aarch64' \
  'development RPM candidate derives the ARM64 package set'
has "$development_workflow" 'platforms: linux/arm64' \
  'development RPM candidate publishes only an ARM64 OCI image'
has "$development_workflow" 'os/Containerfile.rpms' \
  'development RPM candidate reuses the frozen-repository offline closure build'
has "$development_workflow" 'production_signed' \
  'development RPM candidate receipt declares that it is not production-signed'
if grep -Ev '^[[:space:]]*#' "$development_workflow" \
  | grep -Eq 'cosign|promote-native-index|rpms-snapshot\.env|build-image\.yml/dispatches'; then
  bad 'development RPM candidate neither signs nor promotes or dispatches production work'
else
  ok 'development RPM candidate remains separate from production publication'
fi

has "$workflow" './os/dev/derive-rpm-package-set.py' \
  'snapshot workflow uses the invariant-tested package parser'
has "$workflow" 'runner: runner-arm64' \
  'snapshot builds ARM64 dependencies on a native runner'
has "$workflow" 'platforms: ${{ matrix.platform }}' \
  'snapshot explicitly selects each native OCI platform'
has "$workflow" 'needs: snapshot' \
  'snapshot promotion waits for every native build'
has "$workflow" 'needs: signing-preflight' \
  'expensive native snapshot builds wait for signing-authority preflight'
has "$workflow" 'python3 os/dev/promote-native-index.py' \
  'snapshot promotion verifies signed native children'
has "$workflow" 'python3 os/dev/rpm-snapshot-inputs.py --base "$GITHUB_SHA" --head origin/main --require-newer-than-pin' \
  'snapshot promotion refuses only a snapshot that is not newer than the pin on main'
inputs_job="$(sed -n '/^  inputs:/,/^  signing-preflight:/p' "$workflow")"
if grep -Fq 'python3 os/dev/rpm-snapshot-inputs.py --since-pin --head "$GITHUB_SHA"' <<<"$inputs_job" \
   && ! grep -Fq 'fetch-depth: 0' <<<"$inputs_job" \
   && grep -Fxq '      - os/rpms-snapshot.env' "$workflow"; then
  ok 'snapshot refresh compares with the pinned snapshot source, so a refused publication loses no change'
else
  bad 'snapshot refresh compares only with the previous push and can miss a package change'
fi
has "$workflow" 'driver = "vfs"' \
  'snapshot promotion isolates Podman storage from the runner overlay'
has "$workflow" 'rm -f -- "${AWS_WEB_IDENTITY_TOKEN_FILE:-}"' \
  'snapshot promotion preserves web-identity token cleanup when extending its exit trap'
# The kernel builds on its own signed buildroot and never reads this snapshot,
# so the RPM pin lands in its own pull request and never waits for a kernel.
kernel_workflow="$repo/.forgejo/workflows/build-kernel.yml"
promotion_step="$(sed -n '/- name: Propose the frozen snapshot pin through protected main/,$p' "$workflow")"
if ! grep -Eq 'build-kernel\.yml|workflow_call|rpms_ref' <<<"$(sed -n '/^jobs:/,$p' "$workflow")" \
   && ! grep -Eq '^  workflow_call:|RPMS_REF|rpms-snapshot\.env' "$kernel_workflow" \
   && ! grep -Eq 'from=rpms|RPMS_REF' "$repo/os/kernel/Containerfile" \
   && grep -Fq 'RPMS_INDEX_REF: ${{ steps.attest.outputs.ref }}' <<<"$promotion_step" \
   && grep -Fq '"RPMS_SOURCE_REVISION=$GITHUB_SHA" > os/rpms-snapshot.env' <<<"$promotion_step" \
   && grep -Fq 'rpms --rpms-ref "$ref" --source-revision "$GITHUB_SHA"' <<<"$promotion_step"; then
  ok 'the RPM snapshot pin lands on its own, independent of the kernel'
else
  bad 'the RPM snapshot pin still waits for, or is bundled with, a kernel rebuild'
fi
if grep -Fq 'git checkout --detach FETCH_HEAD' <<<"$promotion_step" \
   && ! grep -Fq 'git rebase' <<<"$promotion_step" \
   && grep -Fq 'kernel inputs changed during promotion; refusing stale PR' "$kernel_workflow"; then
  ok 'pin promotion applies its pins on the current main tip; kernel pins refuse moved inputs'
else
  bad 'pin promotion can publish pins built from stale inputs'
fi
# Inputs moving on main during a run must not starve RPM pin promotion: a pin
# newer than main's lands, supersedes older pin PRs, and requests the next
# refresh.
if grep -Fq -- '--base "$GITHUB_SHA" --head FETCH_HEAD --require-newer-than-pin' <<<"$promotion_step" \
   && grep -Fq -- '--base "$GITHUB_SHA" --head FETCH_HEAD --require-equivalent || moved=true' <<<"$promotion_step" \
   && grep -Fq 'superseded+=(--supersede "$candidate")' <<<"$promotion_step" \
   && grep -Fq 'actions/workflows/snapshot-rpms.yml/dispatches' <<<"$promotion_step"; then
  ok 'RPM pin promotion converges while inputs keep moving on main'
else
  bad 'RPM pin promotion can be starved by a busy main'
fi
# Protected main refuses direct pushes; the pins land through a digest-named
# automation branch. The job token pushes it but gets 404 from the
# pull-request API, so RELEASE_TOKEN opens the PR (whose checks then run).
if grep -Fq 'branch="automation/rpm-snapshot-${snapshot_digest:0:16}"' <<<"$promotion_step" \
   && grep -Fq -- '--force-with-lease="refs/heads/$branch:$remote_tip"' <<<"$promotion_step" \
   && grep -Fq 'python3 os/dev/open-snapshot-pr.py' <<<"$promotion_step" \
   && grep -Fq 'FORGEJO_TOKEN: ${{ secrets.RELEASE_TOKEN }}' <<<"$promotion_step" \
   && ! grep -Eq 'FORGEJO_TOKEN: \$\{\{ (secrets\.REGISTRY_TOKEN|github\.token) \}\}' "$workflow" "$kernel_workflow" \
   && ! grep -Eq 'git push [^|]*HEAD:(main|\$\{?GITHUB_REF_NAME)' "$workflow" "$kernel_workflow"; then
  ok 'snapshot pins land through a RELEASE_TOKEN PR, never a direct push to main'
else
  bad 'snapshot pin promotion pushes to main or opens its PR without RELEASE_TOKEN'
fi
if grep -Fq 'Forgejo refused the snapshot pin push to $branch' <<<"$promotion_step" \
   && grep -Fq 'Forgejo refused the snapshot pin push to $branch' "$kernel_workflow" \
   && ! grep -Fq 'branch races' "$kernel_workflow"; then
  ok 'a refused snapshot pin push is reported as refused'
else
  bad 'a refused snapshot pin push can be misreported'
fi
# An OS build verifies only against a merged pin.
if grep -Fq 'actions/workflows/build-image.yml/dispatches' "$workflow"; then
  bad 'snapshot promotion dispatches an image build before its pin lands'
else
  ok 'snapshot promotion leaves image builds to the merged RPM pin'
fi
has "$workflow" 'COSIGN_KEY_REF must name a non-exportable KMS/HSM key' \
  'snapshot signing requires a non-exportable key'
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
if grep -Fq 'cosign public-key --key "$COSIGN_KEY_REF"' "$workflow" \
   && grep -Fq 'cmp --silent keys/cosign.pub "$resolved"' "$workflow" \
   && grep -Fq 'needs: signing-preflight' "$workflow"; then
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
has "$image_workflow" 'frozen RPM provenance does not bind its package set, digest, base, and architectures' \
  'OS CI verifies exact RPM snapshot provenance'
has "$image_workflow" './os/dev/derive-rpm-package-set.py --arch x86_64' \
  'OS CI derives the explicit x86-64 RPM package set'
has "$image_workflow" './os/dev/derive-rpm-package-set.py --arch aarch64' \
  'OS CI derives the explicit ARM64 RPM package set'
has "$image_workflow" '.buildDefinition.externalParameters.packageSetSha256ByArchitecture' \
  'OS CI verifies architecture-specific RPM package-set provenance'
skopeo_step_line=$(grep -Fn -- '- name: Ensure the signed OCI inspector is available' "$image_workflow" | cut -d: -f1)
rpms_auth_line=$(grep -Fn -- '- name: Authenticate RPMS_REF (frozen package snapshot)' "$image_workflow" | cut -d: -f1)
if [ -n "$skopeo_step_line" ] && [ -n "$rpms_auth_line" ] \
   && [ "$skopeo_step_line" -lt "$rpms_auth_line" ] \
   && grep -Fq 'command -v skopeo >/dev/null' "$image_workflow"; then
  ok 'OS CI installs its OCI inspector before signed input verification'
else
  bad 'OS CI can verify signed OCI inputs before its inspector is available'
fi
has "$build_script" 'frozen RPM provenance does not bind its package set, digest, base, and architectures' \
  'local OS builds verify exact RPM snapshot provenance'
has "$workflow" '- os/dev/derive-rpm-package-set.py' \
  'package parser changes retrigger the snapshot'

has "$workflow" '- os/external-assets/github-assets.tsv' \
  'pinned external asset changes retrigger the snapshot'
has "$workflow" './os/dev/cache-github-assets.sh' \
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

packages_line="$(grep -F 'dnf install -y --downloadonly' "$snapshot" || true)"
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
# --downloadonly skips RPMs the base already has; an image that erases one of
# them (pkgconf) and later needs it again would fail offline without this.
has "$snapshot" 'dnf download -y --destdir=/repo --exclude='"'"'qemu-user-static*'"'"' ${PACKAGES}' \
  'snapshot keeps direct specs the base already has installed'
has "$snapshot" '/repo/tilo-direct-package-specs.txt' \
  'snapshot publishes its direct package manifest'
has "$snapshot" 'dnf install -y --downloadonly --setopt=reposdir=/repo --setopt=install_weak_deps=True ${PACKAGES}' \
  'snapshot proves the frozen repo resolves the complete transaction offline'
has "$image_workflow" 'python3 os/dev/verify-rpm-snapshot-buildkit.py "$RPMS_REF" --builder "$SNAPSHOT_BUILDER"' \
  'production CI rejects a stale frozen package manifest before building'
has "$image_workflow" '^code\.podesta\.ai/qos/rpms@sha256:[0-9a-f]{64}$' \
  'production CI accepts only the real digest-pinned RPM registry path'
has "$relay_workflow" 'python3 os/dev/verify-rpm-snapshot-buildkit.py "$RPMS_REF" --builder "$SNAPSHOT_BUILDER"' \
  'relay CI rejects a stale frozen package manifest before building'
has "$build_script" 'verify-rpm-snapshot-package-set.sh" podman "$RPMS_REF"' \
  'local production builds reject a stale frozen package manifest'
has "$build_script" '^code\.podesta\.ai/qos/rpms@sha256:[0-9a-f]{64}$' \
  'local builds accept only the real digest-pinned RPM registry path'

if grep -Rqs --exclude-dir=output --include='Containerfile*' 'DROP_RPMDB_WAL' \
  "$repo/os" || grep -Fq 'DROP_RPMDB_WAL' "$workflow"; then
  bad 'obsolete DROP_RPMDB_WAL workaround remains'
else
  ok 'obsolete DROP_RPMDB_WAL workaround is absent'
fi

python3 "$repo/os/tests/test_rpm_snapshot_inputs.py" \
  && ok "semantic snapshot input gate preserves native and manual rebuilds" \
  || bad "semantic snapshot input gate regression"

exit "$fail"
