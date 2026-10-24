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
# The image's binaries are compiled outside the image build, by this script.
compile="$repo/os/image/compile.sh"
imager="$workflows/release-imager.yml"

if grep -Fq '[[ "$RPMS_REF" =~ ^registry\.tilo\.so/tilo/rpms@sha256:[0-9a-f]{64}$ ]]' "$repo/os/dev/build-image.sh"; then
  ok 'local OS builds accept the published digest-pinned RPM namespace'
else
  bad 'an OS build rejects the RPM namespace published by snapshot-rpms'
fi

cache_driver_ok=1
grep -Fq 'driver: docker-container' "$repo/.forgejo/actions/setup-buildx/action.yml" \
  && grep -Fq 'docker/setup-buildx-action@8d2750c68a42422c14e847fe6c8ac0403b4cbd6f' "$repo/.forgejo/actions/setup-buildx/action.yml" \
  || cache_driver_ok=0
if [ "$cache_driver_ok" = 1 ]; then
  ok 'the shared Buildx action selects the cache-export-capable docker-container driver'
else
  bad 'the shared Buildx action uses the default docker driver'
fi

# A persistent builder keeps BuildKit state on the runner slot between jobs.
# Its name carries the trust class, so a pull request never writes state a
# main build reads, and publications (tags, dispatches, the OS and kernel
# images) never opt in: no remaining workflow does.
persistent_ok=1
for file in "$workflows"/*.yml "$workflows"/*.yaml; do
  if grep -Eq '^ +persistent:' "$file"; then
    printf 'persistent BuildKit builder in a workflow: %s\n' "$(basename "$file")" >&2
    persistent_ok=0
  fi
done
grep -Fq "TRUST: \${{ github.event_name == 'push' && github.ref == 'refs/heads/main' && 'trusted' || 'untrusted' }}" "$repo/.forgejo/actions/setup-buildx/action.yml" \
  && grep -Fq 'name="tilo-$TRUST-$digest"' "$repo/.forgejo/actions/setup-buildx/action.yml" \
  && grep -Fq 'default: "false"' "$repo/.forgejo/actions/setup-buildx/action.yml" \
  || persistent_ok=0
if [ "$persistent_ok" = 1 ]; then
  ok 'persistent BuildKit state is split by trust class and never reaches publications'
else
  bad 'a persistent BuildKit builder can mix trust classes or serve a publication'
fi

# t builds, signs and publishes the kernel, its buildroot and the CI builder (t.toml, generated
# from tools/ci/products.toml); these are the scripts its checks run.
t_toml="$repo/t.toml"
publish="$repo/tools/ci/publish"
kernel_script="$publish/kernel.sh"
# A t check's field, one value per line (a string as itself, an array per element, a table as
# key=value); fails when t.toml declares no such check, so a renamed check cannot pass unobserved.
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
# t_has CHECK FIELD LINE: the field has LINE as one value (an array) or one line (a run script).
t_has() {
  local values
  values="$(tcheck "$1" "$2")" && grep -Fxq -- "$3" <<<"$values"
}

# A registry hiccup after a completed compile must not discard the build: the kernel is built
# into an archive whose digest the build records, and publication pushes that archive, retrying
# a failed copy a bounded number of times, and refuses a registry that holds any other digest.
# A commit's tag is pushed once, so a rebuild on the same commit never moves it off a digest
# already pinned, and the pins name the digest each receipt says the tag holds.
t_has publish-kernel run "bash tools/ci/publish/oci-publish.sh release registry.tilo.so/tilo/tilo-kernel /target/publish/kernel/x86_64 'once:sha-{commit}-x86_64'" \
  && t_has publish-kernel run "bash tools/ci/publish/oci-publish.sh release registry.tilo.so/tilo/tilo-kernel /target/publish/kernel/aarch64 'once:sha-{commit}-aarch64'" \
  && grep -Fq 'archive_digest "$out/image.oci.tar" > "$out/digest"' "$kernel_script" \
  && grep -Fq 'for attempt in 1 2 3 4 5; do' "$publish/oci-publish.sh" \
  && grep -Fq '[[ sha256:$pushed == "$digest" ]] || die' "$publish/oci-publish.sh" \
  && grep -Fq '"$dir/$arch/receipt.json")' <<<"$(sed -n '/^  pins)$/,/^    ;;$/p' "$kernel_script")" \
  && ok 'kernel publication retries a failed push of the completed build and keeps its digest' \
  || bad 'a registry push hiccup can fail kernel publication or publish another digest'

# The browser engine is built inside the image build from the pinned CEF
# distribution; no image build takes a separately compiled engine stage.
grep -Fq 'ARG CHROMIUM_RUNTIME_VERSION=' "$repo/os/Containerfile" \
  && ok 'OS image builds compile the browser engine runtime themselves' \
  || bad 'an OS image build still takes a separately compiled browser engine'

# Both kernels are built natively and published together; the pin moves both at once, only
# after each is verified (signature and provenance) as the OS build verifies it.
kernel_pin="$(sed -n '/^  pin)$/,/^    ;;$/p' "$kernel_script")"
kernel_pins="$(sed -n '/^  pins)$/,/^    ;;$/p' "$kernel_script")"
if t_has image-kernel-{arch} requires 'arch={arch}' \
  && t_has image-kernel-{arch} matrix 'arch=['"'"'x86_64'"'"', '"'"'aarch64'"'"']' \
  && grep -Fq '[[ $(uname -m) == "$arch" ]] || die "the $arch kernel is built natively' "$kernel_script" \
  && grep -Fq 'x86_64) platform=linux/amd64 key=KERNEL_BUILDROOT_X86_64_REF ;;' "$kernel_script" \
  && grep -Fq 'aarch64) platform=linux/arm64 key=KERNEL_BUILDROOT_AARCH64_REF ;;' "$kernel_script" \
  && t_has sign-kernel needs image-kernel-x86_64 && t_has sign-kernel needs image-kernel-aarch64 \
  && t_has sign-kernel needs provenance-kernel \
  && t_has publish-kernel needs sign-kernel \
  && t_has publish-kernel run 'bash tools/ci/publish/kernel.sh pins /target/publish/kernel' \
  && grep -Fq 'for arch in x86_64 aarch64; do' <<<"$kernel_pins" \
  && grep -Fq "key=KERNEL_RPMS_\$(printf '%s' \"\$arch\" | tr '[:lower:]' '[:upper:]')_REF" <<<"$kernel_pins" \
  && grep -Fq 'bash os/dev/verify-kernel-provenance.sh "$ref" --architecture "$arch"' <<<"$kernel_pin" \
  && grep -Fq '[[ $x86 != "$arm" ]] || die' <<<"$kernel_pin" \
  && grep -Fq 'python3 os/dev/open-snapshot-pr.py pins kernel --x86-ref "$x86" --arm-ref "$arm" > os/kernel-snapshot.env' <<<"$kernel_pin" \
  && grep -Fq 'cosign verify-attestation --key "$key" --type slsaprovenance1 "$ref"' "$repo/os/dev/verify-kernel-provenance.sh"; then
  ok 'kernel publication requires both verified native builds and atomic architecture pin promotion'
else
  bad 'kernel publication can omit native ARM64 or bypass verified architecture pin promotion'
fi

# The kernel and its Secure Boot probe inputs come from one build stage with the same build
# arguments, so they share the compilation and cannot diverge; the build has explicit bounds.
if [ "$(grep -Fc '"${args[@]}"' "$kernel_script")" -eq 2 ] \
  && grep -Fq -- '--build-arg "KERNEL_BUILD_JOBS=$(nproc)"' "$kernel_script" \
  && grep -Fq -- '--target secureboot-probe-inputs' "$kernel_script" \
  && grep -q '^memory=' <<<"$(tcheck image-kernel-{arch} requirement)" \
  && grep -q '^cores=' <<<"$(tcheck image-kernel-{arch} requirement)" \
  && [ -n "$(tcheck image-kernel-{arch} timeout_secs)" ]; then
  ok 'native kernel and Secure Boot probe share explicit resource bounds and build arguments'
else
  bad 'native kernel builds can bypass resource bounds or diverge on their build arguments'
fi

# Neither architecture's CI builder is signed or published before t's compiler cache is proven
# to work from it (the warm canary reads back what the cold one wrote, in another job).
builder_ok=1
for arch in x86_64 aarch64; do
  t_has "canary-ci-builder-cache-warm-{arch}" needs "canary-ci-builder-cache-cold-{arch}" || builder_ok=0
  for check in sign-ci-builder sign-dev-ci-builder publish-ci-builder; do
    t_has "$check" needs "canary-ci-builder-cache-warm-$arch" || builder_ok=0
  done
done
t_has image-ci-builder-{arch} requires 'arch={arch}' || builder_ok=0
t_has image-ci-builder needs image-ci-builder-x86_64 && t_has image-ci-builder needs image-ci-builder-aarch64 \
  || builder_ok=0
if [ "$builder_ok" = 1 ]; then
  ok 'native CI builder publication waits for cache acceptance on both architectures'
else
  bad 'native CI builder publication can skip cache acceptance on an architecture'
fi

kernel_verify="$repo/os/dev/verify-kernel-provenance.sh"
if grep -Fq '[[ "$KERNEL_RPMS_REF" =~ ^registry\.tilo\.so/tilo/tilo-kernel@sha256:[0-9a-f]{64}$ ]]' "$repo/os/dev/build-image.sh" \
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
  ok 'OS builds authenticate kernel digest, architecture, and provenance before consumption'
else
  bad 'an OS build can consume an unpinned or unauthenticated kernel repository'
fi

kernel_containerfile="$repo/os/kernel/Containerfile"
kernel_build="$(sed -n '/^  build)$/,/^    ;;$/p' "$kernel_script")"
verify_line="$(grep -nF 'cosign verify --key keys/cosign.pub "$buildroot"' <<<"$kernel_build" | head -1 | cut -d: -f1)"
first_build_line="$(grep -nF 'b build ' <<<"$kernel_build" | head -1 | cut -d: -f1)"
if grep -Fq '. os/kernel-buildroot-snapshot.env' <<<"$kernel_build" \
   && grep -Fq '[[ $buildroot =~ ^registry\.tilo\.so/tilo/tilo-kernel-buildroot@sha256:[0-9a-f]{64}$ ]]' <<<"$kernel_build" \
   && [ -n "$verify_line" ] && [ -n "$first_build_line" ] && [ "$verify_line" -lt "$first_build_line" ] \
   && grep -Fq -- '--build-arg "KERNEL_BUILDROOT_REF=$buildroot"' <<<"$kernel_build" \
   && ! grep -Eq 'RPMS_REF|rpms-snapshot\.env|derive-rpm-package-set' "$kernel_script" \
   && grep -Fq 'FROM ${KERNEL_BUILDROOT_REF} AS build-toolchain' "$kernel_containerfile" \
   && ! grep -Eq 'RPMS_REF|BASE_REF|from=rpms|dnf5? (install|download)|enablerepo' "$kernel_containerfile" \
   && grep -Fq '! find /etc/yum.repos.d -type f -print -quit' "$kernel_containerfile" \
   && grep -Fq 'cp /usr/src/tilo-kernel/kernel-*.src.rpm /work/kernel.src.rpm' "$kernel_containerfile"; then
  ok 'kernel compilation starts only from the signed native kernel buildroot, with no repository fallback'
else
  bad 'kernel compilation can start from an unauthenticated buildroot or fall back to a package repository'
fi
acceptance="$publish/kernel-acceptance.sh"
if grep -Fq 'x86_64) qemu=qemu-system-x86-core firmware=edk2-ovmf ;;' "$acceptance" \
   && grep -Fq 'aarch64) qemu="qemu-system-aarch64-core qemu-device-uefi-vars" firmware=edk2-aarch64 ;;' "$acceptance" \
   && grep -Fq 'python3 rpm zstd dosfstools mtools podman $qemu "$firmware"' "$acceptance" \
   && grep -Eq 'dnf install .* kmod ' "$acceptance" \
   && grep -Fq 'bash os/kernel/tests/test-rpm.sh' "$acceptance" \
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
  ok 'native kernel acceptance installs modinfo, firmware and EFI-image tools before it runs'
else
  bad 'native kernel acceptance can invoke missing module or Secure Boot VM dependencies'
fi

# Each architecture's published reference lands in its own upper-case pin variable; the pin
# (pin-kernel, on main, from stable's `kernel` pointer) commits only os/kernel-snapshot.env and
# only when main would build the same kernels, so it never regresses main to older inputs.
if t_has pin-kernel on main && [ "$(tcheck pin-kernel on)" = main ] \
   && [ "$(tcheck pin-kernel pushes)" = os/kernel-snapshot.env ] \
   && [ "$(tcheck pin-kernel reads_pointers)" = stable:kernel ] \
   && t_has pin-kernel run 'bash tools/ci/publish/kernel.sh pin' \
   && [ "$(tcheck publish-kernel on)" = stable ] && [ "$(tcheck sign-kernel on)" = stable ] \
   && grep -Fq 'if [[ $built_from != "$(inputs_digest)" ]]; then' <<<"$kernel_pin" \
   && grep -Fq 'built_from=$(sed -n '"'"'s/^INPUTS_SHA256=//p'"'"' <<<"$T_POINTER_KERNEL")' <<<"$kernel_pin" \
   && grep -Fq 'printf '"'"'%s\n'"'"' "${lines[@]}" "INPUTS_SHA256=$(inputs_digest)" > "$dir/pins.env"' <<<"$kernel_pins" \
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
   && grep -Fq '"buildType": "https://tilo.sh/buildtypes/kernel-rpm/v1"' "$kernel_script" \
   && t_has sign-kernel run 'bash tools/ci/publish/oci-attest.sh registry.tilo.so/tilo/tilo-kernel /target/publish/kernel/x86_64' \
   && t_has sign-kernel run 'bash tools/ci/publish/oci-attest.sh registry.tilo.so/tilo/tilo-kernel /target/publish/kernel/aarch64' \
   && grep -Fq 'type=${4:-slsaprovenance1}' "$publish/oci-attest.sh" \
   && grep -Fq 'args=(--yes --type "$type" --predicate "$predicate"' "$publish/oci-attest.sh" \
   && grep -Fq 'cosign verify-attestation --key "$verify_key" --type "$type"' "$publish/oci-publish.sh" \
   && grep -Fq 'verify_key=$repo_root/keys/cosign.pub' "$publish/oci-publish.sh" \
   && grep -Fq 'ExclusiveArch:  x86_64 aarch64' "$repo/os/kernel/tilo-kernel-payload.spec"; then
  ok 'kernel publication builds, signs, attests, and reports native x86-64 and ARM64 payloads'
else
  bad 'the two-architecture OS release does not have matching native kernel builds'
fi

# Only kernel inputs rebuild the kernel. The OS RPM snapshot and base are not
# kernel inputs: the kernel builds on its own signed buildroot, so an OS
# package change never invalidates the pinned kernel.
kernel_inputs="$(tcheck image-kernel-{arch} inputs)" || kernel_inputs=""
rpms_inputs="$(tcheck image-rpms-{arch} inputs; tcheck provenance-rpms inputs)" || rpms_inputs=""
kernel_digest="$(sed -n '/^inputs_digest() {/,/^}/p' "$kernel_script")"
if grep -Fxq 'os/kernel-buildroot-snapshot.env' <<<"$kernel_inputs" \
   && grep -Fxq 'os/kernel/**' <<<"$kernel_inputs" \
   && ! grep -Eq 'rpms-snapshot\.env|base-snapshot\.env|derive-rpm-package-set|kernel-snapshot\.env' <<<"$kernel_inputs" \
   && [ -n "$rpms_inputs" ] \
   && ! grep -Eq '^os/kernel(/|-)' <<<"$rpms_inputs" \
   && ! grep -Fq 'os/kernel/Containerfile' "$publish/rpms-build.sh" \
   && ! grep -Fq 'os/kernel/Containerfile' "$repo/os/dev/derive-rpm-package-set.py" \
   && grep -Fq 'os/kernel-buildroot-snapshot.env' <<<"$kernel_digest" \
   && ! grep -Eq 'base-snapshot\.env|kernel-snapshot\.env|rpms' <<<"$kernel_digest"; then
  ok 'only kernel inputs rebuild the kernel; OS package changes do not'
else
  bad 'an OS package or base change can invalidate or rebuild the kernel'
fi

grep -Fq 'tags: ["desktop-v*"]' "$desktop" \
  && grep -Fq 'tags: ["apps-v*", "app-*-v*"]' "$apps" \
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
desktop_signer=$(sed -n '/^  sign-rpm:$/,/^  publish-os-payload:$/p' "$desktop")
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
  && grep -Fq 'needs: [validate, package, sign-rpm]' "$desktop" \
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
  && grep -Fq 'DESKTOP_RELEASE_ARGS=""' "$compile" \
  && ok 'OS composition reuses the install-tested native desktop payload' \
  || bad 'OS releases still rebuild the desktop component'

grep -Fq '"source": sys.argv[3]' "$repo/dist/desktop/package-os-payload.sh" \
  && ok 'OS composition reuses the desktop payload only while no Rust or Cargo input changed since it was built' \
  || bad 'OS composition can ship a desktop payload older than its own compositor sources'

grep -Fq 'TILO_PROVISIONAL_CFS_DIGEST="$(printf '\''0%.0s'\'' {1..128})"' "$repo/os/dev/build-image.sh" \
  && ! grep -Fq 'id=tilo-composefs-' "$repo/os/Containerfile" \
  && grep -Fq 'find "$target" -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +' "$compile" \
  && ok 'ordinary OS integration checks run without release-only steps' \
  || bad 'OS branch validation still requires release-only steps'

# CI and os/dev/build-image.sh derive the Windows provider with one command, so
# x86_64 can never drop it and ARM includes it as soon as both pins exist.
# Every os/Containerfile build passes that derived value: the nine image and
# builder-stage builds and the windows-compat-sysext pack.
grep -Fq 'windows_provider.py" mode --arch "$TILO_WINDOWS_ARCH"' "$repo/os/dev/build-image.sh" \
  && grep -Fq -- '--build-arg "TILO_WITH_WINDOWS=$TILO_WITH_WINDOWS"' "$repo/os/dev/build-image.sh" \
  && grep -Fq 'FROM windows-provider-${TILO_WINDOWS_ARCH}-${TILO_WITH_WINDOWS} AS windows-provider' "$repo/os/Containerfile" \
  && ok 'CI and the local builder derive the Windows provider from the same pins' \
  || bad 'the Windows provider mode is chosen differently in CI and os/dev/build-image.sh'

if grep -Fq 'org.freedesktop.PolicyKit1.Authority.CheckAuthorization' "$repo/crates/session/shell/tests/native_atspi_smoke.sh" \
  && grep -Fq 'org.a11y.atspi.Action.GetActions' "$repo/crates/session/shell/tests/native_atspi_smoke.sh" \
  && grep -Fq 'org.a11y.atspi.Action.DoAction 0' "$repo/crates/session/shell/tests/native_atspi_smoke.sh" \
  && grep -Fq 'org.a11y.atspi.Component.GrabFocus' "$repo/crates/session/shell/tests/native_atspi_smoke.sh" \
  && grep -Fq 'org.a11y.atspi.EditableText.SetTextContents' "$repo/crates/session/shell/tests/native_atspi_smoke.sh" \
  && grep -Fq 'org.a11y.atspi.Text.GetText -- 0 -1' "$repo/crates/session/shell/tests/native_atspi_smoke.sh" \
  && grep -Fq 'wait_for_names "New Folder"' "$repo/apps/file-explorer/tests/atspi_smoke.sh" \
  && grep -Fq 'at-spi2-core' "$repo/os/Containerfile" \
  && grep -Fq '"tools/atspi_external_client.py atspi-external-client.py"' "$repo/os/dev/vm.sh" \
  && ! grep -Fq 'atspi_external_client' "$repo/os/Containerfile" \
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
  && awk '/^on:/ { on = 1 } on && /^  push:/ { push = 1 } push && /branches: \["main"\]/ { main = 1 }
          main && /^    paths:/ { filtered = 1 } /^  workflow_dispatch:/ { push = 0 }
          END { exit !(main && filtered) }' "$imager" \
  && ok 'imager native jobs run for main pushes that touch the Installer (its signed prereleases), explicit release rehearsals and tags' \
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
  && awk '{sub(/\r$/, "")} /^  recover-publication-97894:/{p=1;next} p && /^  [[:alnum:]_-]+:/{exit} p' "$apps" | grep -Fx '          FORGEJO_ARTIFACT_TOKEN: ${{ github.token }}' >/dev/null \
  && awk '{sub(/\r$/, "")} /^  recover-publication-97894:/{p=1;next} p && /^  [[:alnum:]_-]+:/{exit} p' "$apps" | grep -Fx '          VERIFIER_BUNDLE_READ_TOKEN: ${{ secrets.TILO_VERIFIER_BUNDLE_READ_TOKEN }}' >/dev/null \
  && ok 'app collections serialize channel updates and remain durable across workflows' \
  || bad 'app releases can cancel a required channel update or cannot be consumed later'

grep -Fq 'TAPP_DIR=/out/prebuilt-apps' "$repo/os/Containerfile" \
  && grep -Fq 'cp -a os/app-runtime/tilo-runtime "$out/prebuilt-runtime/"' "$compile" \
  && grep -Fq 'RUNTIME_BUNDLE_IDENTITY_FILE' "$repo/dist/apps/package-apps.sh" \
  && grep -Fq -- '-C codegen-units=1' "$repo/tools/ci/rustc-thin-app-wrapper.sh" \
  && ! grep -Fq -- '--icf=all' "$repo/tools/ci/rustc-thin-app-wrapper.sh" \
  && grep -Fq 'tools/reuse_prior_app_collection.sh' "$apps" "$apps_publish" \
  && grep -Fq 'shared runtime changed; the full collection must be rebuilt' \
       "$repo/tools/reuse_prior_app_collection.sh" \
  && ok 'OS builds consume OIDC-authenticated pinned app and desktop inputs without rebuilding them' \
  || bad 'OS releases are still coupled to app source builds'

grep -Fq 'SCCACHE_SHA256_AARCH64=821a86343191aa1cbab74bd42f9e93c9a63bf85e4742945f40d3ae84193c1c77' "$repo/os/Containerfile" \
  && grep -Fq 'sccache_arch=aarch64' "$repo/os/Containerfile" \
  && grep -Fq 'sccache did not start; compiling without it' "$compile" \
  && grep -Fq 'export RUSTC_WRAPPER=' "$compile" \
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
grep -Fq 'lan_qemu_args' "$repo/os/dev/vm.sh" \
  && grep -Fq 'native-atspi-smoke.sh oobe-nearby' "$repo/os/dev/test-nearby-pairing.sh" \
  && grep -Fq "chmod 3750 /var/lib/tilo/enrollment-commits" "$repo/os/dev/test-nearby-pairing.sh" \
  && grep -Fq '  oobe-nearby)' "$repo/crates/session/shell/tests/native_atspi_smoke.sh" \
  && ok 'the OS gate pairs a first-boot device with a nearby trusted one and surfaces helper failures' \
  || bad 'nearby pairing from first-boot onboarding has no real-boot gate'

# Apps opened on a slow machine keep their windows: the dev gate signs in
# again with every frame stalled as on a slow laptop and opens apps. The stall
# is a developer override that release compositors never read.
grep -Fq 'TILO_TEST_FRAME_STALL_MS' "$repo/os/dev/test-slow-frame-launch.sh" \
  && grep -Fq '"TILO_TEST_FRAME_STALL_MS",' "$repo/crates/session/shell/src/compositor/dev_env.rs" \
  && grep -Fq 'super::dev_env::DEV_ENV_OVERRIDES' "$repo/crates/session/shell/src/compositor/frame_stall.rs" \
  && ok 'the dev gate opens apps at a slow machine'"'"'s frame times, behind the developer-override gate' \
  || bad 'apps opened on a slow machine have no real-boot gate'

python3 "$repo/tools/apps/app_release.py" --collection default > /dev/null \
  && python3 "$repo/tools/apps/app_release.py" --collection recovery > /dev/null \
  && ok 'named app collections resolve against the live manifest catalogue' \
  || bad 'an app collection is stale or invalid'

exit "$fail"
