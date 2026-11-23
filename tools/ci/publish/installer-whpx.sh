#!/usr/bin/env bash
# The WHPX installer medium: the qcow2 a Windows on ARM host opens directly, with the source
# record that names its bytes and the disk that passed the installation cycle.
#
#   installer-whpx.sh MEDIUM_DIR PROFILE LIVE_BASE PROOF_JSON OUT
#
# MEDIUM_DIR is tools/ci/publish/installer-whpx-medium.sh's output for an offline target
# (disk.qcow2, disk.qcow2.hardware.json and target.json, which names the verified target the
# medium installs). PROFILE is that target's hardware profile, LIVE_BASE the published
# multi-architecture index the medium was made from, and PROOF_JSON the guided-alongside cycle's
# guest-install-proof.json for that same media.
#
# OUT receives
#   tilo-installer-PROFILE-aarch64.qcow2.zst and .sha256   the compressed medium
#   tilo-installer-PROFILE-aarch64.qcow2.sha256            the qcow2 it expands to
#   tilo-installer-PROFILE-aarch64.whpx-source.json        schema v2 source description
#   artifacts.sha256                                       every file above
#
# With COSIGN_KEY_REF naming the release key in KMS, the source description is signed here and
# the signature verified offline against the public key; nothing here publishes. Windows has no qcow2 tooling, so the host downloads this one file and writes it
# unchanged: the archive expands to exactly the disk the cycle proved, and both digests are
# written down so that is checkable without the qcow2 tooling that made it.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

medium=${1:?usage: installer-whpx.sh MEDIUM_DIR PROFILE LIVE_BASE PROOF_JSON OUT}
profile=${2:?}
live_base=${3:?}
proof=${4:?}
out=${5:?}
[[ $profile =~ ^[a-z0-9][a-z0-9-]*$ ]] || die "unsupported hardware profile: $profile"
[[ $live_base =~ ^registry\.tilo\.so/tilo/tilo@sha256:[0-9a-f]{64}$ ]] || die "LIVE_BASE must be the published multi-architecture index"
[[ -f $medium/disk.qcow2 ]] || die "$medium holds no installer medium"
[[ -f $medium/disk.qcow2.hardware.json ]] || die "$medium carries no media provenance"
[[ -f $medium/target.json ]] || die "$medium names no preloaded target"
[[ -s $proof ]] || die "the installation cycle left no guest install proof: $proof"
[[ $(jq -er '.architecture' "$medium/target.json") == aarch64 ]] \
  || die "a WHPX medium is the aarch64 install medium"
[[ $(jq -er '.hardware_profile' "$medium/target.json") == "$profile" ]] \
  || die "the medium preloads $profile's target, not the one asked for"
target="$(jq -er '.repository' "$medium/target.json")@$(jq -er '.target_digest' "$medium/target.json")" \
  || die "the medium's target metadata names no image"

rm -rf "$out"
mkdir -p "$out"
stem="tilo-installer-$profile-aarch64"
archive="$out/$stem.qcow2.zst"
qcow=$medium/disk.qcow2

# -1 is the level the host downloads: a sparse stream of an 8 GiB disk at -19 spends minutes of
# every run compressing a temporary, while the raw payload a release ships compresses to a
# different artifact the Windows host never reads.
zstd --threads=0 -1 --sparse -f "$qcow" -o "$archive"
(cd "$out" && sha256sum "$(basename "$archive")") > "$archive.sha256"
zstd --decompress --stdout -- "$archive" | sha256sum | awk '{print $1}' > "$out/$stem.qcow2.sha256"
[[ $(cat "$out/$stem.qcow2.sha256") == "$(sha256sum "$qcow" | awk '{print $1}')" ]] \
  || die "the archive does not expand to the tested qcow2"

jq -n \
  --arg schema tilo.whpx-ci-installer-medium.v2 \
  --arg profile "$profile" \
  --arg target "$target" \
  --arg live_base "$live_base" \
  --arg build_revision "$T_COMMIT" \
  --arg os_source_revision "${TILO_EXPECTED_OS_SOURCE_REVISION:-$T_COMMIT}" \
  --arg run_id "${T_RUN_ID:-t-${T_COMMIT:0:12}}" \
  --arg run_attempt "${T_RUN_ATTEMPT:-1}" \
  --arg qcow2_sha256 "$(cat "$out/$stem.qcow2.sha256")" \
  --arg archive_sha256 "$(awk '{print $1}' "$archive.sha256")" \
  --arg guest_proof_sha256 "$(sha256sum "$proof" | awk '{print $1}')" \
  '{schema:$schema,target_profile:$profile,target_image:$target,live_base_image:$live_base,
    build_revision:$build_revision,os_source_revision:$os_source_revision,
    run_id:$run_id,run_attempt:$run_attempt,
    qcow2_sha256:$qcow2_sha256,archive_sha256:$archive_sha256,
    guest_proof_sha256:$guest_proof_sha256}' > "$out/$stem.whpx-source.json"

# The identity the Windows host verifies is signed by the release key in KMS, which never leaves
# it: the host checks this signature offline against the public key before it writes a disk.
if [ -n "${COSIGN_KEY_REF:-}" ]; then
  tool_dir="$(mktemp -d "${TMPDIR:-/var/tmp}/tilo-whpx-sign.XXXXXX")"
  trap 'rm -rf "$tool_dir"' EXIT
  RUNNER_TEMP=$tool_dir GITHUB_PATH=$tool_dir/github-path \
    TILO_GITHUB_ASSET_CACHE=/target/signing-tools TILO_ASSET_OFFLINE=1 \
    bash os/dev/install-cached-cosign.sh >/dev/null
  export PATH="$(cat "$tool_dir/github-path"):$PATH"
  . os/dev/configure-cosign-kms.sh
  identity="$out/$stem.whpx-source.json"
  cosign sign-blob --yes --key "$COSIGN_KEY_REF" \
    --bundle "$identity.bundle" --output-signature "$identity.sig" "$identity"
  cosign verify-blob --offline --key "$repo_root/keys/cosign.pub" \
    --bundle "$identity.bundle" --signature "$identity.sig" "$identity"
  rm -rf "$tool_dir"
  trap - EXIT
fi

(cd "$out" && find . -type f ! -name artifacts.sha256 -printf '%P\n' | LC_ALL=C sort \
  | while IFS= read -r file; do sha256sum "$file"; done) > "$out/artifacts.sha256"
(cd "$out" && sha256sum --check --strict artifacts.sha256 >/dev/null) \
  || die "the artifacts do not match their own checksums"
printf 'packaged the %s WHPX medium from %s\n' "$profile" "$target"
