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

desktop="$workflows/package-desktop.yml"
apps="$workflows/release-apps.yml"
os="$workflows/build-image.yml"
imager="$workflows/release-imager.yml"

if grep -Fq '[[ "$RPMS_REF" =~ ^code\.podesta\.ai/qos/rpms@sha256:[0-9a-f]{64}$ ]]' "$os" \
   && grep -Fq '[[ "$RPMS_REF" =~ ^code\.podesta\.ai/qos/rpms@sha256:[0-9a-f]{64}$ ]]' "$repo/os/dev/build-image.sh"; then
  ok 'CI and local OS builds accept the published digest-pinned RPM namespace'
else
  bad 'an OS build rejects the RPM namespace published by snapshot-rpms'
fi

cache_driver_ok=1
for workflow in build-builder-image.yml build-image.yml build-kernel.yml tilo-relay.yaml snapshot-rpms.yml; do
  file="$workflows/$workflow"
  if grep -Fq 'cache-to: type=registry' "$file" \
     && { ! grep -Fq 'docker/setup-buildx-action@8d2750c68a42422c14e847fe6c8ac0403b4cbd6f' "$file" \
          || ! grep -Fq 'driver: docker-container' "$file"; }; then
    printf 'registry cache workflow lacks a cache-export-capable Buildx driver: %s\n' \
      "$workflow" >&2
    cache_driver_ok=0
  fi
done
if [ "$cache_driver_ok" = 1 ]; then
  ok 'registry cache workflows select the docker-container Buildx driver'
else
  bad 'a registry cache workflow still uses the default docker driver'
fi

grep -Fq "github.event_name != 'pull_request'" "$workflows/build-kernel.yml" \
  && grep -Fq 'tilo-kernel-buildcache:{0},mode=min' "$workflows/build-kernel.yml" \
  && ! grep -Eq 'cache-to:.*tilo-kernel-buildcache:.*mode=max' "$workflows/build-kernel.yml" \
  && ok 'kernel cache export skips PRs and excludes its multi-gigabyte intermediate build tree' \
  || bad 'kernel cache export can duplicate work or exhaust a PR runner'

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

if grep -Fq 'needs: [plan, build, images, cache-cold, cache-warm]' "$workflows/build-builder-image.yml" \
  && grep -Fq 'needs: [plan, build, images, accept-builder]' "$workflows/build-builder-image.yml" \
  && grep -Fq "needs.accept-builder.result == 'success'" "$workflows/build-builder-image.yml" \
  && grep -Fq 'runner: runner-arm64' "$workflows/build-builder-image.yml" \
  && grep -Fq 'python3 os/dev/promote-native-index.py' "$workflows/build-builder-image.yml" \
  && grep -Fq 'runs-on: ${{ matrix.runner }}' "$workflows/check.yml"; then
  ok 'native CI builder publication waits for cache acceptance on both architectures'
else
  bad 'native CI builder or affected-package architecture coverage is missing'
fi

if grep -Fq 'x86_64) ref="$KERNEL_RPMS_X86_64_REF"; expected_arch=amd64' "$os" \
   && grep -Fq 'aarch64) ref="$KERNEL_RPMS_AARCH64_REF"; expected_arch=arm64' "$os" \
   && grep -Fq '[[ "$ref" =~ ^code\.podesta\.ai/qos/tilo-kernel@sha256:[0-9a-f]{64}$ ]]' "$os" \
   && grep -Fq 'cosign verify --key keys/cosign.pub "$ref"' "$os" \
   && grep -Fq 'cosign verify-attestation --key keys/cosign.pub' "$os" \
   && grep -Fq 'https://tilo.sh/buildtypes/kernel-rpm/v1' "$os" \
   && grep -Fq 'actual_arch="$(skopeo inspect "docker://$ref" | jq -er .Architecture)"' "$os" \
   && grep -Fq 'echo "KERNEL_RPMS_REF=$ref" >> "$GITHUB_ENV"' "$os" \
   && grep -Fq '[[ "$KERNEL_RPMS_REF" =~ ^code\.podesta\.ai/qos/tilo-kernel@sha256:[0-9a-f]{64}$ ]]' "$repo/os/dev/build-image.sh" \
   && grep -Fq 'cosign verify --key "$TILO_REPO/keys/cosign.pub" "$KERNEL_RPMS_REF"' "$repo/os/dev/build-image.sh" \
   && grep -Fq 'cosign verify-attestation' "$repo/os/dev/build-image.sh" \
   && grep -Fq 'https://tilo.sh/buildtypes/kernel-rpm/v1' "$repo/os/dev/build-image.sh"; then
  ok 'CI and local OS builds authenticate kernel digest, architecture, and provenance before consumption'
else
  bad 'an OS build can consume an unpinned or unauthenticated kernel repository'
fi

kernel_workflow="$workflows/build-kernel.yml"
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
   && grep -Fq 'export PATH="/usr/local/sbin:/usr/sbin:/sbin:$PATH"' "$repo/os/kernel/tests/test-rpm.sh" \
   && grep -Fq 'command -v modinfo >/dev/null' "$repo/os/kernel/tests/test-rpm.sh" \
   && grep -Fq 'modinfo -F name' "$repo/os/kernel/tests/test-rpm.sh"; then
  ok 'native kernel acceptance installs modinfo, firmware and EFI-image tools on apt and dnf runners'
else
  bad 'native kernel acceptance can invoke missing module or Secure Boot VM dependencies'
fi

if grep -Fq 'runner: runner-disk-100g' "$kernel_workflow" \
   && grep -Fq 'runner: runner-disk-100g-arm64' "$kernel_workflow" \
   && grep -Fq 'assert-native-arch.sh "${{ matrix.arch }}"' "$kernel_workflow" \
   && grep -Fq 'tilo-kernel-buildcache:${{ matrix.arch }}' "$kernel_workflow" \
   && grep -Fq 'KERNEL_RPMS_${{ matrix.arch }}_REF' "$kernel_workflow" \
   && grep -Fq 'promote-native-kernels:' "$kernel_workflow" \
   && grep -Fq 'KERNEL_RPMS_X86_64_REF=$KERNEL_RPMS_X86_64_REF' "$kernel_workflow" \
   && grep -Fq 'KERNEL_RPMS_AARCH64_REF=$KERNEL_RPMS_AARCH64_REF' "$kernel_workflow" \
   && grep -Fq 'kernel inputs changed during promotion; refusing stale push' "$kernel_workflow" \
   && grep -Fq "github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/main'" "$kernel_workflow" \
   && ! grep -Fq "github.event_name == 'workflow_dispatch' && github.ref != 'refs/heads/main'" "$kernel_workflow" \
   && grep -Fq 'kernel-build-support.sh stage-boot "$(uname -m)"' "$repo/os/kernel/Containerfile" \
   && grep -Fq 'aarch64) image=arch/arm64/boot/Image' "$repo/os/kernel/build-support.sh" \
   && grep -Fq 'x86_64) image=arch/x86/boot/bzImage' "$repo/os/kernel/build-support.sh" \
   && grep -Fq 'rpm-build make gcc clang lld bc flex bison rust cargo' "$repo/os/kernel/Containerfile" \
   && ! grep -Fq 'TILO_BUILDER_REF' "$repo/os/kernel/Containerfile" \
   && grep -Fq 'https://tilo.sh/buildtypes/kernel-rpm/v1' "$kernel_workflow" \
   && grep -Fq -- '--predicate kernel-provenance.json --type slsaprovenance1' "$kernel_workflow" \
   && grep -Fq 'cosign verify-attestation --key keys/cosign.pub' "$kernel_workflow" \
   && grep -Fq 'ExclusiveArch:  x86_64 aarch64' "$repo/os/kernel/tilo-kernel-payload.spec"; then
  ok 'kernel publication builds, signs, attests, and reports native x86-64 and ARM64 payloads'
else
  bad 'the two-architecture OS release does not have matching native kernel builds'
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

grep -Fq 'publish-os-payload:' "$desktop" \
  && grep -Fq 'TILO_DESKTOP_RELEASE_AUDIENCE' "$desktop" \
  && grep -Fq 'CONTINUOUS_OS_RELEASE: "0"' "$desktop" \
  && grep -Fq "if: (github.event_name == 'schedule' || github.event_name == 'workflow_dispatch') && github.ref == 'refs/heads/main'" "$desktop" \
  && grep -Fq 'TILO_DESKTOP_PAYLOAD_URL_X86_64' "$os" \
  && grep -Fq 'TILO_PREBUILT_DESKTOP=${{ env.TILO_PREBUILT_DESKTOP }}' "$os" \
  && grep -Fq 'DESKTOP_RELEASE_ARGS=""' "$repo/os/Containerfile" \
  && ok 'OS composition reuses the install-tested native desktop payload' \
  || bad 'OS releases still rebuild the desktop component'

grep -Fq 'with_browser=0' "$os" \
  && grep -Fq 'WITH_BROWSER: ${{ env.TILO_WITH_BROWSER }}' "$os" \
  && grep -Fq 'max-parallelism = 1' "$os" \
  && test "$(grep -Fc 'TILO_CFS_DIGEST=00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000' "$os")" -eq 2 \
  && grep -Fq 'TILO_PROVISIONAL_CFS_DIGEST="$(printf '\''0%.0s'\'' {1..128})"' "$repo/os/dev/build-image.sh" \
  && ! grep -Fq 'id=tilo-composefs-' "$repo/os/Containerfile" \
  && grep -Fq 'find /build/target -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +' "$repo/os/Containerfile" \
  && grep -Fq 'CONTINUOUS_RELEASE: "0"' "$os" \
  && ! grep -Fq 'token: ${{ secrets.REGISTRY_TOKEN }}' "$os" \
  && ! grep -Fq 'fetch-depth: 0' "$os" \
  && grep -Fq "if: steps.release-mode.outputs.validate == 'true'" "$os" \
  && grep -Fq 'installed OS, desktop, and app integration passed; publication was not requested' "$os" \
  && grep -Fq "if: steps.release-mode.outputs.publish == 'true'" "$os" \
  && ok 'ordinary OS integration checks omit the release-only browser engine' \
  || bad 'OS branch validation still rebuilds or requires the WPE release snapshot'

if ! grep -q '^  pull_request:' "$os" \
  && ! sed -n '/^  push:/,/^  workflow_dispatch:/p' "$os" | grep -q '^    branches:' \
  && grep -Fq '  schedule:' "$os" \
  && grep -Fq '  workflow_dispatch:' "$os" \
  && grep -Fq 'native-atspi-smoke.sh oobe-actions' "$os" \
  && grep -Fq 'installed-atspi-apps.sh' "$os" \
  && grep -Fq 'org.a11y.atspi.Action.GetActions' "$repo/crates/session/shell/tests/native_atspi_smoke.sh" \
  && grep -Fq 'org.a11y.atspi.Action.DoAction 0' "$repo/crates/session/shell/tests/native_atspi_smoke.sh" \
  && grep -Fq 'org.a11y.atspi.Component.GrabFocus' "$repo/crates/session/shell/tests/native_atspi_smoke.sh" \
  && grep -Fq 'org.a11y.atspi.EditableText.SetTextContents' "$repo/crates/session/shell/tests/native_atspi_smoke.sh" \
  && grep -Fq 'org.a11y.atspi.Text.GetText 0 -1' "$repo/crates/session/shell/tests/native_atspi_smoke.sh" \
  && grep -Fq 'wait_for_names "New Folder"' "$repo/apps/file-explorer/tests/atspi_smoke.sh" \
  && grep -Fq 'at-spi2-core' "$repo/os/Containerfile" \
  && grep -Fq 'atspi-external-client.py' "$repo/os/Containerfile" \
  && grep -Fq 'accessibility.atspi_acceptance' "$repo/apps/settings/tilo-app.toml"; then
  ok 'OS release VM drives compositor and manifest-certified app AT-SPI actions over its session bus'
else
  bad 'AT-SPI release gate can ship an app outside the readable, action-capable desktop endpoint'
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
  && grep -Fq "cancel-in-progress: \${{ github.event_name == 'pull_request' }}" "$apps" \
  && grep -Fq '$package_api/tilo-app-collections/$VERSION/' "$apps" \
  && grep -Fq '$package_api/tilo-app-$name/$VERSION/$file' "$apps" \
  && grep -Fq 'PRIOR_X86_URL' "$apps" \
  && grep -Fq 'if [[ $GITHUB_REF == refs/heads/main ]]; then' "$apps" \
  && grep -Fq 'dist/app-release/runtime/**' "$apps" \
  && grep -Fq 'update_variable TILO_APP_COLLECTION_URL' "$apps" \
  && grep -Fq '$GITHUB_EVENT_NAME == push && $GITHUB_REF == refs/heads/main' "$apps" \
  && grep -Fq '${REQUESTED_PUBLISH:-false} == true' "$apps" \
  && grep -Fq 'REQUESTED_ARCHITECTURES: ${{ github.event.inputs.architectures }}' "$apps" \
  && grep -Fq 'tools/apps/app_release.py --architecture-matrix' "$apps" \
  && awk '{sub(/\r$/, "")} /^  plan:/{p=1;next} p && /^  [[:alnum:]_-]+:/{exit} p' "$apps" | grep -Fx '      builder_image: ${{ steps.builder.outputs.image }}' >/dev/null \
  && awk '{sub(/\r$/, "")} /^  plan:/{p=1;next} p && /^  [[:alnum:]_-]+:/{exit} p' "$apps" | grep -Fx '        id: builder' >/dev/null \
  && awk '{sub(/\r$/, "")} /^  plan:/{p=1;next} p && /^  [[:alnum:]_-]+:/{exit} p' "$apps" | grep -Fx '        run: python3 -I tools/ci/python.py tools/ci/resolve_builder.py' >/dev/null \
  && awk '{sub(/\r$/, "")} /^  build:/{p=1;next} p && /^  [[:alnum:]_-]+:/{exit} p' "$apps" | grep -Fx '    needs: [plan, authorize-publish]' >/dev/null \
  && awk '{sub(/\r$/, "")} /^  build:/{p=1;next} p && /^  [[:alnum:]_-]+:/{exit} p' "$apps" | grep -Fx '      image: ${{ needs.plan.outputs.builder_image }}' >/dev/null \
  && grep -Fq 'actions/upload-artifact@ff15f0306b3f739f7b6fd43fb5d26cd321bd4de5' "$apps" \
  && grep -Fq 'enable-openid-connect: true' "$apps" \
  && grep -Fq 'TILO_PACKAGE_PUBLIC_BASE_URL: ${{ vars.TILO_PACKAGE_PUBLIC_BASE_URL }}' "$apps" \
  && grep -Fq 'bundle_url="$package_public_api/tilo-app-collections/' "$apps" \
  && grep -Fq 'bundle_upload_url="$package_api/tilo-app-collections/' "$apps" \
  && ! grep -Fq 'channel_url=$bundle_upload_url' "$apps" \
  && grep -Fq 'tools/upload_immutable_packages.sh "$upload_manifest"' "$apps" \
  && grep -Fq 'tools/reuse_prior_app_collection.sh' "$apps" \
  && grep -Fq 'audience=$MAIN_AUDIENCE' "$apps" \
  && ! grep -Fq 'TILO_APP_RELEASE_TAG_AUDIENCE' "$apps" \
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
  && test "$(grep -Fc 'http://forgejo.forgejo.svc.cluster.local:3000/api/packages/qos/generic/*' "$os")" -eq 2 \
  && test "$(grep -Fc -- "--proto '=https' --proto-redir '=https'" "$os")" -eq 4 \
  && test "$(grep -Fc 'https:\/\/code.podesta.ai}' "$os")" -eq 2 \
  && ! grep -Fq -- "--proto '=http'" "$os" \
  && ! grep -Fq '[[ $EVENT_NAME != pull_request ]] || exit 0' "$os" \
  && grep -Fq 'Authorization: Bearer $jwt' "$os" \
  && grep -Fq 'TILO_PREBUILT_APPS=${{ env.TILO_PREBUILT_APPS }}' "$os" \
  && grep -Fq 'TAPP_DIR=/out/prebuilt-apps' "$repo/os/Containerfile" \
  && grep -Fq 'cp -a /build/os/app-runtime/tilo-runtime /usr/lib/' "$repo/os/Containerfile" \
  && grep -Fq 'RUNTIME_BUNDLE_IDENTITY_FILE' "$repo/dist/apps/package-apps.sh" \
  && grep -Fq -- '-C codegen-units=1' "$repo/tools/ci/rustc-thin-app-wrapper.sh" \
  && ! grep -Fq -- '--icf=all' "$repo/tools/ci/rustc-thin-app-wrapper.sh" \
  && grep -Fq 'tools/reuse_prior_app_collection.sh' "$apps" \
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
  && grep -Fq 'git jq ripgrep time yt-dlp' "$repo/os/Containerfile.builder" \
  && ok 'OS and CI builder images install checksum-pinned native cache clients' \
  || bad 'an ARM build image can still install the x86 compiler-cache client'

grep -Fq "PKGEXT='.pkg.tar.zst'" "$repo/dist/arch/build-arch.sh" \
  && grep -Fq 'makepkg --config "$makepkg_config"' "$repo/dist/arch/build-arch.sh" \
  && ok 'Arch packages use the release artifact compression on every architecture' \
  || bad 'an Arch base image can silently change the release package extension'

python3 "$repo/tools/apps/app_release.py" --collection default > /dev/null \
  && python3 "$repo/tools/apps/app_release.py" --collection recovery > /dev/null \
  && ok 'named app collections resolve against the live manifest catalogue' \
  || bad 'an app collection is stale or invalid'

exit "$fail"
