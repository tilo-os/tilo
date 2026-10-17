#!/usr/bin/env bash
# The publishing half of an image: push OUT/image.oci.tar (one image, or an index of them from
# oci-index.py) to REPOSITORY with its digest unchanged, attach the signature oci-sign.sh made if
# there is one, and point TAG... at it.
# Writes OUT/receipt.json, the digest receipt deployments pin. Logs in to the registry as `oidc`
# with an identity token from t for the registry (`id_token = ["registry.tilo.so"]`), which the
# registry accepts for this repository's main, stable and v* tags; never signing credentials.
#
# usage: oci-publish.sh dev|release|any REPOSITORY OUT TAG...
# The first argument is the channel the check publishes to; it must be this run's (lib.sh), so a
# release check can never publish main's tree and a development check never stable's. With
# `any` the check publishes on both, and each TAG says its channel: `dev:dev-{commit}`,
# `release:sha-{commit}`; only the run's channel's tags are pushed, so one check, with its
# signature as an optional need, pushes development tags from main and the signed release from
# stable.
# Tags may contain {commit}, replaced by T_COMMIT, on a release tag v<version> {version}, and
# {rust}, the toolchain channel rust-toolchain.toml pins (the CI builder's), and {fingerprint},
# OUT/fingerprint (the RPM snapshot's package-set fingerprint).
#
# With TILO_RELEASE_OUT=DIR a release publishes the archive, digest and signature in DIR instead
# of OUT's: a product whose release is derived from its build (products.toml's `release_build`)
# publishes that image from stable and the build's from main. The receipt goes to OUT either way,
# where the check's pointer reads it. A receipt-extra.json beside the archive, one JSON object on
# one line, adds its members to the receipt (the web entry: the image it replaces, the version
# it carries).
#
# With TILO_PUBLISH_DRY_RUN=DIR nothing reaches a registry: the image goes to the OCI layout
# DIR/REPOSITORY instead, without logging in, a signature is verified against the key the
# dry-run signer wrote (OUT/dry-run.pub) or keys/cosign.pub, and the receipt is written as for
# a real publication. T_REF and T_COMMIT say what the run pretends to publish for.
#
# An OUT/attestation.json beside the signature (oci-attest.sh: signed SLSA provenance) is
# attached too, and verified on the published image as its consumers verify it.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
wanted=${1:?usage: oci-publish.sh dev|release|any REPOSITORY OUT TAG...}
repository=${2:?}
out=${3:?}
shift 3
run_channel=$(channel)
case $wanted in
  dev | release)
    [[ $run_channel == "$wanted" ]] || die "this check publishes to $wanted; the run is for ${T_REF:-}"
    for tag; do
      [[ $tag != dev:* && $tag != release:* ]] || die "a $wanted check's tags name no channel: $tag"
    done ;;
  any)
    tags=()
    for tag; do
      case $tag in
        "$run_channel":*) tags+=("${tag#*:}") ;;
        dev:* | release:*) ;;
        *) die "with any, each tag names its channel (dev:TAG or release:TAG), not $tag" ;;
      esac
    done
    (($#)) || die 'no tags'
    ((${#tags[@]})) || die "no tags for the $run_channel channel"
    set -- "${tags[@]}" ;;
  *) die "unknown channel $wanted" ;;
esac
receipt=$out/receipt.json
if [[ $run_channel == release && -n ${TILO_RELEASE_OUT:-} ]]; then
  out=$TILO_RELEASE_OUT
  [[ -s $out/image.oci.tar ]] || die "no release image in $out: the step deriving it did not run"
fi
[[ $run_channel == dev || -s $out/signature ]] || die 'a release is published signed only'
(($#)) || die 'no tags'
require_commit
# Releases verify against the release key; a signed development candidate against the
# development key (oci-sign.sh, TILO_DEV_SIGNING).
verify_key=$repo_root/keys/cosign.pub
[[ $run_channel == dev ]] && verify_key=$repo_root/keys/cosign-dev.pub
digest=$(cat "$out/digest")
[[ $(archive_digest "$out/image.oci.tar") == "$digest" ]] || die 'the archive is not the build'
registry=${repository%%/*}
auth=$scratch/registry-auth.json
# Where the image goes: the registry, or with TILO_PUBLISH_DRY_RUN a local OCI layout.
# `pushed` is the image by digest, or in the layout (which names images by tag only) the first tag.
target() {
  if [[ -n ${TILO_PUBLISH_DRY_RUN:-} ]]; then
    printf 'oci:%s/%s:%s' "$TILO_PUBLISH_DRY_RUN" "$repository" "$1"
  else
    printf 'docker://%s:%s' "$repository" "$1"
  fi
}
pushed() {
  if [[ -n ${TILO_PUBLISH_DRY_RUN:-} ]]; then target "$first"; else printf 'docker://%s@%s' "$repository" "$digest"; fi
}
if [[ -n ${TILO_PUBLISH_DRY_RUN:-} ]]; then
  mkdir -p "$TILO_PUBLISH_DRY_RUN/$repository"
  printf '{}' > "$auth"
else
  export REGISTRY_AUTH_FILE=$auth DOCKER_CONFIG=$scratch/docker
  mkdir -p "$DOCKER_CONFIG"
fi
# Log in with a fresh identity token. It is the registry password and lasts minutes, so a push
# that outlives it (a multi-GB image) fails to authenticate its next blob; copy() logs in again
# and retries, and the blobs already pushed are not sent twice.
login() {
  [[ -n ${TILO_PUBLISH_DRY_RUN:-} ]] && return
  local password
  password=$(id_token "$registry")
  printf '%s' "$password" | skopeo login --authfile "$auth" -u oidc --password-stdin "$registry" >/dev/null
  cp "$auth" "$DOCKER_CONFIG/config.json"
}
copy() {
  local attempt
  for attempt in 1 2 3 4 5; do
    login
    skopeo copy --all --preserve-digests --retry-times 3 --authfile "$auth" "$@" && return
    printf 'publish: copy attempt %s failed; logging in again\n' "$attempt" >&2
  done
  die "could not copy $1 to $2"
}
login
# A tag with its placeholders filled in.
fill() {
  local tag=${1//\{commit\}/$T_COMMIT}
  if [[ $tag == *'{version}'* ]]; then
    [[ ${T_REF:-} =~ ^tag:v([0-9][0-9A-Za-z.+-]*)$ ]] || die "{version} needs a v<version> tag, not ${T_REF:-}"
    tag=${tag//\{version\}/${BASH_REMATCH[1]}}
  fi
  if [[ $tag == *'{rust}'* ]]; then
    local rust
    rust=$(sed -n 's/^channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$repo_root/rust-toolchain.toml")
    [[ -n $rust ]] || die 'no channel in rust-toolchain.toml for {rust}'
    tag=${tag//\{rust\}/$rust}
  fi
  if [[ $tag == *'{fingerprint}'* ]]; then
    local fingerprint
    fingerprint=$(cat "$out/fingerprint" 2>/dev/null) || die "{fingerprint} needs $out/fingerprint"
    [[ $fingerprint =~ ^[0-9a-f]{64}$ ]] || die "not a fingerprint: $fingerprint"
    tag=${tag//\{fingerprint\}/$fingerprint}
  fi
  printf '%s' "$tag"
}
# A tag written `once:TAG` never moves: where TAG exists already, nothing is published and the
# receipt names the image TAG holds. The RPM snapshot is published once per fingerprint, and a
# rebuild of the same package set would otherwise replace what earlier OS builds verified.
tags=()
for tag; do
  if [[ $tag == once:* ]]; then
    tag=$(fill "${tag#once:}")
    if skopeo inspect --raw --authfile "$auth" "$(target "$tag")" > "$scratch/held-manifest" 2>/dev/null; then
      held=sha256:$(sha256sum < "$scratch/held-manifest" | cut -d' ' -f1)
      printf '{"schemaVersion":3,"repository":"%s","digest":"%s","image":"%s@%s","tags":["%s"],"sourceRevision":"%s","reference":"%s","signed":false,"existing":true}\n' \
        "$repository" "$held" "$repository" "$held" "$tag" "$T_COMMIT" "$T_REF" > "$out/receipt.json"
      printf '%s:%s is published already, as %s@%s; publishing nothing\n' "$repository" "$tag" "$repository" "$held"
      exit 0
    fi
  fi
  tags+=("$tag")
done
set -- "${tags[@]}"
first=$(fill "$1")
copy "oci-archive:$out/image.oci.tar" "$(target "$first")"
pushed=$(skopeo inspect --raw --authfile "$auth" "$(pushed)" | sha256sum | cut -d' ' -f1)
[[ sha256:$pushed == "$digest" ]] || die "the registry holds sha256:$pushed, not $digest"
if [[ -s $out/signature && -n ${TILO_PUBLISH_DRY_RUN:-} ]]; then
  # No registry to attach to: check the signature is over this image, by the expected key.
  use_cosign
  key=$verify_key
  [[ -s $out/dry-run.pub ]] && key=$out/dry-run.pub
  [[ $(cat "$out/payload.json") == "$(signing_payload "$repository" "$digest")" ]] \
    || die 'the signature is not over this repository and digest'
  cosign verify-blob --insecure-ignore-tlog=true --key "$key" --signature "$out/signature" "$out/payload.json"
elif [[ -s $out/signature ]]; then
  use_cosign
  login
  if [[ -s $out/bundle.json ]]; then
    # Signed with a transparency log entry (oci-sign.sh, TILO_SIGN_TLOG=1): attach it, and
    # verify the way release builds do, requiring it.
    cosign attach signature --payload "$out/payload.json" --signature "$out/signature" \
      --rekor-response "$out/bundle.json" "$repository@$digest"
    cosign verify --key "$verify_key" "$repository@$digest" >/dev/null
    # The certified signatures oci-sign.sh made for a repository installed systems verify
    # (lib.sh device_verified): each verified as their container policy does, through the
    # certificate to the release root, for the purpose and epoch it was issued for.
    if [[ -d $out/certified ]]; then
      . "$repo_root/keys/release/trust.env"
      for sig in "$out"/certified/*.sig; do
        n=${sig%.sig}
        cosign attach signature --payload "$out/payload.json" --signature "$sig" \
          --certificate "$n.crt" --certificate-chain "$repo_root/keys/release/root.crt" \
          --rekor-response "$n.bundle" "$repository@$digest"
        cosign verify --certificate-chain "$repo_root/keys/release/root.crt" \
          --certificate-identity "os-images@$TILO_RELEASE_IDENTITY_DOMAIN" \
          --certificate-oidc-issuer "$TILO_RELEASE_ISSUER_PREFIX$(cat "$n.epoch")" \
          --insecure-ignore-sct "$repository@$digest" >/dev/null
      done
    fi
  else
    cosign attach signature --payload "$out/payload.json" --signature "$out/signature" "$repository@$digest"
    cosign verify --insecure-ignore-tlog=true --key "$verify_key" "$repository@$digest" >/dev/null
  fi
fi
if [[ -s $out/attestation.json ]]; then
  # The provenance oci-attest.sh signed: its statement must be about this image, and it is
  # attached so `cosign verify-attestation` finds it on the published digest.
  [[ -s $out/signature ]] || die 'an attestation is published only with its signature'
  use_cosign
  statement=$(sed -n 's/.*"payload":"\([A-Za-z0-9+\/=]*\)".*/\1/p' "$out/attestation.json" | base64 -d)
  [[ $statement == *"\"sha256\":\"${digest#sha256:}\""* && $statement == *"\"expectedReleaseDigest\":\"$digest\""* ]] \
    || die 'the attestation is not about this image'
  if [[ -n ${TILO_PUBLISH_DRY_RUN:-} ]]; then
    key=$verify_key
    [[ -s $out/dry-run.pub ]] && key=$out/dry-run.pub
    cosign verify-blob-attestation --insecure-ignore-tlog=true --check-claims=false --key "$key" \
      --type slsaprovenance1 --signature "$out/attestation.json"
  else
    cosign attach attestation --attestation "$out/attestation.json" "$repository@$digest"
    # Entered in the transparency log (oci-attest.sh kept the entry): verified requiring it.
    tlog=(--insecure-ignore-tlog=true)
    [[ -s $out/attestation.bundle.json ]] && tlog=()
    cosign verify-attestation --key "$verify_key" --type slsaprovenance1 "${tlog[@]}" \
      "$repository@$digest" >/dev/null
  fi
fi
tags="\"$first\""
for tag in "${@:2}"; do
  tag=$(fill "$tag")
  copy "$(pushed)" "$(target "$tag")"
  tags+=",\"$tag\""
done
signed=false
[[ -s $out/signature ]] && signed=true
extra=
if [[ -s $out/receipt-extra.json ]]; then
  extra=$(cat "$out/receipt-extra.json")
  [[ $extra != *$'\n'* && $extra =~ ^\{(\"[A-Za-z]+\":.+)\}$ ]] || die "receipt-extra.json is not one JSON object on one line: $extra"
  extra=,${BASH_REMATCH[1]}
fi
printf '{"schemaVersion":3,"repository":"%s","digest":"%s","image":"%s@%s","tags":[%s],"sourceRevision":"%s","reference":"%s","signed":%s%s}\n' \
  "$repository" "$digest" "$repository" "$digest" "$tags" "$T_COMMIT" "$T_REF" "$signed" "$extra" > "$receipt"
printf 'published %s@%s\n' "$repository" "$digest"
