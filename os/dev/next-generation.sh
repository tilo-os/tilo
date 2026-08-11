#!/bin/bash
# Print the generation number the next qOS image should carry.
#
#   ./os/dev/next-generation.sh            # next in the published series
#   ./os/dev/next-generation.sh --current  # the newest already published
#
# The generation is what bounded rollback is arithmetic on, so how it is produced
# is a correctness question rather than a bookkeeping one (see
# crates/system/qos-rollback/src/generation.rs):
#
#   * dense — `+1` per published image. The retention window means "N, N-1, N-2",
#     so a sparse sequence keeps fewer images than it claims to. Worse, the floor
#     is a TPM NV counter and advancing it from A to B costs B-A NV writes;
#     qos-rollback refuses an advance of more than 64 outright, so a numbering
#     that jumps by hundreds soft-bricks the machine into recovery-key-only.
#   * monotonic — an image must never carry a number an older image already used,
#     or two different images would share one sealed policy.
#
# So it cannot be a timestamp, and it cannot be a commit count: commits between
# releases would open gaps of exactly the size that breaks the arithmetic above.
#
# THE SERIES IS ITS OWN RECORD. Every published image is tagged `gen<N>` in the
# registry, so the next number is `max(gen tags) + 1`, read from the place the
# images actually are. That needs no counter file to keep in sync with reality, no
# commit written back by CI, and it cannot drift from what was published — which a
# tracked file can, in either direction, silently.
#
# Nothing here falls back to a constant when the registry cannot be reached. A
# constant is precisely the failure this replaces: it produced images that all
# claimed to be generation 0, whose rollback floor could therefore never move.
set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

# Where the published series lives. The generation counts published images, so it
# is read from the repository `bootc upgrade` pulls from, not from a scratch
# registry a disk build happened to push to.
QOS_PUBLISH_REPO="${QOS_PUBLISH_REPO:-code.podesta.ai/qos/qos}"

WANT=next
case "${1:-}" in
  --current) WANT=current ;;
  --next|'') WANT=next ;;
  -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
  *) die "unknown argument: $1" ;;
esac

# The registry is private, so the listing has to be authenticated: an anonymous
# read fails rather than returning an empty list, which is the right way round —
# an empty list would silently restart the series at 1 and hand out a number that
# is already taken.
#
# QOS_REGISTRY_CREDS ("user:token") is the explicit path, and it is what CI uses.
# Discovering an auth file works interactively and is unreliable in a job: the
# runner's `docker login` may write a `credsStore` entry with no credential in the
# file at all, and the first file that *exists* is not necessarily the one with the
# entry — which fails as "authentication required" and looks like a permissions
# problem rather than a lookup one.
QOS_REGISTRY_CREDS="${QOS_REGISTRY_CREDS:-}"

# skopeo is not installed everywhere this runs — the host is image-based and the CI
# runner does not ship it — so fall back to running it in a container.
SKOPEO_IMAGE="${QOS_SKOPEO_IMAGE:-quay.io/skopeo/stable:latest}"

# An auth file that actually carries an entry for this registry, not merely the
# first one that exists.
find_authfile() {
  local host="${QOS_PUBLISH_REPO%%/*}" f
  for f in "${REGISTRY_AUTH_FILE:-}" \
           "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/containers/auth.json" \
           "$HOME/.config/containers/auth.json" \
           "$HOME/.docker/config.json"; do
    [ -n "$f" ] && [ -f "$f" ] || continue
    grep -q "\"$host\"" "$f" 2>/dev/null || continue
    printf '%s\n' "$f"; return 0
  done
  return 1
}

list_tags() {
  local creds_args=() auth tmp rc
  if [ -n "$QOS_REGISTRY_CREDS" ]; then
    creds_args=(--creds "$QOS_REGISTRY_CREDS")
  fi

  if command -v skopeo >/dev/null 2>&1; then
    skopeo list-tags "${creds_args[@]+"${creds_args[@]}"}" \
      "docker://$QOS_PUBLISH_REPO" 2>&1
    return
  fi

  local engine
  engine="$(command -v podman || command -v docker)" || {
    echo "no skopeo, and no podman or docker to run it in" >&2; return 1; }

  if [ ${#creds_args[@]} -gt 0 ]; then
    "$engine" run --rm "$SKOPEO_IMAGE" \
      list-tags "${creds_args[@]}" "docker://$QOS_PUBLISH_REPO" 2>&1
    return
  fi

  auth="$(find_authfile)" || {
    echo "no container auth file carries an entry for ${QOS_PUBLISH_REPO%%/*}." >&2
    echo "Log in (podman login ${QOS_PUBLISH_REPO%%/*}), or set" >&2
    echo "QOS_REGISTRY_CREDS=user:token." >&2
    return 1; }
  # A copy in a directory of our own, mounted with an SELinux relabel. Bind-mounting
  # the credential file straight in fails "permission denied" reading it — the
  # container cannot see through the label on $XDG_RUNTIME_DIR — and relabelling the
  # real file to fix that would rewrite the host's credential store.
  tmp="$(mktemp -d)"
  cp "$auth" "$tmp/auth.json" && chmod 0644 "$tmp/auth.json"
  "$engine" run --rm \
    -e REGISTRY_AUTH_FILE=/run/qos-auth/auth.json \
    -v "$tmp:/run/qos-auth:ro,z" \
    "$SKOPEO_IMAGE" list-tags "docker://$QOS_PUBLISH_REPO" 2>&1
  rc=$?
  rm -rf "$tmp"
  return $rc
}

tags="$(list_tags)" || die "could not list tags of
  $QOS_PUBLISH_REPO — the published generation series is unreadable, and guessing
  it would produce an image whose generation collides with one already published.
  Log in (podman login code.podesta.ai) or pass QOS_GENERATION=<N> explicitly.
  The tag listing said:
$(printf '%s' "$tags" | sed 's/^/    /')"

# The highest gen<N> tag. `sort -n | tail -1` rather than a max in the pipeline,
# because a lexical sort makes gen9 the newest of gen9..gen10.
current="$(printf '%s' "$tags" \
  | sed -n 's/.*"gen\([0-9][0-9]*\)".*/\1/p' \
  | sort -n | tail -1)"
current="${current:-0}"

if [ "$WANT" = current ]; then
  printf '%s\n' "$current"
  exit 0
fi

next=$(( current + 1 ))

# A gen tag for the number we are about to hand out means the series was read
# wrong, and reusing it would give two images one sealed policy.
if printf '%s' "$tags" | grep -q "\"gen$next\""; then
  die "gen$next is already a tag in $QOS_PUBLISH_REPO, but gen$current was read as the
  newest. Refusing to reuse a published generation number."
fi

if [ "$current" = 0 ]; then
  msg "no gen<N> tags in $QOS_PUBLISH_REPO yet — starting the series at 1"
else
  msg "published generation series tops out at $current; next is $next"
fi
printf '%s\n' "$next"
