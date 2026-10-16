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
# Tags may contain {commit}, replaced by T_COMMIT, and on a release tag v<version> {version}.
#
# With TILO_PUBLISH_DRY_RUN=DIR nothing reaches a registry: the image goes to the OCI layout
# DIR/REPOSITORY instead, without logging in, a signature is verified against the key the
# dry-run signer wrote (OUT/dry-run.pub) or keys/cosign.pub, and the receipt is written as for
# a real publication. T_REF and T_COMMIT say what the run pretends to publish for.
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
[[ $run_channel == dev || -s $out/signature ]] || die 'a release is published signed only'
(($#)) || die 'no tags'
require_commit
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
  printf '%s' "$tag"
}
first=$(fill "$1")
copy "oci-archive:$out/image.oci.tar" "$(target "$first")"
pushed=$(skopeo inspect --raw --authfile "$auth" "$(pushed)" | sha256sum | cut -d' ' -f1)
[[ sha256:$pushed == "$digest" ]] || die "the registry holds sha256:$pushed, not $digest"
if [[ -s $out/signature && -n ${TILO_PUBLISH_DRY_RUN:-} ]]; then
  # No registry to attach to: check the signature is over this image, by the expected key.
  use_cosign
  key=$repo_root/keys/cosign.pub
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
    cosign verify --key "$repo_root/keys/cosign.pub" "$repository@$digest" >/dev/null
  else
    cosign attach signature --payload "$out/payload.json" --signature "$out/signature" "$repository@$digest"
    cosign verify --insecure-ignore-tlog=true --key "$repo_root/keys/cosign.pub" "$repository@$digest" >/dev/null
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
printf '{"schemaVersion":3,"repository":"%s","digest":"%s","image":"%s@%s","tags":[%s],"sourceRevision":"%s","reference":"%s","signed":%s}\n' \
  "$repository" "$digest" "$repository" "$digest" "$tags" "$T_COMMIT" "$T_REF" "$signed" > "$out/receipt.json"
printf 'published %s@%s\n' "$repository" "$digest"
