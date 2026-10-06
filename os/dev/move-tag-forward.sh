#!/usr/bin/env bash
# Point a moving tag at this commit's image, only ever forward along history.
#
#   os/dev/move-tag-forward.sh SOURCE TAG
#
# SOURCE is this commit's image (a registry reference, best by digest), TAG the
# moving tag (e.g. registry.tilo.so/tilo/tilo-vm:lenovo-83ed-ci). Main builds
# run side by side, so a later commit's build can finish first: TAG moves only
# when the commit it names now (its org.opencontainers.image.revision) is this
# commit or one of its ancestors. GITHUB_SHA, GITHUB_SERVER_URL and
# GITHUB_REPOSITORY name this commit and repository; GITHUB_TOKEN, when set,
# authenticates the history lookup.
set -euo pipefail
source_ref="${1:?usage: move-tag-forward.sh SOURCE TAG}"
tag="${2:?usage: move-tag-forward.sh SOURCE TAG}"
sha="${GITHUB_SHA:?}"

revision_of() {
  # An index answers with one child's config; every child names one commit.
  skopeo inspect "docker://$1" 2>/dev/null \
    | jq -r '.Labels["org.opencontainers.image.revision"] // empty' || true
}

current="$(revision_of "$tag")"
if [[ -n $current && $current != "$sha" ]]; then
  auth=()
  [[ -z ${GITHUB_TOKEN:-} ]] || auth=(-H "Authorization: token $GITHUB_TOKEN")
  # Commits the tag's commit has that this one lacks: none means it is an ancestor.
  ahead="$(curl -fsS "${auth[@]}" \
    "$GITHUB_SERVER_URL/api/v1/repos/$GITHUB_REPOSITORY/compare/$sha...$current" \
    | jq -er '.total_commits')" || {
      echo "::warning::could not compare $current with $sha; leaving $tag"; exit 0; }
  if [[ $ahead != 0 ]]; then
    echo "::notice::$tag names $current, which is not an ancestor of $sha; leaving it"
    exit 0
  fi
fi
if skopeo inspect --raw "docker://$source_ref" | jq -e '.manifests? | type == "array"' >/dev/null; then
  skopeo copy --multi-arch index-only --preserve-digests "docker://$source_ref" "docker://$tag"
else
  skopeo copy --preserve-digests "docker://$source_ref" "docker://$tag"
fi
echo "::notice::$tag -> $source_ref ($sha)"
