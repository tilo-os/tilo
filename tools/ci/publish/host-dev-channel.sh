#!/usr/bin/env bash
# The development server's channel tag, printed when main's commit of this run may move it: the
# tag publish-host-dev points at this commit's build beside its own server-sha-<commit>, which a
# development server follows (tilo-update-channel). Nothing is printed when a build of a newer
# commit of main was published already, so a run that finishes late never moves the channel
# back. The server image names no commit (its build reads no Git), so the commits are read off
# the server-sha-<commit> tags of the repository instead of the image's labels.
#
# usage: host-dev-channel.sh TAG
# Logs in to registry.tilo.so with an identity token (lib.sh registry_login).
set -euo pipefail
here=$(dirname -- "${BASH_SOURCE[0]}")
. "$here/lib.sh"
cd "$repo_root"
tag=${1:?usage: host-dev-channel.sh TAG}
repository=registry.tilo.so/tilo/tilo-vm
[[ $(channel) == dev ]] || die "the development server's channel moves from main only; this run is for $T_REF"
require_commit
auth=$scratch/registry-auth.json
registry_login registry.tilo.so "$auth"
published=$(skopeo list-tags --authfile "$auth" "docker://$repository" \
  | sed -n 's/^[[:space:]]*"server-sha-\([0-9a-f]\{40\}\)",\{0,1\}$/\1/p')
for commit in $published; do
  # An ancestor of this commit is older; one this checkout lacks is asked of the forge, as the
  # dev desktop's channel is (lib.sh moves_forward).
  git merge-base --is-ancestor "$commit" "$T_COMMIT" 2>/dev/null && continue
  moves_forward "$commit" && continue
  printf 'a build of %s is published already; leaving %s\n' "$commit" "$tag" >&2
  exit 0
done
printf '%s\n' "$tag"
