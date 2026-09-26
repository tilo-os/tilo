#!/bin/bash
# Print the generation number the next Tilo image should carry.
#
#   ./os/dev/next-generation.sh            # next in the published series
#   ./os/dev/next-generation.sh --current  # the newest already published
#
# The generation is what bounded rollback is arithmetic on, so how it is produced
# is a correctness question rather than a bookkeeping one (see
# crates/system/tilo-rollback/src/generation.rs):
#
#   * dense — `+1` per published image. The retention window means "N, N-1, N-2",
#     so a sparse sequence keeps fewer images than it claims to. Worse, the floor
#     is a TPM NV counter and advancing it from A to B costs B-A NV writes;
#     tilo-rollback refuses an advance of more than 64 outright, so a numbering
#     that jumps by hundreds soft-bricks the machine into recovery-key-only.
#   * monotonic — an image must never carry a number an older image already used,
#     or two different images would share one sealed policy.
#
# So it cannot be a timestamp, and it cannot be a commit count: commits between
# releases would open gaps of exactly the size that breaks the arithmetic above.
#
# THE SIGNED SERIES IS ITS OWN RECORD. Every published image is tagged `gen<N>`
# and has signed SLSA provenance carrying the same generation. Registry tag names
# are attacker-controlled input: the resolver verifies the highest tag, its
# provenance, the latest channel, and a dense tag history before trusting it.
#
# Nothing here falls back to a constant when the registry cannot be reached. A
# constant is precisely the failure this replaces: it produced images that all
# claimed to be generation 0, whose rollback floor could therefore never move.
set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

# Where the published series lives. The generation counts published images, so it
# is read from the repository `bootc upgrade` pulls from, not from a scratch
# registry a disk build happened to push to.
TILO_PUBLISH_REPO="${TILO_PUBLISH_REPO:-registry.tilo.so/tilo/tilo}"

WANT=next
case "${1:-}" in
  --current) WANT=current ;;
  --next|'') WANT=next ;;
  -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
  *) die "unknown argument: $1" ;;
esac

# The published series is anonymously readable. A repository that does not
# exist fails the listing ("name unknown") rather than returning an empty list,
# which is the right way round: an empty list would silently restart the series
# at 1 and hand out a number that is already taken.
#
# A private TILO_PUBLISH_REPO needs credentials. TILO_REGISTRY_CREDS
# ("user:token") is the explicit path; it is converted immediately into a
# mode-0600 temporary auth file. Passing it
# through a credential command-line option would expose the token in process argv.
# Discovering an auth file works interactively and is unreliable in a job: the
# runner's `docker login` may write a `credsStore` entry with no credential in the
# file at all, and the first file that *exists* is not necessarily the one with the
# entry — which fails as "authentication required" and looks like a permissions
# problem rather than a lookup one.
TILO_REGISTRY_CREDS="${TILO_REGISTRY_CREDS:-}"
# A validation-only bootstrap may treat an authenticated registry's exact
# "name unknown" response as an empty series. Publication never sets this:
# auth, transport and every other lookup failure remain fatal.
TILO_ALLOW_EMPTY_SERIES="${TILO_ALLOW_EMPTY_SERIES:-0}"

# Cosign performs separate registry requests after tag discovery. Give it the
# same explicit credential in a private Docker config instead of relying on a
# runner credential helper that may not exist inside the job container.
TILO_COSIGN_AUTH_DIR=""
cleanup_auth() {
  [ -z "$TILO_COSIGN_AUTH_DIR" ] || rm -r "$TILO_COSIGN_AUTH_DIR"
}
trap cleanup_auth EXIT
if [ -n "$TILO_REGISTRY_CREDS" ]; then
  case "$TILO_REGISTRY_CREDS" in
    *:*) ;;
    *) die "TILO_REGISTRY_CREDS must be user:token" ;;
  esac
  [ -n "${TILO_REGISTRY_CREDS%%:*}" ] && [ -n "${TILO_REGISTRY_CREDS#*:}" ] \
    || die "TILO_REGISTRY_CREDS must contain a non-empty user and token"
  TILO_COSIGN_AUTH_DIR="$(mktemp -d)"
  chmod 0700 "$TILO_COSIGN_AUTH_DIR"
  encoded="$(printf '%s' "$TILO_REGISTRY_CREDS" | base64 | tr -d '\n')"
  printf '{"auths":{"%s":{"auth":"%s"}}}\n' \
    "${TILO_PUBLISH_REPO%%/*}" "$encoded" > "$TILO_COSIGN_AUTH_DIR/config.json"
  chmod 0600 "$TILO_COSIGN_AUTH_DIR/config.json"
  export DOCKER_CONFIG="$TILO_COSIGN_AUTH_DIR"
fi

# skopeo is not installed everywhere this runs — the host is image-based and the CI
# runner does not ship it — so fall back to running it in a container.
SKOPEO_IMAGE="${TILO_SKOPEO_IMAGE:-quay.io/skopeo/stable:latest}"
TILO_GENERATION_VERIFY_KEY="${TILO_GENERATION_VERIFY_KEY:-keys/cosign.pub}"

# An auth file that actually carries an entry for this registry, not merely the
# first one that exists.
find_authfile() {
  local host="${TILO_PUBLISH_REPO%%/*}" f
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
  local auth tmp rc encoded
  if [ -n "$TILO_REGISTRY_CREDS" ]; then
    case "$TILO_REGISTRY_CREDS" in
      *:*) ;;
      *) echo "TILO_REGISTRY_CREDS must be user:token" >&2; return 1 ;;
    esac
    [ -n "${TILO_REGISTRY_CREDS%%:*}" ] && [ -n "${TILO_REGISTRY_CREDS#*:}" ] || {
      echo "TILO_REGISTRY_CREDS must contain a non-empty user and token" >&2
      return 1
    }
    tmp="$(mktemp -d)" || return 1
    chmod 0700 "$tmp" || { rm -r "$tmp"; return 1; }
    auth="$tmp/auth.json"
    encoded="$(printf '%s' "$TILO_REGISTRY_CREDS" | base64 | tr -d '\n')" \
      || { rm -r "$tmp"; return 1; }
    if ! printf '{"auths":{"%s":{"auth":"%s"}}}\n' \
        "${TILO_PUBLISH_REPO%%/*}" "$encoded" > "$auth" \
      || ! chmod 0600 "$auth"; then
      rm -r "$tmp"
      return 1
    fi
  fi

  if command -v skopeo >/dev/null 2>&1; then
    if [ -n "${auth:-}" ]; then
      if REGISTRY_AUTH_FILE="$auth" skopeo list-tags "docker://$TILO_PUBLISH_REPO"; then
        rc=0
      else
        rc=$?
      fi
      rm -r "$tmp"
      return $rc
    fi
    skopeo list-tags "docker://$TILO_PUBLISH_REPO"
    return $?
  fi

  local engine
  engine="$(command -v podman || command -v docker)" || {
    echo "no skopeo, and no podman or docker to run it in" >&2; return 1; }

  if [ -z "${auth:-}" ]; then
  # Copy into a directory of our own first. The resulting file is streamed over
  # stdin below: an Actions job commonly controls a host Docker daemon, which
  # cannot bind-mount paths from the job container's private filesystem. With
  # no auth file carrying an entry for the registry, the lookup is anonymous.
    tmp="$(mktemp -d)" || return 1
    if auth="$(find_authfile)"; then
      if ! cp "$auth" "$tmp/auth.json" || ! chmod 0600 "$tmp/auth.json"; then
        rm -r "$tmp"
        return 1
      fi
    elif ! (umask 077 && printf '{"auths":{}}\n' > "$tmp/auth.json"); then
      rm -r "$tmp"
      return 1
    fi
  fi
  if "$engine" run --rm --interactive \
      --entrypoint /bin/sh \
      -e REGISTRY_AUTH_FILE=/tmp/tilo-auth.json \
      "$SKOPEO_IMAGE" -ceu \
      'umask 077; cat > "$REGISTRY_AUTH_FILE"; exec skopeo list-tags "docker://$1"' \
      sh "$TILO_PUBLISH_REPO" < "$tmp/auth.json"; then
    rc=0
  else
    rc=$?
  fi
  rm -r "$tmp"
  return $rc
}

tag_error="$(mktemp)"
if tags="$(list_tags 2>"$tag_error")"; then
  cat "$tag_error" >&2
elif [ "$TILO_ALLOW_EMPTY_SERIES" = 1 ] \
    && grep -Fq 'fetching tags list: name unknown' "$tag_error" \
    && ! grep -Eiq 'unauthorized|denied|authentication required' "$tag_error"; then
  echo "[tilo] validation bootstrap: $TILO_PUBLISH_REPO has no published series yet" >&2
  tags='{"Tags":[]}'
else
  lookup_error="$(cat "$tag_error")"
  rm -f "$tag_error"
  die "could not list tags of
  $TILO_PUBLISH_REPO — the published generation series is unreadable, and guessing
  it would produce an image whose generation collides with one already published.
  Check that the registry is reachable (a private TILO_PUBLISH_REPO needs a
  login or TILO_REGISTRY_CREDS), or pass TILO_GENERATION=<N> explicitly.
  The tag listing said:
$(printf '%s' "$lookup_error" | sed 's/^/    /')"
fi
rm -f "$tag_error"

# The highest gen<N> tag. `sort -n | tail -1` rather than a max in the pipeline,
# because a lexical sort makes gen9 the newest of gen9..gen10.
command -v jq >/dev/null 2>&1 || die "jq is required to parse the registry response safely"
generations="$(printf '%s' "$tags" | jq -r '
  .Tags // [] | .[] | select(test("^gen[0-9]+$")) | sub("^gen"; "")
' | sort -n)" || die "the registry returned an invalid tag listing"
current="$(printf '%s\n' "$generations" | tail -1)"
current="${current:-0}"

verify_signed_generation() {
  local generation="$1" image="$TILO_PUBLISH_REPO:gen$1" signatures provenance
  command -v cosign >/dev/null 2>&1 || die \
    "cosign is required: unsigned registry tags are not a generation oracle"
  [ -s "$TILO_GENERATION_VERIFY_KEY" ] || die \
    "release verification key is missing: $TILO_GENERATION_VERIFY_KEY"
  signatures="$(cosign verify --key "$TILO_GENERATION_VERIFY_KEY" "$image")" || die \
    "gen$generation has no valid transparency-logged Tilo release signature"
  provenance="$(cosign verify-attestation --key "$TILO_GENERATION_VERIFY_KEY" \
    --type slsaprovenance1 "$image")" || die \
    "gen$generation has no valid transparency-logged SLSA provenance"
  printf '%s' "$provenance" | jq -e --argjson generation "$generation" '
    [(if type == "array" then .[] else . end)
      | .payload | @base64d | fromjson
      | .predicate.buildDefinition.externalParameters.generation]
    | any(. == $generation)
  ' >/dev/null || die \
    "gen$generation provenance does not bind the tag to generation $generation"
  printf '%s' "$signatures" | jq -er \
    '.[0].critical.image["docker-manifest-digest"]' | head -1
}

if [ "$current" != 0 ]; then
  # Missing historical tags make deletion/reset indistinguishable from a fresh
  # series. Refuse before assigning a duplicate generation.
  expected=1
  while IFS= read -r generation; do
    [ -n "$generation" ] || continue
    [ "$generation" -eq "$expected" ] || die \
      "published generation history is not dense: expected gen$expected, found gen$generation"
    expected=$(( expected + 1 ))
  done <<< "$generations"

  current_digest="$(verify_signed_generation "$current")"
  latest_signatures="$(cosign verify --key "$TILO_GENERATION_VERIFY_KEY" \
    "$TILO_PUBLISH_REPO:latest")" || die \
    "latest has no valid transparency-logged Tilo release signature"
  latest_digest="$(printf '%s' "$latest_signatures" | jq -er \
    '.[0].critical.image["docker-manifest-digest"]')"
  [ "$latest_digest" = "$current_digest" ] || die \
    "latest does not point at signed gen$current; refusing a split release series"
fi

if [ "$WANT" = current ]; then
  printf '%s\n' "$current"
  exit 0
fi

next=$(( current + 1 ))

# A gen tag for the number we are about to hand out means the series was read
# wrong, and reusing it would give two images one sealed policy.
if grep -q "\"gen$next\"" <<<"$tags"; then
  die "gen$next is already a tag in $TILO_PUBLISH_REPO, but gen$current was read as the
  newest. Refusing to reuse a published generation number."
fi

if [ "$current" = 0 ]; then
  msg "no gen<N> tags in $TILO_PUBLISH_REPO yet — starting the series at 1"
else
  msg "published generation series tops out at $current; next is $next"
fi
printf '%s\n' "$next"
