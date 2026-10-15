#!/usr/bin/env bash
# The publishing half of an image: push OUT/image.oci.tar (one image, or an index of them from
# oci-index.py) to REPOSITORY with its digest unchanged, attach the signature oci-sign.sh made if
# there is one, and point TAG... at it.
# Writes OUT/receipt.json, the digest receipt deployments pin. Logs in to the registry as `oidc`
# with an identity token from t for the registry (`id_token = ["registry.tilo.so"]`), which the
# registry accepts for this repository's main, stable and v* tags; never signing credentials.
#
# usage: oci-publish.sh dev|release REPOSITORY OUT TAG...
# The first argument is the channel the check publishes to; it must be this run's (lib.sh), so a
# release check can never publish main's tree and a development check never stable's.
# Tags may contain {commit}, replaced by T_COMMIT, and on a release tag v<version> {version}.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
wanted=${1:?usage: oci-publish.sh dev|release REPOSITORY OUT TAG...}
repository=${2:?}
out=${3:?}
shift 3
[[ $(channel) == "$wanted" ]] || die "this check publishes to $wanted; the run is for ${T_REF:-}"
[[ $wanted == dev || -s $out/signature ]] || die 'a release is published signed only'
(($#)) || die 'no tags'
require_commit
digest=$(cat "$out/digest")
[[ $(archive_digest "$out/image.oci.tar") == "$digest" ]] || die 'the archive is not the build'
registry=${repository%%/*}
auth=$scratch/registry-auth.json
export REGISTRY_AUTH_FILE=$auth DOCKER_CONFIG=$scratch/docker
mkdir -p "$DOCKER_CONFIG"
password=$(id_token "$registry")
printf '%s' "$password" | skopeo login --authfile "$auth" -u oidc --password-stdin "$registry"
unset password
cp "$auth" "$DOCKER_CONFIG/config.json"
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
skopeo copy --all --preserve-digests --authfile "$auth" "oci-archive:$out/image.oci.tar" "docker://$repository:$first"
pushed=$(skopeo inspect --raw --authfile "$auth" "docker://$repository@$digest" | sha256sum | cut -d' ' -f1)
[[ sha256:$pushed == "$digest" ]] || die "the registry holds sha256:$pushed, not $digest"
if [[ -s $out/signature ]]; then
  use_cosign
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
  skopeo copy --all --preserve-digests --authfile "$auth" "docker://$repository@$digest" "docker://$repository:$tag"
  tags+=",\"$tag\""
done
signed=false
[[ -s $out/signature ]] && signed=true
printf '{"schemaVersion":3,"repository":"%s","digest":"%s","image":"%s@%s","tags":[%s],"sourceRevision":"%s","reference":"%s","signed":%s}\n' \
  "$repository" "$digest" "$repository" "$digest" "$tags" "$T_COMMIT" "$T_REF" "$signed" > "$out/receipt.json"
printf 'published %s@%s\n' "$repository" "$digest"
