#!/usr/bin/env bash
# Cheap policy checks for the component release boundaries. These catch the
# expensive mistakes (coupled tags, full native matrices on every PR, ignored
# Forgejo keys) before a runner is allocated.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
workflows="$repo/.forgejo/workflows"
fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

if grep -R -n -E '^[[:space:]]*permissions:' "$workflows"; then
  bad 'Forgejo-ignored permissions keys remain in workflows'
else
  ok 'workflows use Forgejo authorization/OIDC controls without ignored permissions keys'
fi

desktop="$workflows/release-desktop.yml"
apps="$workflows/release-apps.yml"
apps_publish="$repo/tools/apps/publish-collections.sh"
os="$workflows/build-image.yml"
imager="$workflows/release-imager.yml"

if grep -Fq 'resolve-rpm-snapshot.sh' "$os" \
   && grep -Fq '[[ "$RPMS_REF" =~ ^registry\.tilo\.so/tilo/rpms@sha256:[0-9a-f]{64}$ ]]' "$repo/os/dev/build-image.sh"; then
  ok 'CI and local OS builds accept the published digest-pinned RPM namespace'
else
  bad 'an OS build rejects the RPM namespace published by snapshot-rpms'
fi

cache_driver_ok=1
for workflow in build-ci-builder.yml build-image.yml build-kernel.yml release-relay.yml build-rpms.yml; do
  file="$workflows/$workflow"
  if grep -Fq 'cache-to: type=registry' "$file" \
     && ! grep -Fq 'uses: ./.forgejo/actions/setup-buildx' "$file"; then
    printf 'registry cache workflow lacks a cache-export-capable Buildx driver: %s\n' \
      "$workflow" >&2
    cache_driver_ok=0
  fi
done
grep -Fq 'driver: docker-container' "$repo/.forgejo/actions/setup-buildx/action.yml" \
  && grep -Fq 'docker/setup-buildx-action@8d2750c68a42422c14e847fe6c8ac0403b4cbd6f' "$repo/.forgejo/actions/setup-buildx/action.yml" \
  || cache_driver_ok=0
if [ "$cache_driver_ok" = 1 ]; then
  ok 'registry cache workflows select the docker-container Buildx driver'
else
  bad 'a registry cache workflow still uses the default docker driver'
fi

# A persistent builder keeps BuildKit state on the runner slot between jobs.
# Its name carries the trust class, so a pull request never writes state a
# main build reads, and publications (tags, dispatches, the OS and kernel
# images) never opt in.
persistent_ok=1
for file in "$workflows"/*.yml "$workflows"/*.yaml; do
  case "$(basename "$file")" in
    release-relay.yml|release-registry-server.yml) continue ;;
  esac
  if grep -Eq '^ +persistent:' "$file"; then
    printf 'persistent BuildKit builder outside its allowed workflows: %s\n' "$(basename "$file")" >&2
    persistent_ok=0
  fi
done
grep -Fq "persistent: \${{ !(startsWith(github.ref, 'refs/tags/v') || github.event_name == 'workflow_dispatch') }}" "$workflows/release-relay.yml" \
  && grep -Fq "TRUST: \${{ github.event_name == 'push' && github.ref == 'refs/heads/main' && 'trusted' || 'untrusted' }}" "$repo/.forgejo/actions/setup-buildx/action.yml" \
  && grep -Fq 'name="tilo-$TRUST-$digest"' "$repo/.forgejo/actions/setup-buildx/action.yml" \
  && grep -Fq 'default: "false"' "$repo/.forgejo/actions/setup-buildx/action.yml" \
  || persistent_ok=0
if [ "$persistent_ok" = 1 ]; then
  ok 'persistent BuildKit state is split by trust class and never reaches publications'
else
  bad 'a persistent BuildKit builder can mix trust classes or serve a publication'
fi

grep -Fq "github.event_name != 'pull_request'" "$workflows/build-kernel.yml" \
  && grep -Fq 'scope=kernel-{0},mode=min' "$workflows/build-kernel.yml" \
  && ! grep -Eq 'cache-to:.*scope=kernel-.*mode=max' "$workflows/build-kernel.yml" \
  && ok 'kernel cache export skips PRs and excludes its multi-gigabyte intermediate build tree' \
  || bad 'kernel cache export can duplicate work or exhaust a PR runner'

# A registry hiccup after a completed compile must neither fail the release on
# its best-effort cache write nor discard the build: the cache export ignores
# errors, and a failed publication push retries the already-loaded image a
# bounded number of times before the signing gate consumes its digest.
grep -Fq 'scope=kernel-{0},mode=min,ignore-error=true' "$workflows/build-kernel.yml" \
  && grep -Fq "if: \${{ steps.build.outcome == 'failure' }}" "$workflows/build-kernel.yml" \
  && grep -Fq 'for attempt in 1 2 3; do' "$workflows/build-kernel.yml" \
  && grep -Fq 'docker image inspect "$LOCAL_IMAGE"' "$workflows/build-kernel.yml" \
  && grep -Fq 'ref="registry.tilo.so/tilo/tilo-kernel@${{ steps.published.outputs.digest }}"' "$workflows/build-kernel.yml" \
  && ! grep -Fq 'tilo-kernel@${{ steps.build.outputs.digest }}' "$workflows/build-kernel.yml" \
  && ok 'kernel publication survives cache-write failures and retries a failed push of the completed build' \
  || bad 'a registry cache or push hiccup can fail or silently skip kernel publication'

# The browser engine is built inside the image build from the pinned CEF
# distribution; no image build takes a separately compiled engine stage.
! grep -Fq 'wpe-builder' "$os" && ! grep -Fq 'WPE_BUILDROOT_REF' "$os" \
  && grep -Fq 'ARG CHROMIUM_RUNTIME_VERSION=' "$repo/os/Containerfile" \
  && ok 'OS image builds compile the browser engine runtime themselves' \
  || bad 'an OS image build still takes a separately compiled browser engine'

# A cache manifest naming a blob the registry lost fails a build only when the
# layer's content is needed. The OS build treats exactly that as a cache miss:
# rebuild without cache-from, rewrite every cache tag, consume either digest
# (or the rechunked image's, TIL-747, which is made from either).
uncached=$(awk '/- name: Build the Tilo image without the damaged layer cache/,/# Dev VMs must not wait/' "$os")
[ "$(grep -c "continue-on-error: true" <(awk '/- name: Build and cache the build toolchain/,/- name: Recognize a missing registry cache blob/' "$os"))" = 5 ] \
  && grep -Fq 'run: ./os/dev/recognize-missing-cache-blob.sh "$BUILDER_NAME"' "$os" \
  && ! grep -Fq 'cache-from:' <<<"$uncached" \
  && [ "$(grep -c 'ignore-error=true' <<<"$uncached")" = 1 ] \
  && ! grep -Fq "'\${{ steps.build.outputs.digest }}'" "$os" \
  && grep -Fq "'\${{ steps.rechunk.outputs.digest || steps.build.outputs.digest || steps.build-uncached.outputs.digest }}'" "$os" \
  && grep -Fq "BUILD_DIGEST: \${{ steps.build.outputs.digest || steps.build-uncached.outputs.digest }}" <(awk '/- name: Rechunk the image$/,/- name: Publish the dev-trust integration image/' "$os") \
  && ok 'a missing registry cache blob degrades the OS build to an uncached rebuild' \
  || bad 'a lost registry cache blob can still fail the OS build'

if grep -Fq 'runner: runner-disk-100g-arm64' "$workflows/build-kernel.yml" \
  && grep -Fxq '            runner: runner-disk-100g' "$workflows/build-kernel.yml" \
  && grep -Fq 'platforms: ${{ matrix.platform }}' "$workflows/build-kernel.yml" \
  && grep -Fq 'needs: build-and-push-kernel' "$workflows/build-kernel.yml" \
  && grep -Fq 'promote-native-kernels:' "$workflows/build-kernel.yml" \
  && grep -Fq 'cosign verify-attestation --key keys/cosign.pub' "$workflows/build-kernel.yml" \
  && grep -Fq 'KERNEL_RPMS_X86_64_REF=$KERNEL_RPMS_X86_64_REF' "$workflows/build-kernel.yml" \
  && grep -Fq 'KERNEL_RPMS_AARCH64_REF=$KERNEL_RPMS_AARCH64_REF' "$workflows/build-kernel.yml"; then
  ok 'kernel publication requires both verified native builds and atomic architecture pin promotion'
else
  bad 'kernel publication can omit native ARM64 or bypass verified architecture pin promotion'
fi

if grep -Fq 'memory=9g' "$workflows/build-kernel.yml" \
  && grep -Fq 'memory-swap=9g' "$workflows/build-kernel.yml" \
  && grep -Fq '            kernel_build_jobs: 6' "$workflows/build-kernel.yml" \
  && grep -Fq '            kernel_build_jobs: 4' "$workflows/build-kernel.yml" \
  && test "$(grep -Fc 'KERNEL_BUILD_JOBS=${{ matrix.kernel_build_jobs }}' "$workflows/build-kernel.yml")" -eq 2; then
  ok 'native kernel and Secure Boot probe share explicit resource bounds and cache keys'
else
  bad 'native kernel builds can bypass resource bounds or diverge on their compilation cache key'
fi

if grep -Fq 'needs: [plan, build, images, cache-cold, cache-warm]' "$workflows/build-ci-builder.yml" \
  && grep -Fq 'needs: [plan, build, images, accept-builder]' "$workflows/build-ci-builder.yml" \
  && grep -Fq "needs.accept-builder.result == 'success'" "$workflows/build-ci-builder.yml" \
  && grep -Fq 'runner: runner-arm64' "$workflows/build-ci-builder.yml" \
  && grep -Fq 'python3 os/dev/promote-native-index.py' "$workflows/build-ci-builder.yml" \
  && grep -Fq 'runs-on: ${{ matrix.runner }}' "$workflows/check.yml"; then
  ok 'native CI builder publication waits for cache acceptance on both architectures'
else
  bad 'native CI builder or affected-package architecture coverage is missing'
fi

kernel_verify="$repo/os/dev/verify-kernel-provenance.sh"
if grep -Fq 'x86_64) ref="$KERNEL_RPMS_X86_64_REF" ;;' "$os" \
   && grep -Fq 'aarch64) ref="$KERNEL_RPMS_AARCH64_REF" ;;' "$os" \
   && grep -Fq './os/dev/verify-kernel-provenance.sh "$ref"' "$os" \
   && ! grep -Eq 'verify-kernel-provenance\.sh "\$ref" "|binding=signed-snapshot|--arg rpms "\$RPMS_REF"' "$os" \
   && grep -Fq 'echo "KERNEL_RPMS_REF=$ref" >> "$GITHUB_ENV"' "$os" \
   && grep -Fq '[[ "$KERNEL_RPMS_REF" =~ ^registry\.tilo\.so/tilo/tilo-kernel@sha256:[0-9a-f]{64}$ ]]' "$repo/os/dev/build-image.sh" \
   && grep -Fq 'dev/verify-kernel-provenance.sh" "$KERNEL_RPMS_REF" \' "$repo/os/dev/build-image.sh" \
   && ! grep -Fq 'TILO_KERNEL_RPMS_BINDING' "$repo/os/dev/build-image.sh" \
   && grep -Fq '[[ "$ref" =~ ^registry\.tilo\.so/tilo/tilo-kernel@sha256:[0-9a-f]{64}$ ]]' "$kernel_verify" \
   && grep -Fq 'cosign verify --key "$key" "$ref"' "$kernel_verify" \
   && grep -Fq 'cosign verify-attestation --key "$key" --type slsaprovenance1 "$ref"' "$kernel_verify" \
   && grep -Fq 'https://tilo.sh/buildtypes/kernel-rpm/v1' "$kernel_verify" \
   && grep -Fq 'python3 "$repo/os/dev/kernel-build-environment.py"' "$kernel_verify" \
   && grep -Fq 'cosign verify --key "$key" "$environment"' "$kernel_verify" \
   && grep -Fq 'BUILD_TYPE = "https://tilo.sh/buildtypes/kernel-rpm/v1"' "$repo/os/dev/kernel-build-environment.py" \
   && grep -Fq 'actual_arch="$(skopeo inspect "docker://$ref" | jq -er .Architecture)"' "$kernel_verify"; then
  ok 'CI and local OS builds authenticate kernel digest, architecture, and provenance before consumption'
else
  bad 'an OS build can consume an unpinned or unauthenticated kernel repository'
fi

kernel_workflow="$workflows/build-kernel.yml"
kernel_containerfile="$repo/os/kernel/Containerfile"
if grep -Fq -- '- name: Verify the signed native kernel buildroot' "$kernel_workflow" \
   && grep -Fq '. os/kernel-buildroot-snapshot.env' "$kernel_workflow" \
   && grep -Fq '[[ "$ref" =~ ^registry\.tilo\.so/tilo/tilo-kernel-buildroot@sha256:[0-9a-f]{64}$ ]]' "$kernel_workflow" \
   && grep -Fq 'cosign verify --key keys/cosign.pub "$ref"' "$kernel_workflow" \
   && test "$(grep -Fc 'KERNEL_BUILDROOT_REF=' "$kernel_workflow")" -ge 3 \
   && ! grep -Eq 'RPMS_REF|rpms-snapshot\.env|derive-rpm-package-set' "$kernel_workflow" \
   && grep -Fq 'FROM ${KERNEL_BUILDROOT_REF} AS build-toolchain' "$kernel_containerfile" \
   && ! grep -Eq 'RPMS_REF|BASE_REF|from=rpms|dnf5? (install|download)|enablerepo' "$kernel_containerfile" \
   && grep -Fq '! find /etc/yum.repos.d -type f -print -quit' "$kernel_containerfile" \
   && grep -Fq 'cp /usr/src/tilo-kernel/kernel-*.src.rpm /work/kernel.src.rpm' "$kernel_containerfile"; then
  ok 'kernel compilation starts only from the signed native kernel buildroot, with no repository fallback'
else
  bad 'kernel compilation can start from an unauthenticated buildroot or fall back to a package repository'
fi
if grep -Fq 'apt_firmware=qemu-efi-aarch64' "$kernel_workflow" \
   && grep -Fq 'apt_firmware=ovmf' "$kernel_workflow" \
   && grep -Fq 'fedora_firmware=edk2-aarch64' "$kernel_workflow" \
   && grep -Fq 'fedora_firmware=edk2-ovmf' "$kernel_workflow" \
   && grep -Fq 'zstd dosfstools mtools "$apt_qemu" "$apt_firmware"' "$kernel_workflow" \
   && grep -Fq 'zstd dosfstools mtools "$fedora_qemu" "$fedora_firmware"' "$kernel_workflow" \
   && grep -Fq '/usr/share/edk2/ovmf /usr/share/OVMF' "$repo/os/tests/secureboot_vm_probe.sh" \
   && grep -Fq "expect_log 'TILO_SECURE_BOOT_ENROLL_PASS'" "$repo/os/tests/secureboot_vm_probe.sh" \
   && grep -Fq 'mapfile -t runtime_files' "$repo/os/tests/secureboot_vm_probe.sh" \
   && grep -Fq 'for directory in "${runtime_dirs[@]}"' "$repo/os/tests/secureboot_vm_probe.sh" \
   && grep -Fq 'mkfs.vfat -n TILO_SBTEST' "$repo/os/tests/secureboot_vm_probe.sh" \
   && grep -Fq "expect_log 'Access Denied'" "$repo/os/tests/secureboot_vm_probe.sh" \
   && grep -Fq 'AAVMF_CODE.ms.fd' "$repo/os/tests/secureboot_vm_probe.sh" \
   && grep -Fq 'virtio-blk-device,drive=esp' "$repo/os/tests/secureboot_vm_probe.sh" \
   && grep -Fq -- '-nic none -nographic -no-reboot' "$repo/os/tests/secureboot_vm_probe.sh" \
   && grep -Fq 'export PATH="/usr/local/sbin:/usr/sbin:/sbin:$PATH"' "$repo/os/kernel/tests/test-rpm.sh" \
   && grep -Fq 'command -v modinfo >/dev/null' "$repo/os/kernel/tests/test-rpm.sh" \
   && grep -Fq 'modinfo -F name' "$repo/os/kernel/tests/test-rpm.sh" \
   && grep -Fq 'command -v zstd >/dev/null' "$repo/os/kernel/tests/boot-smoke.sh" \
   && grep -Fq 'kernel/drivers/net/dummy.ko.zst' "$repo/os/kernel/tests/boot-smoke.sh"; then
  ok 'native kernel acceptance installs modinfo, firmware and EFI-image tools on apt and dnf runners'
else
  bad 'native kernel acceptance can invoke missing module or Secure Boot VM dependencies'
fi

if grep -Fq 'runner: runner-disk-100g' "$kernel_workflow" \
   && grep -Fq 'runner: runner-disk-100g-arm64' "$kernel_workflow" \
   && grep -Fq 'assert-native-arch.sh "${{ matrix.arch }}"' "$kernel_workflow" \
   && grep -Fq "format('type=gha,scope=kernel-{0}', matrix.arch)" "$kernel_workflow" \
   && grep -Fq "variable=\"KERNEL_RPMS_\$(printf '%s' '\${{ matrix.arch }}' | tr '[:lower:]' '[:upper:]')_REF\"" "$kernel_workflow" \
   && grep -Fq 'KERNEL_RPMS_X86_64_REF|KERNEL_RPMS_AARCH64_REF) ;;' "$kernel_workflow" \
   && ! grep -Fq 'variable=KERNEL_RPMS_${{ matrix.arch }}_REF' "$kernel_workflow" \
   && grep -Fq 'promote-native-kernels:' "$kernel_workflow" \
   && grep -Fq 'KERNEL_RPMS_X86_64_REF=$KERNEL_RPMS_X86_64_REF' "$kernel_workflow" \
   && grep -Fq 'KERNEL_RPMS_AARCH64_REF=$KERNEL_RPMS_AARCH64_REF' "$kernel_workflow" \
   && grep -Fq 'refusing to regress' "$kernel_workflow" \
   && grep -Fq 'git merge-base --is-ancestor "$1" "$GITHUB_SHA"' "$kernel_workflow" \
   && grep -Fq 'superseded+=(--supersede "$candidate")' "$kernel_workflow" \
   && ! grep -Fq 'refusing stale PR' "$kernel_workflow" \
   && grep -Fq 'automation/kernel-snapshot-${snapshot_digest:0:16}' "$kernel_workflow" \
   && grep -Fq 'python3 os/dev/open-snapshot-pr.py' "$kernel_workflow" \
   && grep -Fq 'FORGEJO_TOKEN: ${{ secrets.RELEASE_TOKEN }}' "$kernel_workflow" \
   && ! grep -Eq 'FORGEJO_TOKEN: \$\{\{ (secrets\.REGISTRY_TOKEN|github\.token) \}\}' "$kernel_workflow" \
   && grep -Fq -- '--force-with-lease="refs/heads/$branch:$remote_tip"' "$kernel_workflow" \
   && ! grep -Fq 'git push origin HEAD:main' "$kernel_workflow" \
   && grep -Fq "(github.event_name == 'workflow_dispatch' || github.event_name == 'push') && github.ref == 'refs/heads/main'" "$kernel_workflow" \
   && ! grep -Fq "github.event_name == 'workflow_dispatch' && github.ref != 'refs/heads/main'" "$kernel_workflow" \
   && grep -Fq 'kernel-build-support.sh stage-modules "$(uname -m)"' "$repo/os/kernel/Containerfile" \
   && grep -Fq 'sh "$self" stage-boot "$arch" "$ksrc" "$stage" "$kver"' "$repo/os/kernel/build-support.sh" \
   && grep -Fq 'certs/system_certificates.S|*/certs/system_certificates.S|' "$repo/os/kernel/cc-wrapper.sh" \
   && grep -Fq 'python3 /work/check-signing-identity.py "$ksrc/vmlinux"' "$repo/os/kernel/build-support.sh" \
   && grep -Fq '"$stage/lib/modules/$kver/vmlinuz" "$ksrc/scripts/extract-vmlinux"' "$repo/os/kernel/build-support.sh" \
   && grep -Fq '"$stage/lib/modules/$kver/vmlinuz"' "$repo/os/kernel/build-support.sh" \
   && grep -Fq 'check_extracted_image(boot_image, extractor, signed_with)' "$repo/os/kernel/check-signing-identity.py" \
   && grep -Fq 'aarch64) image=arch/arm64/boot/Image' "$repo/os/kernel/build-support.sh" \
   && grep -Fq 'x86_64) image=arch/x86/boot/bzImage' "$repo/os/kernel/build-support.sh" \
   && grep -Fq 'rpm-build make gcc clang lld bc flex bison rust cargo' "$repo/os/Containerfile.kernel-buildroot" \
   && ! grep -Fq 'TILO_BUILDER_REF' "$repo/os/kernel/Containerfile" \
   && grep -Fq 'https://tilo.sh/buildtypes/kernel-rpm/v1' "$kernel_workflow" \
   && grep -Fq -- '--predicate kernel-provenance.json --type slsaprovenance1' "$kernel_workflow" \
   && grep -Fq 'cosign verify-attestation --key keys/cosign.pub' "$kernel_workflow" \
   && grep -Fq 'ExclusiveArch:  x86_64 aarch64' "$repo/os/kernel/tilo-kernel-payload.spec"; then
  ok 'kernel publication builds, signs, attests, and reports native x86-64 and ARM64 payloads'
else
  bad 'the two-architecture OS release does not have matching native kernel builds'
fi

# Only kernel inputs rebuild the kernel. The OS RPM snapshot and base are not
# kernel inputs: the kernel builds on its own signed buildroot, so an OS
# package change never invalidates the pinned kernel.
kernel_triggers="$(sed -n '/^on:/,/^concurrency:/p' "$kernel_workflow")"
if grep -Fq '"os/kernel-buildroot-snapshot.env"' <<<"$kernel_triggers" \
   && grep -Fq '"os/kernel/**"' <<<"$kernel_triggers" \
   && ! grep -Eq 'rpms-snapshot\.env|base-snapshot\.env|derive-rpm-package-set|kernel-snapshot\.env"|workflow_call' <<<"$kernel_triggers" \
   && ! grep -Fq 'build-kernel.yml' "$workflows/build-rpms.yml" \
   && ! grep -Fq 'os/kernel/Containerfile' "$workflows/build-rpms.yml" \
   && ! grep -Fq 'os/kernel/Containerfile' "$repo/os/dev/derive-rpm-package-set.py" \
   && grep -Fq 'os/kernel-buildroot-snapshot.env' <<<"$(sed -n '/KERNEL_SNAPSHOT_INPUTS: |/,/^    steps:/p' "$kernel_workflow")"; then
  ok 'only kernel inputs rebuild the kernel; OS package changes do not'
else
  bad 'an OS package or base change can invalidate or rebuild the kernel'
fi

grep -Fq 'tags: ["desktop-v*"]' "$desktop" \
  && grep -Fq 'tags: ["apps-v*", "app-*-v*"]' "$apps" \
  && grep -Fq 'tags: ["v*"]' "$os" \
  && ok 'desktop, app and OS component tags are disjoint' \
  || bad 'a component tag can accidentally release another component'

! sed -n '/^  push:/,/^  workflow_dispatch:/p' "$desktop" | grep -q '^    branches:' \
  && [ "$(grep -c '      - "crates/session/shell/\*\*"' "$desktop")" -eq 1 ] \
  && grep -Fq "if: github.event_name != 'pull_request'" "$desktop" \
  && grep -Fq '  schedule:' "$desktop" \
  && grep -Fq 'TILO_DESKTOP_ARM64_RELEASES: "1"' "$desktop" \
  && grep -Fq '[[ ${TILO_DESKTOP_ARM64_RELEASES:-0} == 1 ]]' "$desktop" \
  && grep -Fq 'arm64_runner=runner-large-arm64' "$desktop" \
  && grep -Fq 'release_arches=amd64,arm64' "$desktop" \
  && grep -Fq 'release_arches: ${{ steps.lanes.outputs.release_arches }}' "$desktop" \
  && grep -Fq 'RELEASE_ARCHES: ${{ needs.validate.outputs.release_arches }}' "$desktop" \
  && grep -Fq 'verify-release-artifacts.sh release "$version" "$RELEASE_ARCHES"' "$desktop" \
  && ok 'desktop PRs keep preflight while native packaging is scheduled or explicit' \
  || bad 'desktop packaging boundaries or native release lanes regressed'

# Native package lanes have neither private keys nor OIDC. A separate tag-only
# broker job signs only successful install-tested artifacts after exact-main proof.
desktop_signer=$(sed -n '/^  sign-rpm:$/,/^  host-session-e2e:$/p' "$desktop")
desktop_validate=$(sed -n '/^  validate:$/,/^  package:$/p' "$desktop")
desktop_package=$(sed -n '/^  package:$/,/^  sign-rpm:$/p' "$desktop")
signing_boundary_ok=1
for name in TILO_RPM_SIGNING_KEY_B64 TILO_RPM_SIGNING_KEY_ID; do
  ! grep -Fq "secrets.$name" "$desktop" || signing_boundary_ok=0
done
grep -Fq "if: github.event_name == 'push' && startsWith(github.ref, 'refs/tags/desktop-v')" <<<"$desktop_signer" \
  && grep -Fq 'enable-openid-connect: true' <<<"$desktop_signer" \
  && grep -Fq 'needs: [validate, package]' <<<"$desktop_signer" \
  && grep -Fq 'dist/rpm/sign-rpms.sh signed-fedora' <<<"$desktop_signer" \
  && grep -Fq 'vars.TILO_RPM_SIGNING_FINGERPRINT' <<<"$desktop_signer" \
  && ! grep -Fq 'enable-openid-connect' <<<"$desktop_package" \
  && ! grep -Fq 'secrets.TILO_RPM_SIGNING' <<<"$desktop_package" \
  && grep -Fq 'run: tools/ci/receipt.sh verify "$GITHUB_SHA" origin' <<<"$desktop_validate" \
  && grep -Fq 'needs: validate' <<<"$desktop_package" \
  && grep -Fq 'needs: [validate, package, sign-rpm, host-session-e2e]' "$desktop" \
  && grep -Fq "'signed-fedora-44-amd64' || 'fedora-44-amd64'" "$desktop" \
  && ! grep -Fq -- '--env TILO_RPM_SIGNING' "$repo/dist/ci/run-package-job.sh" \
  || signing_boundary_ok=0
if [[ $signing_boundary_ok == 1 ]]; then
  ok 'desktop isolated broker signs exact-main-qualified tested tag RPMs without exposing a key'
else
  bad 'desktop signing boundary or pre-packaging qualification regressed'
fi

grep -Fq 'publish-os-payload:' "$desktop" \
  && grep -Fq 'PACKAGE_TOKEN: ${{ secrets.REGISTRY_TOKEN }}' "$desktop" \
  && grep -Fq 'VARIABLE_TOKEN: ${{ secrets.RELEASE_TOKEN }}' "$desktop" \
  && ! grep -Fq 'TILO_DESKTOP_RELEASE_AUDIENCE' "$desktop" \
  && grep -Fq 'CONTINUOUS_OS_RELEASE: "0"' "$desktop" \
  && grep -Fq "if: (github.event_name == 'schedule' || github.event_name == 'workflow_dispatch') && github.ref == 'refs/heads/main'" "$desktop" \
  && grep -Fq 'TILO_DESKTOP_PAYLOAD_URL_X86_64' "$os" \
  && grep -Fq 'TILO_PREBUILT_DESKTOP=${{ env.TILO_PREBUILT_DESKTOP }}' "$os" \
  && grep -Fq 'DESKTOP_RELEASE_ARGS=""' "$repo/os/Containerfile" \
  && ok 'OS composition reuses the install-tested native desktop payload' \
  || bad 'OS releases still rebuild the desktop component'

grep -Fq 'payload_source=$(jq -r '"'"'.source // empty'"'"' "$work/manifest.json")' "$os" \
  && grep -Fq 'Cargo.toml Cargo.lock rust-toolchain.toml .cargo crates apps vendor) ]]; then' "$os" \
  && grep -Fq 'predates this commit'"'"'s desktop inputs; building the desktop from source' "$os" \
  && grep -Fq '"source": sys.argv[3]' "$repo/dist/desktop/package-os-payload.sh" \
  && ok 'OS composition reuses the desktop payload only while no Rust or Cargo input changed since it was built' \
  || bad 'OS composition can ship a desktop payload older than its own compositor sources'

grep -Fq "max-parallelism = \${{ github.event_name == 'workflow_call' && 4 || 1 }}" "$os" \
  && test "$(grep -Fc 'TILO_CFS_DIGEST=00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000' "$os")" -eq 9 \
  && grep -Fq 'TILO_PROVISIONAL_CFS_DIGEST="$(printf '\''0%.0s'\'' {1..128})"' "$repo/os/dev/build-image.sh" \
  && ! grep -Fq 'id=tilo-composefs-' "$repo/os/Containerfile" \
  && grep -Fq 'find /build/target -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +' "$repo/os/Containerfile" \
  && grep -Fq 'CONTINUOUS_RELEASE: "0"' "$os" \
  && ! grep -Fq 'token: ${{ secrets.REGISTRY_TOKEN }}' "$os" \
  && ! grep -Fq 'fetch-depth: 0' "$os" \
  && grep -Fq "if: steps.release-mode.outputs.validate == 'true'" "$os" \
  && grep -Fq 'installed dev image, desktop, app integration and tilo-vmd passed; publication was not requested' "$os" \
  && grep -Fq "if: steps.release-mode.outputs.publish == 'true'" "$os" \
  && ok 'ordinary OS integration checks run without release-only steps' \
  || bad 'OS branch validation still requires release-only steps'

! grep -Fq 'cron:' "$os" \
  && grep -Fq "if: github.event_name == 'schedule' && github.ref == 'refs/heads/main'" "$os" \
  && grep -Fq 'elif [[ $GITHUB_EVENT_NAME == schedule && $GITHUB_REF == refs/heads/main ]]; then' "$os" \
  && grep -Fq '$PUBLISHED_REVISION == "$GITHUB_SHA"' "$os" \
  && grep -Fq "needs.plan.outputs.skip != 'true'" "$os" \
  && grep -Fq '&& ${PLANNED_PUBLISH:-false} == true ) \' "$os" \
  && grep -Fq "(github.event_name == 'schedule' && github.ref == 'refs/heads/main')" "$os" \
  && ok 'publications are manual (tags and dispatch) and skip an already published revision' \
  || bad 'nightly OS publication is missing, not main-only, or rebuilds an already published revision'

grep -Fq 'refs/tags/v*) with_browser=1 ;;' "$os" \
  && grep -Fq 'with_browser=${REQUESTED_BROWSER:-1}' "$os" \
  && ok 'every image carries the browser unless a manual run leaves it out' \
  || bad 'an image can omit the browser without a manual request'

# CI and os/dev/build-image.sh derive the Windows provider with one command, so
# x86_64 can never drop it and ARM includes it as soon as both pins exist.
grep -Fq 'python3 os/dev/windows_provider.py mode --arch "$(uname -m)"' "$os" \
  && grep -Fq 'FEX_PAYLOAD_REF: ${{ vars.TILO_FEX_PAYLOAD_REF }}' "$os" \
  && grep -Fq 'WINDOWS_GUEST_REF: ${{ vars.TILO_WINDOWS_GUEST_REF }}' "$os" \
  && [ "$(grep -Fc 'TILO_WITH_WINDOWS=${{ env.TILO_WITH_WINDOWS }}' "$os")" = 9 ] \
  && grep -Fq 'windows_provider.py" mode --arch "$TILO_WINDOWS_ARCH"' "$repo/os/dev/build-image.sh" \
  && grep -Fq -- '--build-arg "TILO_WITH_WINDOWS=$TILO_WITH_WINDOWS"' "$repo/os/dev/build-image.sh" \
  && grep -Fq 'FROM windows-provider-${TILO_WINDOWS_ARCH}-${TILO_WITH_WINDOWS} AS windows-provider' "$repo/os/Containerfile" \
  && ok 'CI and the local builder derive the Windows provider from the same pins' \
  || bad 'the Windows provider mode is chosen differently in CI and os/dev/build-image.sh'

if ! grep -q '^  pull_request:' "$os" \
  && ! sed -n '/^  push:/,/^  workflow_dispatch:/p' "$os" | grep -q '^    branches:' \
  && ! grep -Fq '  schedule:' "$os" \
  && grep -Fq '  workflow_dispatch:' "$os" \
  && grep -Fq 'native-atspi-smoke.sh oobe-actions' "$os" \
  && grep -Fq 'native-atspi-smoke.sh quick-settings' "$os" \
  && grep -Fq 'native-atspi-smoke.sh auth' "$os" \
  && grep -Fq 'org.freedesktop.PolicyKit1.Authority.CheckAuthorization' "$repo/crates/session/shell/tests/native_atspi_smoke.sh" \
  && grep -Fq 'installed-atspi-apps.sh' "$os" \
  && grep -Fq 'org.a11y.atspi.Action.GetActions' "$repo/crates/session/shell/tests/native_atspi_smoke.sh" \
  && grep -Fq 'org.a11y.atspi.Action.DoAction 0' "$repo/crates/session/shell/tests/native_atspi_smoke.sh" \
  && grep -Fq 'org.a11y.atspi.Component.GrabFocus' "$repo/crates/session/shell/tests/native_atspi_smoke.sh" \
  && grep -Fq 'org.a11y.atspi.EditableText.SetTextContents' "$repo/crates/session/shell/tests/native_atspi_smoke.sh" \
  && grep -Fq 'org.a11y.atspi.Text.GetText -- 0 -1' "$repo/crates/session/shell/tests/native_atspi_smoke.sh" \
  && grep -Fq 'wait_for_names "New Folder"' "$repo/apps/file-explorer/tests/atspi_smoke.sh" \
  && grep -Fq 'at-spi2-core' "$repo/os/Containerfile" \
  && grep -Fq 'atspi-external-client.py' "$repo/os/Containerfile" \
  && grep -Fq 'accessibility.atspi_acceptance' "$repo/apps/settings/tilo-app.toml"; then
  ok 'OS release VM drives compositor and manifest-certified app AT-SPI actions over its session bus'
else
  bad 'AT-SPI release gate can ship an app outside the readable, action-capable desktop endpoint'
fi

# polkit's rules engine reads a subject as a process or a bus name only; a
# unix-session subject reaches g_assert_not_reached() and aborts polkitd, so
# the administrator-prompt check must ask as a process in the session.
smoke="$repo/crates/session/shell/tests/native_atspi_smoke.sh"
if grep -Fq "('unix-process', {'pid': <uint32 \$asker>, 'start-time': <uint64 \$started>" "$smoke" \
  && ! grep -Fq "('unix-session'" "$smoke"; then
  ok 'the release VM asks polkit for administrator access as a session process'
else
  bad 'the release VM asks polkit with a subject its rules engine aborts on'
fi

grep -Fq 'matrix: ${{ fromJSON(needs.catalog.outputs.linux-matrix) }}' "$imager" \
  && grep -Fq 'github.event.inputs.full_native' "$imager" \
  && grep -Fq 'runner":"imager-linux-x86_64"' "$imager" \
  && grep -Fq 'runner":"imager-linux-aarch64"' "$imager" \
  && ! grep -Fq 'pull_request:' "$imager" \
  && ! grep -Fq 'branches: ["main"]' "$imager" \
  && ok 'imager native jobs are reserved for explicit release rehearsals and tags' \
  || bad 'ordinary commits can still allocate or misroute native imager hosts'

grep -Fq 'dist/apps/app-collections.toml' "$apps" \
  && grep -Fq 'cancel-in-progress: false' "$apps" \
  && ! grep -Eq '^  pull_request(_target)?:' "$apps" \
  && grep -Fq '$package_api/tilo-app-collections/$collection_version/' "$apps_publish" \
  && grep -Fq 'collection_version="$VERSION-$bundle_sha"' "$apps_publish" \
  && grep -Fq 'bundle_sha=$(sha256sum "$bundle"' "$apps_publish" \
  && grep -Fq '$package_api/tilo-app-$name/$VERSION/$file' "$apps" "$apps_publish" \
  && grep -Fq 'PRIOR_X86_URL' "$apps" \
  && grep -Fq 'if [[ $GITHUB_REF == refs/heads/main ]]; then' "$apps" \
  && grep -Fq 'dist/app-release/runtime/**' "$apps" \
  && grep -Fq 'update_variable TILO_APP_COLLECTION_URL' "$apps" "$apps_publish" \
  && grep -Fq '$GITHUB_EVENT_NAME == push && $GITHUB_REF == refs/heads/main' "$apps" \
  && grep -Fq '${REQUESTED_PUBLISH:-false} == true' "$apps" \
  && grep -Fq 'REQUESTED_ARCHITECTURES: ${{ github.event.inputs.architectures }}' "$apps" \
  && grep -Fq 'tools/apps/app_release.py --architecture-matrix' "$apps" \
  && awk '{sub(/\r$/, "")} /^  plan:/{p=1;next} p && /^  [[:alnum:]_-]+:/{exit} p' "$apps" | grep -Fx '      builder_image: ${{ steps.builder.outputs.image }}' >/dev/null \
  && awk '{sub(/\r$/, "")} /^  plan:/{p=1;next} p && /^  [[:alnum:]_-]+:/{exit} p' "$apps" | grep -Fx '        id: builder' >/dev/null \
  && awk '{sub(/\r$/, "")} /^  plan:/{p=1;next} p && /^  [[:alnum:]_-]+:/{exit} p' "$apps" | grep -Fx '        run: python3 -I tools/ci/python.py tools/ci/resolve_builder.py' >/dev/null \
  && awk '{sub(/\r$/, "")} /^  build:/{p=1;next} p && /^  [[:alnum:]_-]+:/{exit} p' "$apps" | grep -Fx '    needs: [plan, authorize-publish]' >/dev/null \
  && awk '{sub(/\r$/, "")} /^  build:/{p=1;next} p && /^  [[:alnum:]_-]+:/{exit} p' "$apps" | grep -Fx '      image: ${{ needs.plan.outputs.builder_image }}' >/dev/null \
  && grep -Fq 'actions/upload-artifact@16871d9e8cfcf27ff31822cac382bbb5450f1e1e' "$apps" \
  && grep -Fq 'enable-openid-connect: true' "$apps" \
  && grep -Fq 'TILO_PACKAGE_PUBLIC_BASE_URL: ${{ vars.TILO_PACKAGE_PUBLIC_BASE_URL }}' "$apps" \
  && grep -Fq 'bundle_url="$package_public_api/tilo-app-collections/' "$apps" "$apps_publish" \
  && grep -Fq 'bundle_upload_url="$package_api/tilo-app-collections/' "$apps" "$apps_publish" \
  && ! grep -Fq 'channel_url=$bundle_upload_url' "$apps" "$apps_publish" \
  && grep -Fq 'tools/upload_immutable_packages.sh "$upload_manifest"' "$apps" "$apps_publish" \
  && grep -Fq 'tools/reuse_prior_app_collection.sh' "$apps" "$apps_publish" \
  && grep -Fq 'audience=$MAIN_AUDIENCE' "$apps" "$apps_publish" \
  && ! grep -Fq 'TILO_APP_RELEASE_TAG_AUDIENCE' "$apps" "$apps_publish" \
  && awk '{sub(/\r$/, "")} /^  qualify-collection:/{p=1;next} p && /^  [[:alnum:]_-]+:/{exit} p' "$apps" | grep -Fx '          FORGEJO_ARTIFACT_TOKEN: ${{ github.token }}' >/dev/null \
  && awk '{sub(/\r$/, "")} /^  qualify-collection:/{p=1;next} p && /^  [[:alnum:]_-]+:/{exit} p' "$apps" | grep -Fx '          VERIFIER_BUNDLE_READ_TOKEN: ${{ secrets.TILO_VERIFIER_BUNDLE_READ_TOKEN }}' >/dev/null \
  && awk '{sub(/\r$/, "")} /^  publish:/{p=1;next} p && /^  [[:alnum:]_-]+:/{exit} p' "$apps" | grep -Fx '          VERIFIER_BUNDLE_READ_TOKEN: ${{ secrets.TILO_VERIFIER_BUNDLE_READ_TOKEN }}' >/dev/null \
  && ! awk '{sub(/\r$/, "")} /^  publish:/{publish=1} publish && /^    steps:/{steps=1} publish && steps' "$apps" | grep -F 'secrets.REGISTRY_TOKEN' >/dev/null \
  && ok 'app collections serialize channel updates and remain durable across workflows' \
  || bad 'app releases can cancel a required channel update or cannot be consumed later'

grep -Fq 'TILO_APP_COLLECTION_SHA256' "$os" \
  && grep -Fq 'TILO_APP_COLLECTION_URL_AARCH64' "$os" \
  && grep -Fq 'enable-openid-connect: true' "$os" \
  && grep -Fq 'TILO_OS_APP_COLLECTION_AUDIENCE' "$os" \
  && test "$(grep -Fc 'http://forgejo.forgejo.svc.cluster.local:3000/api/packages/tilo/generic/*' "$os")" -eq 2 \
  && test "$(grep -Fc -- "--proto '=https' --proto-redir '=https'" "$os")" -eq 4 \
  && test "$(grep -Fc 'https:\/\/code.tilo.so}' "$os")" -eq 2 \
  && ! grep -Fq -- "--proto '=http'" "$os" \
  && ! grep -Fq '[[ $EVENT_NAME != pull_request ]] || exit 0' "$os" \
  && grep -Fq 'auth="Bearer $jwt"' "$os" \
  && grep -Fq 'TILO_PREBUILT_APPS=${{ env.TILO_PREBUILT_APPS }}' "$os" \
  && grep -Fq 'TAPP_DIR=/out/prebuilt-apps' "$repo/os/Containerfile" \
  && grep -Fq 'cp -a /build/os/app-runtime/tilo-runtime /usr/lib/' "$repo/os/Containerfile" \
  && grep -Fq 'RUNTIME_BUNDLE_IDENTITY_FILE' "$repo/dist/apps/package-apps.sh" \
  && grep -Fq -- '-C codegen-units=1' "$repo/tools/ci/rustc-thin-app-wrapper.sh" \
  && ! grep -Fq -- '--icf=all' "$repo/tools/ci/rustc-thin-app-wrapper.sh" \
  && grep -Fq 'tools/reuse_prior_app_collection.sh' "$apps" "$apps_publish" \
  && grep -Fq 'shared runtime changed; the full collection must be rebuilt' \
       "$repo/tools/reuse_prior_app_collection.sh" \
  && ok 'OS builds consume OIDC-authenticated pinned app and desktop inputs without rebuilding them' \
  || bad 'OS releases are still coupled to app source builds'

grep -Fq "CARGO_BUILD_JOBS: \${{ matrix.arch == 'aarch64' && '1' || '4' }}" \
    "$workflows/check.yml" \
  && ok 'affected-package CI serializes memory-heavy ARM links' \
  || bad 'affected-package CI can OOM the native ARM runner with parallel links'

grep -Fq 'SCCACHE_SHA256_AARCH64=821a86343191aa1cbab74bd42f9e93c9a63bf85e4742945f40d3ae84193c1c77' "$repo/os/Containerfile" \
  && grep -Fq 'sccache_arch=aarch64' "$repo/os/Containerfile" \
  && grep -Fq 'sccache backend unavailable; compiling normally' "$repo/os/Containerfile" \
  && grep -Fq 'export RUSTC_WRAPPER=' "$repo/os/Containerfile" \
  && grep -Fq 'SCCACHE_SHA256_AARCH64=821a86343191aa1cbab74bd42f9e93c9a63bf85e4742945f40d3ae84193c1c77' "$repo/os/Containerfile.builder" \
  && grep -Fq 'sccache_arch=aarch64' "$repo/os/Containerfile.builder" \
  && grep -Fq 'ripgrep time yt-dlp' "$repo/os/Containerfile.builder" \
  && ok 'OS and CI builder images install checksum-pinned native cache clients' \
  || bad 'an ARM build image can still install the x86 compiler-cache client'

grep -Fq "PKGEXT='.pkg.tar.zst'" "$repo/dist/arch/build-arch.sh" \
  && grep -Fq 'makepkg --config "$makepkg_config"' "$repo/dist/arch/build-arch.sh" \
  && ok 'Arch packages use the release artifact compression on every architecture' \
  || bad 'an Arch base image can silently change the release package extension'

# Joining an identity from first-boot onboarding is proven on two booted
# disks sharing one Ethernet segment, driven over AT-SPI, including a helper
# failure that must surface in the UI.
grep -Fq './os/dev/test-nearby-pairing.sh "$TILO_DISK"' "$os" \
  && grep -Fq 'lan_qemu_args' "$repo/os/dev/vm.sh" \
  && grep -Fq 'native-atspi-smoke.sh oobe-nearby' "$repo/os/dev/test-nearby-pairing.sh" \
  && grep -Fq "chmod 2750 /var/lib/tilo/enrollment-commits" "$repo/os/dev/test-nearby-pairing.sh" \
  && grep -Fq '  oobe-nearby)' "$repo/crates/session/shell/tests/native_atspi_smoke.sh" \
  && ok 'the OS gate pairs a first-boot device with a nearby trusted one and surfaces helper failures' \
  || bad 'nearby pairing from first-boot onboarding has no real-boot gate'

# Apps opened on a slow machine keep their windows: the dev gate signs in
# again with every frame stalled as on a slow laptop and opens apps. The stall
# is a developer override that release compositors never read.
grep -Fq './os/dev/test-slow-frame-launch.sh --out "$TILO_RUN/slow-frames" --stall 50-130' "$os" \
  && grep -Fq 'TILO_TEST_FRAME_STALL_MS' "$repo/os/dev/test-slow-frame-launch.sh" \
  && grep -Fq '"TILO_TEST_FRAME_STALL_MS",' "$repo/crates/session/shell/src/compositor/dev_env.rs" \
  && grep -Fq 'super::dev_env::DEV_ENV_OVERRIDES' "$repo/crates/session/shell/src/compositor/frame_stall.rs" \
  && ok 'the dev gate opens apps at a slow machine'"'"'s frame times, behind the developer-override gate' \
  || bad 'apps opened on a slow machine have no real-boot gate'

python3 "$repo/tools/apps/app_release.py" --collection default > /dev/null \
  && python3 "$repo/tools/apps/app_release.py" --collection recovery > /dev/null \
  && ok 'named app collections resolve against the live manifest catalogue' \
  || bad 'an app collection is stale or invalid'

exit "$fail"
