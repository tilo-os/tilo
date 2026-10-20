#!/usr/bin/env bash
# Every remote workflow action is executable third-party code. Require an exact
# commit rather than a mutable tag or branch across the entire workflow set.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflows="$repo/.forgejo/workflows"
cosign_action="$repo/.forgejo/actions/install-cosign/action.yml"
fail=0

manifest="$repo/os/external-assets/github-assets.tsv"
package_workflow="$workflows/release-desktop.yml"
kernel_workflow="$workflows/build-kernel.yml"
rpm_snapshot_workflow="$workflows/build-rpms.yml"
containerfile="$repo/os/Containerfile"
visual_baselines="$repo/tools/ci/visual-baselines.sh"
github_cache_workflows=(
  "$workflows/build-ci-builder.yml"
  "$workflows/build-image.yml"
  "$workflows/release-desktop.yml"
  "$workflows/release-imager.yml"
  "$workflows/build-rpms.yml"
)
cosign_workflows=(
  "$workflows/build-ci-builder.yml"
  "$workflows/build-image.yml"
  "$workflows/build-kernel.yml"
  "$workflows/release-relay.yml"
  "$workflows/release-imager.yml"
  "$workflows/build-rpms.yml"
  "$workflows/build-buildroots.yml"
  "$workflows/dev-disk.yml"
)
dev_disk_workflow="$workflows/dev-disk.yml"
syft_workflow="$workflows/build-image.yml"
release_signed_workflows=(
  "$workflows/build-ci-builder.yml"
  "$workflows/build-image.yml"
  "$workflows/build-kernel.yml"
  "$workflows/release-relay.yml"
  "$workflows/build-rpms.yml"
)

if python3 "$repo/tools/policy/check_workflow_actions.py"; then
  printf 'ok    every remote workflow action is pinned to a full commit SHA\n'
else
  fail=1
fi

if grep -Fq "group: snapshot-rpms-\${{ github.event_name == 'workflow_dispatch' && 'publication' || github.sha }}" "$rpm_snapshot_workflow" \
   && grep -Fq "cancel-in-progress: false" "$rpm_snapshot_workflow" \
   && grep -Fq 'python3 os/dev/rpm-snapshot-fingerprint.py' "$rpm_snapshot_workflow" \
   && grep -Fq './os/dev/resolve-rpm-snapshot.sh' "$rpm_snapshot_workflow" \
   && ! grep -Fq 'workflow_call:' "$rpm_snapshot_workflow" \
   && grep -Fq 'git checkout --detach FETCH_HEAD' "$kernel_workflow"; then
  printf 'ok    RPM snapshot refreshes cannot starve and stale runs cannot publish\n'
else
  printf 'RPM snapshot refreshes may be cancelled indefinitely or publish stale pins\n' >&2
  fail=1
fi

if [ "$(grep -Fc '"crates/system/tilo-crypto/Cargo.toml"' "$kernel_workflow")" -eq 2 ] \
   && [ "$(grep -Fc '"crates/system/tilo-crypto/src/lib.rs"' "$kernel_workflow")" -eq 2 ] \
   && [ "$(grep -Fc '"crates/system/tilo-crypto/src/main.rs"' "$kernel_workflow")" -eq 2 ] \
   && ! grep -Fq '"crates/system/tilo-crypto/**"' "$kernel_workflow" \
   && ! grep -Fq 'tls_fixture' "$kernel_workflow" \
   && grep -Fq 'COPY crates/system/tilo-crypto/src/lib.rs' "$repo/os/kernel/Containerfile" \
   && grep -Fq 'COPY crates/system/tilo-crypto/src/main.rs' "$repo/os/kernel/Containerfile" \
   && ! grep -Fq 'COPY crates/system/tilo-crypto/src /work/tilo-crypto/src' "$repo/os/kernel/Containerfile" \
   && grep -Fq 'cargo build --release --frozen --no-default-features' "$repo/os/kernel/Containerfile"; then
  printf 'ok    kernel CI retriggers for signer sources, not TLS fixtures\n'
else
  printf 'kernel CI does not isolate signer sources from TLS fixture inputs\n' >&2
  fail=1
fi

if grep -Fqx '        run: os/dev/require-cosign-authority.sh' "$kernel_workflow" \
   && grep -Fqx '    needs: signing-preflight' "$kernel_workflow" \
   && grep -Fq "(needs.signing-preflight.result == 'success' || needs.signing-preflight.result == 'skipped')" "$kernel_workflow"; then
  printf 'ok    release kernel compilation starts only after KMS identity preflight\n'
else
  printf 'release kernel compilation can start before its signing authority is identity-bound\n' >&2
  fail=1
fi

authority="$repo/os/dev/require-cosign-authority.sh"
if grep -Fq 'cosign public-key --key "$COSIGN_KEY_REF" > "$resolved"' "$authority" \
   && grep -Fq 'cmp --silent "$repo/keys/cosign.pub" "$resolved"' "$authority" \
   && grep -Fq '. "$repo/os/dev/configure-cosign-kms.sh"' "$authority" \
   && python3 "$repo/os/tests/test_require_cosign_authority.py" >/dev/null 2>&1; then
  printf 'ok    the shared signing preflight binds the KMS key to keys/cosign.pub\n'
else
  printf 'the shared signing preflight does not bind the KMS key to keys/cosign.pub\n' >&2
  fail=1
fi

relay_workflow="$workflows/release-relay.yml"
if grep -Fqx '        run: os/dev/require-cosign-authority.sh' "$relay_workflow" \
   && grep -Fq 'needs: [signing-preflight, ensure-rpms]' "$relay_workflow"; then
  printf 'ok    release relay build starts only after KMS identity preflight\n'
else
  printf 'release relay build can start before its signing authority is identity-bound\n' >&2
  fail=1
fi

if [ "$(grep -Fc '"os/tests/secureboot_vm_*"' "$kernel_workflow")" -eq 2 ] \
   && grep -Fq -- '--target secureboot-probe-inputs' "$kernel_workflow" \
   && grep -Fq 'bash os/tests/secureboot_vm_probe.sh' "$kernel_workflow" \
   && grep -Fq 'TILO_SECUREBOOT_VM_ARCH="${{ matrix.arch }}"' "$kernel_workflow" \
   && grep -Fq 'TILO_SECUREBOOT_VM_APP="$probe/secureboot_vm_app.efi"' "$kernel_workflow" \
   && grep -Fq 'ld.lld -flavor link /subsystem:efi_application' "$repo/os/kernel/Containerfile" \
   && grep -Fq 'FROM scratch AS secureboot-probe-inputs' "$repo/os/kernel/Containerfile" \
   && [ "$(grep '^FROM ' "$repo/os/kernel/Containerfile" | tail -1)" = 'FROM scratch AS publish' ]; then
  printf 'ok    kernel CI retriggers and runs OVMF enforcement against its exact build\n'
else
  printf 'kernel CI does not execute its OVMF Secure Boot enforcement probe\n' >&2
  fail=1
fi

# The refresh renders on the project's builder and records it with its
# candidates; the goldens check renders on the recorded one.
if grep -Fq 'bash tools/ci/golden-image.sh record "$image"' "$visual_baselines" \
  && grep -Eq '^registry\.tilo\.so/tilo/tilo-builder@sha256:[0-9a-f]{64}$' \
    "$repo/.config/visual-goldens-image" \
  && grep -Fqx "image = \"$(cat "$repo/.config/visual-goldens-image")\"" "$repo/t.toml" \
  && grep -Fq 'xargs -0 -r git add -N --' "$visual_baselines" \
  && grep -Fq 'sha256sum candidates.patch > candidates.patch.sha256' "$visual_baselines" \
  && ! grep -Fq 'git push' "$visual_baselines"; then
  printf 'ok    visual verification and reviewed candidates name one recorded builder\n'
else
  printf 'visual candidates must record the builder the nightly verifies on and never auto-commit pixels\n' >&2
  fail=1
fi

declare -A asset_names=()
while IFS=$'\t' read -r checksum name url extra; do
  case "$checksum" in ''|'#'*) continue ;; esac
  if [[ ! "$checksum" =~ ^[0-9a-f]{64}$ ]] \
    || [[ ! "$name" =~ ^[A-Za-z0-9][A-Za-z0-9._+-]*$ ]] \
    || [[ ! "$url" =~ ^https:// ]] \
    || [[ -n "$extra" ]] \
    || [[ -n "${asset_names[$name]:-}" ]]; then
    printf 'invalid or duplicate external-asset manifest row: %s\n' "$name" >&2
    fail=1
    continue
  fi
  asset_names[$name]=1
  case "$url" in
    https://raw.githubusercontent.com/*)
      [[ "$url" =~ /[0-9a-f]{40}/ ]] || {
        printf 'mutable raw GitHub URL: %s\n' "$url" >&2
        fail=1
      }
      ;;
    https://github.com/*/archive/*)
      [[ "$url" =~ /archive/[0-9a-f]{40}\.tar\.gz$ ]] || {
        printf 'mutable GitHub source archive URL: %s\n' "$url" >&2
        fail=1
      }
      ;;
    https://github.com/*)
      [[ "$url" == */releases/download/* ]] && [[ "$url" != */releases/download/latest/* ]] || {
        printf 'unversioned GitHub release URL: %s\n' "$url" >&2
        fail=1
      }
      ;;
  esac
done < "$manifest"
if [ "$fail" = 0 ]; then
  printf 'ok    external GitHub assets have safe names, immutable revisions, and SHA-256 pins\n'
fi

declare -A cosign_sha=(
  [amd64]=bcfeae05557a9f313ee4392d2f335d0ff69ebbfd232019e3736fb04999fe1734
  [arm64]=2cbcea1873ad76274c3f241ef175d204654e3aac3e73e6ec4504e5227015cb0a
)
for arch in amd64 arm64; do
  name="cosign-v2.5.2-linux-$arch"
  if ! awk -F '\t' -v n="$name" -v arch="$arch" -v sha="${cosign_sha[$arch]}" \
    '$1 == sha && $2 == n && $3 == "https://github.com/sigstore/cosign/releases/download/v2.5.2/cosign-linux-" arch { found=1 }
     END { exit !found }' "$manifest"; then
    printf 'missing checksum-pinned Cosign input: %s\n' "$name" >&2
    fail=1
  fi
done
if ! grep -Fq './os/dev/install-cached-cosign.sh' "$cosign_action" \
  || ! grep -Fq 'key: cosign-v2.5.2-${{ inputs.cache-architecture }}-' "$cosign_action" \
  || ! grep -Fq 'uses: actions/cache/restore@5a3ec84eff668545956fd18022155c47e93e2684' "$cosign_action" \
  || ! grep -Fq 'uses: actions/cache/save@5a3ec84eff668545956fd18022155c47e93e2684' "$cosign_action"; then
  printf 'shared Cosign action bypasses verified installation or persistent caching\n' >&2
  fail=1
fi
for workflow in "${cosign_workflows[@]}"; do
  if grep -Fq 'uses: ./.forgejo/actions/install-cosign' "$workflow"; then
    continue
  fi
  if ! grep -Fq './os/dev/install-cached-cosign.sh' "$workflow" \
    || ! grep -Fq 'github-assets-all-v1-' "$workflow"; then
    printf 'workflow bypasses cached, checksum-verified Cosign installation: %s\n' \
      "${workflow#$repo/}" >&2
    fail=1
  fi
done
if grep -R -q 'sigstore/cosign-installer' "$workflows"; then
  printf 'workflow still downloads Cosign through the installer action\n' >&2
  fail=1
fi
for workflow in "${release_signed_workflows[@]}"; do
  if grep -Eq -- 'COSIGN_PRIVATE_KEY|COSIGN_PASSWORD|--tlog-upload=false|--insecure-ignore-tlog' \
      "$workflow" \
     || ! grep -Fq '. os/dev/configure-cosign-kms.sh' "$workflow" \
     || ! grep -Eq 'cosign verify --key keys/cosign\.pub "\$(ref|image)"' "$workflow"; then
    printf 'release artifact bypasses KMS, transparency, or digest verification: %s\n' \
      "${workflow#$repo/}" >&2
    fail=1
  fi
done
if ! grep -Fq 'COSIGN_KEY_REF must name a non-exportable KMS/HSM key' "$repo/os/dev/configure-cosign-kms.sh"; then
  printf 'shared Cosign configuration does not refuse exportable signing keys\n' >&2
  fail=1
fi
if [ "$fail" = 0 ]; then
  printf 'ok    OS, kernel, RPM, and relay releases use KMS signing, transparency, and digest verification\n'
fi
builder_workflow="$workflows/build-ci-builder.yml"
if grep -Fqx '        run: os/dev/require-cosign-authority.sh' "$builder_workflow" \
   && grep -Fq 'needs: [plan, signing-preflight]' "$builder_workflow"; then
  printf 'ok    native builder work starts only after KMS authority matches the pinned public key\n'
else
  printf 'native builder can start before its signing authority is identity-bound\n' >&2
  fail=1
fi
if ! grep -Fq 'cache-github-assets.sh" "$asset"' \
    "$repo/os/dev/install-cached-cosign.sh"; then
  printf 'Cosign installer bypasses the checksum-verified persistent cache\n' >&2
  fail=1
elif [ "$fail" = 0 ]; then
  printf 'ok    every Cosign workflow uses cached, checksum-pinned native binaries\n'
fi

declare -A syft_sha=(
  [amd64]=2a2e837a2c8d59ec9af5472ee22d3b04ee463c4e44476ecf993fd1e5ab6ebc7f
  [arm64]=6c0466811541ea03add5213a60a1562f0851e4c0b0ecfdee1a694a9455285900
)
for arch in amd64 arm64; do
  name="syft-1.51.0-linux-$arch.tar.gz"
  if ! awk -F '\t' -v n="$name" -v arch="$arch" -v sha="${syft_sha[$arch]}" \
    '$1 == sha && $2 == n && $3 == "https://github.com/anchore/syft/releases/download/v1.51.0/syft_1.51.0_linux_" arch ".tar.gz" { found=1 }
     END { exit !found }' "$manifest"; then
    printf 'missing checksum-pinned Syft input: %s\n' "$name" >&2
    fail=1
  fi
done
if grep -Rq 'anchore/sbom-action' "$workflows"; then
  printf 'workflow still depends on a GitHub-only SBOM action\n' >&2
  fail=1
fi
if ! grep -Fq './os/dev/install-cached-syft.sh' "$syft_workflow" \
  || ! grep -Fq 'spdx-json=sbom.spdx.json' "$syft_workflow" \
  || ! grep -Fq 'cache-github-assets.sh" "$asset"' \
    "$repo/os/dev/install-cached-syft.sh"; then
  printf 'image SBOM bypasses the checksum-verified Syft cache\n' >&2
  fail=1
elif [ "$fail" = 0 ]; then
  printf 'ok    image SBOM uses a cached, checksum-pinned native Syft binary\n'
fi

asset_action="$repo/.forgejo/actions/cache-github-assets/action.yml"
if ! grep -Fqx '          ${{ inputs.key }}-' "$asset_action" \
  || ! grep -Fq 'run: ./os/dev/cache-github-assets.sh $ASSETS' "$asset_action" \
  || [ "$(grep -Fc "key: \${{ inputs.key }}-\${{ hashFiles('os/external-assets/github-assets.tsv') }}" "$asset_action")" -ne 2 ]; then
  printf 'shared asset cache action does not restore prior generations of its exact manifest key\n' >&2
  fail=1
fi
for workflow in "${github_cache_workflows[@]}"; do
  # Either the shared action keyed on the whole-manifest cache, or an inline
  # restore that also falls back to its earlier generations.
  if grep -Fq 'uses: ./.forgejo/actions/cache-github-assets' "$workflow" \
    && grep -Fqx '          key: github-assets-all-v1' "$workflow"; then
    continue
  fi
  if ! grep -Fq 'github-assets-all-v1-' "$workflow" \
    || ! grep -Fqx '            github-assets-all-v1-' "$workflow"; then
    printf 'workflow does not reuse prior verified GitHub cache generations: %s\n' \
      "${workflow#$repo/}" >&2
    fail=1
  fi
done
if ! grep -Fq "printf '[tilo] pruning stale cached asset %s" \
  "$repo/os/dev/cache-github-assets.sh"; then
  printf 'GitHub cache cannot reconcile a prior manifest generation\n' >&2
  fail=1
fi
if ! grep -Fq 'chcon -R -t container_file_t -l s0 -- "$cache_dir"' \
  "$repo/os/dev/cache-github-assets.sh"; then
  printf 'GitHub cache does not clear private container MCS categories before reuse\n' >&2
  fail=1
fi
if [ "$fail" = 0 ]; then
  printf 'ok    GitHub inputs reuse prior manifest cache generations with shared SELinux labels\n'
fi

for target in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
  name="sccache-v0.17.0-$target.tar.gz"
  if ! awk -F '\t' -v n="$name" \
    '$1 !~ /^#/ && $2 == n && $3 ~ /^https:\/\/github\.com\// { found=1 }
     END { exit !found }' "$manifest"; then
    printf 'missing checksum-pinned GitHub cache entry: %s\n' "$name" >&2
    fail=1
  fi
done
if ! grep -Fq 'uses: ./.forgejo/actions/cache-github-assets' "$package_workflow" \
  || ! grep -Fq 'key: desktop-external-inputs-${{ matrix.arch }}' "$package_workflow" \
  || ! grep -Fq 'github-assets-all-v1-${{ hashFiles('\''os/external-assets/github-assets.tsv'\'') }}' \
    "$package_workflow" \
  || ! grep -Fq "assets: sccache-v0.17.0-\${{ matrix.arch == 'arm64' && 'aarch64' || 'x86_64' }}-unknown-linux-musl.tar.gz" \
    "$package_workflow" \
  || ! grep -Fq 'cached_archive="$repo_root/os/external-assets/cache/${name}.tar.gz"' \
    "$repo/dist/ci/install-sccache.sh"; then
  printf 'desktop package CI bypasses the verified GitHub input cache\n' >&2
  fail=1
else
  printf 'ok    desktop package CI restores checksum-pinned GitHub inputs\n'
fi

if grep -Fq 'ENV GOPROXY=https://proxy.golang.org GOSUMDB=sum.golang.org' "$containerfile" \
  && grep -Fq -- '--mount=type=cache,target=/go/pkg/mod' "$containerfile" \
  && grep -Fq -- '--mount=type=cache,target=/root/.cache/go-build' "$containerfile"; then
  printf 'ok    Go helper builds use verified persistent proxy caches without GitHub fallback\n'
else
  printf 'Go helper builds can bypass the verified module proxy or redownload unchanged modules\n' >&2
  fail=1
fi

# Dev images carry sshd, a toolchain and an unsealed /usr. Signing one with the
# OS release authority would let a production machine accept it as an update,
# so the dev workflow only verifies the base against keys/cosign.pub.
if grep -Eq 'COSIGN_KEY_REF|AWS_SIGNING_ROLE_ARN|cosign (sign|attest)' "$dev_disk_workflow"; then
  printf 'dev disk workflow can sign with the OS release authority\n' >&2
  fail=1
elif ! grep -Fq 'cosign verify --key keys/cosign.pub "$child"' "$dev_disk_workflow" \
  || ! grep -Fq '"git+ssh://code.tilo.so/tilo/tilo@" + $source' "$dev_disk_workflow"; then
  printf 'dev disk workflow builds on a base it has not bound to the release key and source\n' >&2
  fail=1
else
  printf 'ok    dev disks build only on a verified same-source base and are never release-signed\n'
fi
exit "$fail"
