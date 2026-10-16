#!/usr/bin/env bash
# The development signing tools proven natively: a disposable fixture image with private fixture
# keys and a local registry, run privileged because its tests nest Podman.
#
#   development_signing_native.sh context OUT   assemble the fixture's build context in OUT:
#                                               the tools, their tests, tilo-crypto built here
#                                               and the pinned Cosign (CI builder)
#   development_signing_native.sh run CONTEXT [PROOF]
#                                               build the fixture with Podman and run PROOF:
#                                               full (every fixture), prediction, retention or
#                                               prediction-retention (a privileged node)
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$repo"
# tilo-fs's build script writes test fixtures here; the source tree is read-only on t.
export TILO_TEST_ASSET_CACHE=${TILO_TEST_ASSET_CACHE:-${CARGO_TARGET_DIR:-$repo/target}/test-assets}

retention=(test_initramfs_trust.py initramfs_trust_native.py test_rebind_development_trust.py
  test_complete_initramfs_rebind.py test_rebind_development_rootfs.py
  test_retain_development_rootfs.py test_import_development_rootfs.py
  development_retention_native.py test_rootfs_layer_audit.py development_layer_audit_native.py
  development_rebind_native.py)

case ${1:-} in
  context)
    context=${2:?usage: development_signing_native.sh context OUT}
    rm -rf "$context"
    mkdir -p "$context/dev" "$context/91tilo-rollback"
    cp os/dev/{verify-development-source.py,development_boot.py,development_prediction.py,development-trust.py,install-public-trust.py,hardware_profile.py,development-signer.py,setup-development-signer.py,verify-authenticode-envelope.py,verify-development-loader.py,sign-development-rootfs.py,private_disk_image.py} "$context/dev/"
    cp os/dev/{retain-development-rootfs.py,import-development-rootfs.py,rebind-development-trust.py,initramfs-trust.py,complete-initramfs-rebind.py,rebind-development-rootfs.py} "$context/dev/"
    cp os/dev/{audit-rootfs-layers.py,installed_material_audit.py} "$context/dev/"
    cp os/tests/{development_source_registry.py,development_signer.py,test_development_signer.py,test_development_loader.py,test_development_boot.py,test_development_prediction.py,development_prediction_native.py,test_development_source.py,development_stage_reuse.py,development_rootfs.py,test_sign_development_rootfs.py,test_private_disk_image.py} "$context/"
    cp os/tests/{test_retain_development_rootfs.py,test_import_development_rootfs.py,test_rebind_development_trust.py,test_initramfs_trust.py,initramfs_trust_native.py,test_complete_initramfs_rebind.py,test_rebind_development_rootfs.py,test_development_trust.py,development_retention_native.py,tls_registry_fixture.py,test_tls_registry_fixture.py} "$context/"
    cp os/tests/{test_rootfs_layer_audit.py,development_layer_audit_native.py,development_rebind_native.py} "$context/"
    cp os/Containerfile.development-loader "$context/"
    cp -a os/usr/lib/dracut/modules.d/91tilo-rollback/. "$context/91tilo-rollback/"
    tools=${TMPDIR:-/tmp}/development-signing-tools
    mkdir -p "$tools"
    RUNNER_TEMP=$tools GITHUB_PATH=$tools/path TILO_GITHUB_ASSET_CACHE=$tools/assets \
      TILO_ASSET_STORE= bash os/dev/install-cached-cosign.sh >/dev/null
    cp "$tools/tilo-cosign-tools/cosign" "$context/cosign"
    cargo build --locked --release -p tilo-crypto
    cp "${CARGO_TARGET_DIR:-target}/release/tilo-crypto" "$context/tilo-crypto"
    cp os/tests/Containerfile.development-source "$context/Containerfile"
    ;;
  run)
    context=${2:?usage: development_signing_native.sh run CONTEXT [PROOF]}
    proof=${3:-full}
    # t runs this as root in a micro-VM step whose userspace is the CI builder: it brings its
    # own Podman, and with no systemd in the VM, cgroups and events are Podman's own.
    command -v podman >/dev/null \
      || dnf install -y --setopt=install_weak_deps=False podman >/dev/null
    podman=(podman --cgroup-manager=cgroupfs --events-backend=file)
    image="localhost/tilo-development-source-fixture:$$"
    trap '"${podman[@]}" image rm "$image" >/dev/null 2>&1 || true' EXIT
    "${podman[@]}" build --network host --file "$context/Containerfile" --tag "$image" "$context"
    # Nested Podman mounts only inside this disposable container; no host directory or
    # credential is mounted.
    run=("${podman[@]}" run --rm --privileged --network host --tmpfs /tmp --tmpfs /run)
    case $proof in
      full) "${run[@]}" "$image" ;;
      prediction)
        "${run[@]}" --entrypoint python3 "$image" /fixture/os/tests/development_prediction_native.py ;;
      retention | prediction-retention)
        fixtures=("${retention[@]}")
        [[ $proof == retention ]] || fixtures=(development_prediction_native.py "${fixtures[@]}")
        "${run[@]}" --entrypoint bash "$image" -uc \
          'status=0; for fixture in "$@"; do python3 "/fixture/os/tests/$fixture" || status=1; done; exit "$status"' \
          bash "${fixtures[@]}" ;;
      *) echo "unknown native proof: $proof" >&2; exit 2 ;;
    esac
    ;;
  *) echo 'usage: development_signing_native.sh context OUT | run CONTEXT [PROOF]' >&2; exit 2 ;;
esac
