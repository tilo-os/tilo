#!/usr/bin/env bash
# Every remote workflow action is executable third-party code. Require an exact
# commit rather than a mutable tag or branch across the entire workflow set.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflows="$repo/.forgejo/workflows"
fail=0

manifest="$repo/os/external-assets/github-assets.tsv"
package_workflow="$workflows/release-desktop.yml"
containerfile="$repo/os/Containerfile"
visual_baselines="$repo/tools/ci/visual-baselines.sh"
# t builds, signs and publishes the kernel, the RPM snapshot, the kernel buildroot, the relay,
# the env-runtime, the legacy guest and the CI builder (t.toml, generated from
# tools/ci/products.toml); these scripts are what its checks run.
t_toml="$repo/t.toml"
publish="$repo/tools/ci/publish"
kernel_script="$publish/kernel.sh"
github_cache_workflows=(
  "$workflows/release-desktop.yml"
  "$workflows/release-imager.yml"
)
cosign_workflows=(
  "$workflows/release-imager.yml"
)
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

if python3 "$repo/tools/policy/check_workflow_actions.py"; then
  printf 'ok    every remote workflow action is pinned to a full commit SHA\n'
else
  fail=1
fi

# The kernel is rebuilt for the module signer's own sources, not for the TLS fixtures beside
# them: its fingerprint (kernel.sh inputs_digest) and its build check's inputs name exactly them.
kernel_inputs="$(tcheck image-kernel-{arch} inputs)" || fail=1
kernel_digest="$(sed -n '/^inputs_digest() {/,/^}/p' "$kernel_script")"
if [ -n "$kernel_digest" ] \
   && grep -Fq 'crates/system/tilo-crypto/Cargo.toml' <<<"$kernel_digest" \
   && grep -Fq 'crates/system/tilo-crypto/src/lib.rs' <<<"$kernel_digest" \
   && grep -Fq 'crates/system/tilo-crypto/src/main.rs' <<<"$kernel_digest" \
   && ! grep -Eq 'tilo-crypto( |/\*|$)|tls_fixture' <<<"$kernel_digest" \
   && grep -Fxq 'crates/system/tilo-crypto/Cargo.toml' <<<"$kernel_inputs" \
   && grep -Fxq 'crates/system/tilo-crypto/src/lib.rs' <<<"$kernel_inputs" \
   && grep -Fxq 'crates/system/tilo-crypto/src/main.rs' <<<"$kernel_inputs" \
   && ! grep -Eq '^crates/system/tilo-crypto/(\*\*|src/?\*?\*?)$|tls_fixture' <<<"$kernel_inputs" \
   && grep -Fq 'COPY crates/system/tilo-crypto/src/lib.rs' "$repo/os/kernel/Containerfile" \
   && grep -Fq 'COPY crates/system/tilo-crypto/src/main.rs' "$repo/os/kernel/Containerfile" \
   && ! grep -Fq 'COPY crates/system/tilo-crypto/src /work/tilo-crypto/src' "$repo/os/kernel/Containerfile" \
   && grep -Fq 'cargo build --release --frozen --no-default-features' "$repo/os/kernel/Containerfile"; then
  printf 'ok    kernel CI retriggers for signer sources, not TLS fixtures\n'
else
  printf 'kernel CI does not isolate signer sources from TLS fixture inputs\n' >&2
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

# On t the signing checks hold the KMS key and nothing else, and bind it to keys/cosign.pub by
# verifying every signature they make against it before handing it on (oci-sign.sh), so no
# build waits on a separate preflight.
if grep -Fq 'public_key=$repo_root/keys/cosign.pub' "$publish/oci-sign.sh" \
   && grep -Fq '. "$repo_root/os/dev/configure-cosign-kms.sh"' "$publish/oci-sign.sh" \
   && grep -Fq 'cosign verify-blob --key "$public_key" --bundle "$out/bundle.json" "$out/payload.json"' "$publish/oci-sign.sh" \
   && grep -Fq 'cosign verify-blob --insecure-ignore-tlog=true --key "$public_key" \' "$publish/oci-sign.sh"; then
  printf 'ok    release signing verifies every KMS signature against keys/cosign.pub\n'
else
  printf 'release signing does not bind its KMS key to keys/cosign.pub\n' >&2
  fail=1
fi

# The kernel's acceptance (module signatures and OVMF Secure Boot enforcement, against the
# exact build) passes before it is signed or published, and reruns when the probe changes.
acceptance="$publish/kernel-acceptance.sh"
acceptance_inputs="$(tcheck kernel-acceptance-{arch} inputs)" || fail=1
sign_kernel_needs="$(tcheck sign-kernel needs)" || fail=1
publish_kernel_needs="$(tcheck publish-kernel needs)" || fail=1
if grep -Fxq 'os/tests/secureboot_vm_*' <<<"$acceptance_inputs" \
   && grep -Fxq 'image-kernel-{arch}' <<<"$(tcheck kernel-acceptance-{arch} needs)" \
   && grep -Fxq 'bash tools/ci/publish/kernel-acceptance.sh {arch} /target/publish/kernel/{arch}' <<<"$(tcheck kernel-acceptance-{arch} run)" \
   && grep -Fxq 'kernel-acceptance-x86_64' <<<"$sign_kernel_needs" \
   && grep -Fxq 'kernel-acceptance-aarch64' <<<"$sign_kernel_needs" \
   && grep -Fxq 'kernel-acceptance-x86_64' <<<"$publish_kernel_needs" \
   && grep -Fxq 'kernel-acceptance-aarch64' <<<"$publish_kernel_needs" \
   && grep -Fq -- '--target secureboot-probe-inputs' "$kernel_script" \
   && grep -Fq 'bash os/tests/secureboot_vm_probe.sh' "$acceptance" \
   && grep -Fq 'TILO_SECUREBOOT_VM_ARCH="$arch"' "$acceptance" \
   && grep -Fq 'TILO_SECUREBOOT_VM_APP="$probe/secureboot_vm_app.efi"' "$acceptance" \
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
for workflow in "${cosign_workflows[@]}"; do
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
if ! grep -Fq 'COSIGN_KEY_REF must name a non-exportable KMS/HSM key' "$repo/os/dev/configure-cosign-kms.sh"; then
  printf 'shared Cosign configuration does not refuse exportable signing keys\n' >&2
  fail=1
fi
if [ "$fail" = 0 ]; then
  printf 'ok    OS, kernel, RPM, and relay releases use KMS signing, transparency, and digest verification\n'
fi
# t's release signing checks: the release key in KMS through the check's own identity token,
# only on release references, through oci-sign.sh; the images release builds verify with a
# plain `cosign verify --key keys/cosign.pub` (all but the CI builder) also enter the public
# transparency log. Publication verifies against keys/cosign.pub again and refuses any digest
# but the build's.
for check in sign-kernel sign-rpms sign-relay sign-kernel-buildroot sign-env-runtime \
    sign-legacy-guest sign-ci-builder; do
  env="$(tcheck "$check" env)" && on="$(tcheck "$check" on)" \
    && id_token="$(tcheck "$check" id_token)" && run="$(tcheck "$check" run)" || { fail=1; continue; }
  if ! grep -q '^COSIGN_KEY_REF=awskms:///' <<<"$env" \
     || grep -q '^COSIGN_PRIVATE_KEY\|^COSIGN_PASSWORD' <<<"$env" \
     || ! grep -Fxq 'sts.amazonaws.com' <<<"$id_token" \
     || [ -z "$on" ] || grep -Eqx 'main|dispatch' <<<"$on" \
     || ! grep -q '^bash tools/ci/publish/oci-sign.sh ' <<<"$run" \
     || { [ "$check" != sign-ci-builder ] && ! grep -Fxq 'TILO_SIGN_TLOG=1' <<<"$env"; }; then
    printf 'release artifact bypasses KMS, transparency, or release-only signing: t.toml#%s\n' "$check" >&2
    fail=1
  fi
done
if ! grep -Fq 'verify_key=$repo_root/keys/cosign.pub' "$publish/oci-publish.sh" \
   || ! grep -Fq 'cosign verify --key "$verify_key" "$repository@$digest"' "$publish/oci-publish.sh" \
   || ! grep -Fq '[[ sha256:$pushed == "$digest" ]] || die' "$publish/oci-publish.sh"; then
  printf 'publication does not verify the pushed digest and its signature against keys/cosign.pub\n' >&2
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
if ! grep -Fq 'cache-github-assets.sh" "$asset"' \
    "$repo/os/dev/install-cached-syft.sh"; then
  printf 'the Syft installer bypasses the checksum-verified cache\n' >&2
  fail=1
elif [ "$fail" = 0 ]; then
  printf 'ok    the Syft installer uses a cached, checksum-pinned native binary\n'
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

exit "$fail"
