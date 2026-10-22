#!/usr/bin/env bash
# Copy every image of mirrored-images.tsv into its mirror in registry.tilo.so, digest for digest,
# unless the mirror has it already. The copy is of the pinned digest, never of whatever the
# upstream tag points to now, so a mirror holds exactly what the pins were reviewed against; an
# upstream that no longer serves the digest fails here, before anything depends on it.
# Logs in to the registry as `oidc` with an identity token from t (`id_token =
# ["registry.tilo.so"]`). With TILO_PUBLISH_DRY_RUN it checks the list and that upstream (or the
# mirror) still serves each digest, and copies nothing.
#
# usage: mirror-images.sh [LIST]
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
list=${1:-$repo_root/tools/ci/publish/mirrored-images.tsv}
auth=$scratch/mirror-auth.json
logged_in=
login() {
  [[ -n $logged_in ]] && return
  local password
  password=$(id_token registry.tilo.so)
  printf '%s' "$password" | skopeo login --authfile "$auth" -u oidc --password-stdin registry.tilo.so
  logged_in=1
}
digest_of() { skopeo inspect --raw "$@" | sha256sum | cut -d' ' -f1; }
while IFS=$'\t' read -r mirror upstream digest; do
  [[ -z $mirror || $mirror == '#'* ]] && continue
  [[ $mirror =~ ^registry\.tilo\.so/tilo/[a-z0-9._-]+$ ]] || die "not a mirror in registry.tilo.so/tilo: $mirror"
  [[ $upstream =~ ^[a-z0-9.-]+/[a-z0-9._/-]+:[A-Za-z0-9._-]+$ ]] || die "not an upstream tag: $upstream"
  [[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "not a digest: $digest"
  if skopeo inspect --raw "docker://$mirror@$digest" >/dev/null 2>&1; then
    printf 'mirrored already: %s@%s\n' "$mirror" "$digest"
    continue
  fi
  source="docker://${upstream%:*}@$digest"
  [[ sha256:$(digest_of "$source") == "$digest" ]] || die "$upstream no longer serves $digest; pin a digest it serves"
  if [[ -n ${TILO_PUBLISH_DRY_RUN:-} ]]; then
    printf 'would mirror %s@%s from %s (dry run)\n' "$mirror" "$digest" "$upstream"
    continue
  fi
  login
  tag=${upstream##*:}
  # A multi-gigabyte layer copies for minutes without skopeo printing a line; say it still is.
  (while sleep 60; do printf 'still copying %s@%s\n' "$mirror" "$digest"; done) &
  ticker=$!
  copied=0
  skopeo copy --all --preserve-digests --retry-times 3 --authfile "$auth" "$source" "docker://$mirror:$tag" \
    || copied=$?
  kill "$ticker"
  ((copied == 0)) || die "copying $upstream failed"
  [[ sha256:$(digest_of "docker://$mirror@$digest") == "$digest" ]] || die "the mirror does not hold $digest after copying"
  printf 'mirrored %s@%s from %s\n' "$mirror" "$digest" "$upstream"
done < "$list"
