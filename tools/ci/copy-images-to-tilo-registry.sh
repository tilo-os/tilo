#!/usr/bin/env bash
# Copy every container image this repository references from the forge's
# registry to registry.tilo.so, byte for byte, so pinned @sha256 references
# keep resolving under the new names.
#
# Dry run by default: it prints the plan and contacts no registry. Set
# APPLY=1 to copy. Re-running is safe: a digest already present at the
# destination is skipped, and skopeo copies only missing blobs.
#
#   OWNER_MOVES   required. Space-separated OLD:NEW owner pairs, e.g.
#                 "<old-owner>:tilo public:tilo". Only these owners move;
#                 several old owners may land in one new owner.
#   SOURCE_REGISTRY   default code.sandbox.example
#   DEST_REGISTRY     default registry.tilo.so
#   SOURCE_USERNAME/SOURCE_TOKEN
#                 optional source credentials; otherwise the caller's
#                 containers auth file is used.
#   DEST_TOKEN    optional destination password (DEST_USERNAME, default
#                 `oidc`). Without it, inside a Forgejo Actions job with
#                 `enable-openid-connect: true`, a fresh ID token is fetched
#                 for every push: registry.tilo.so accepts it as the password
#                 of user `oidc` and grants push below tilo/ to allow-listed
#                 source repositories. copy-images-to-tilo-registry.yml runs
#                 this script that way.
#   ALL_TAGS=1    copy every tag of each referenced repository, not only the
#                 tags and digests named in the tree. Includes cosign tags.
#   RESIGN=1      after copying, cosign-sign each copied digest of a
#                 repository that os/etc/containers/policy.json verifies with
#                 sigstoreSigned/matchRepository. Needs COSIGN_KEY_REF (and
#                 AWS_SIGNING_ROLE_ARN plus a Forgejo OIDC token for awskms://,
#                 as in the release workflow). Copied signatures still name
#                 the old repository, which that policy refuses.
#   SCAN_ROOT     default: this checkout.
#
# Signature (.sig), attestation (.att) and SBOM (.sbom) tags that cosign keeps
# next to a digest are copied with it, for an index and for each child.
set -euo pipefail

SOURCE_REGISTRY=${SOURCE_REGISTRY:-code.sandbox.example}
DEST_REGISTRY=${DEST_REGISTRY:-registry.tilo.so}
SCAN_ROOT=${SCAN_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}
APPLY=${APPLY:-0}
ALL_TAGS=${ALL_TAGS:-0}
RESIGN=${RESIGN:-0}
: "${OWNER_MOVES:?set OWNER_MOVES to space-separated OLD:NEW owner pairs}"
ID_TOKEN="$SCAN_ROOT/.forgejo/actions/tilo-registry-login/id-token.sh"

die() { printf 'copy-images: %s\n' "$*" >&2; exit 1; }
plan() { printf '%s\n' "$*"; }

declare -A OWNER_NEW=() OWNER_OLD=()
for pair in $OWNER_MOVES; do
  [[ $pair =~ ^([a-z0-9][a-z0-9._-]*):([a-z0-9][a-z0-9._-]*)$ ]] \
    || die "malformed OWNER_MOVES entry: $pair"
  OWNER_NEW[${BASH_REMATCH[1]}]=${BASH_REMATCH[2]}
  OWNER_OLD[${BASH_REMATCH[2]}]+="${OWNER_OLD[${BASH_REMATCH[2]}]:+ }${BASH_REMATCH[1]}"
done

name_re='[a-z0-9][a-z0-9._/-]*[a-z0-9]'
tag_re='[A-Za-z0-9_]([A-Za-z0-9_.-]{0,126}[A-Za-z0-9_])?'
digest_re='sha256:[0-9a-f]{64}'

# Collect "<new-owner>/<name> <tag|-> <digest|->" for every reference to
# either the old or the new location. Test fixtures and experiment records
# name repositories that were never published, so they are not scanned.
references() {
  local registry pattern
  for registry in "$SOURCE_REGISTRY" "$DEST_REGISTRY"; do
    # The trailing class drops references that continue into a shell or
    # workflow expression ("tilo:main-${sha}"): only complete names are real.
    pattern="${registry//./\\.}/[a-z0-9][a-z0-9._-]*/${name_re}(:${tag_re})?(@${digest_re})?([^A-Za-z0-9_.$\{/-]|\$)"
    git -C "$SCAN_ROOT" grep -hoE "$pattern" -- . \
        ':(exclude,glob)**/tests/**' ':(exclude,glob)**/test_*' ':(exclude,glob)**/*_test.*' \
        ':(exclude,glob)**/*_tests.rs' ':(exclude,glob)**/tests.rs' \
        ':(exclude,glob)experiments/**' ':(exclude)tools/ci/copy-images-to-tilo-registry.sh' || true
  done | sed -E 's/[^A-Za-z0-9_]$//' | sort -u | while IFS= read -r ref; do
    local rest=${ref#*/} owner name tag=- digest=- origin=-
    owner=${rest%%/*}
    rest=${rest#*/}
    if [[ $rest == *@sha256:* ]]; then digest=${rest##*@}; rest=${rest%@*}; fi
    if [[ $rest == *:* ]]; then tag=${rest##*:}; rest=${rest%:*}; fi
    name=$rest
    case $name in *.git) continue ;; esac
    if [[ $ref == "$SOURCE_REGISTRY"/* ]]; then
      [[ -n ${OWNER_NEW[$owner]:-} ]] || continue
      origin=$owner
      owner=${OWNER_NEW[$owner]}
    else
      [[ -n ${OWNER_OLD[$owner]:-} ]] || continue
    fi
    printf '%s/%s %s %s %s\n' "$owner" "$name" "$tag" "$digest" "$origin"
  done | sort -u
}

old_repositories() {  # $1 = <new-owner>/<name>, $2 = old owner or - for any it may come from
  local old owners=${OWNER_OLD[${1%%/*}]}
  [[ ${2:--} == - ]] || owners=$2
  for old in $owners; do
    printf '%s/%s/%s\n' "$SOURCE_REGISTRY" "$old" "${1#*/}"
  done
}

signed_repositories() {
  jq -r '.transports.docker | to_entries[]
         | select(any(.value[]; .type == "sigstoreSigned")) | .key' \
    "$SCAN_ROOT/os/etc/containers/policy.json"
}

mapfile -t ROWS < <(references)
((${#ROWS[@]})) || die "no references to $SOURCE_REGISTRY or $DEST_REGISTRY for owners: $OWNER_MOVES"
mapfile -t SIGNED < <(signed_repositories)

is_signed_repository() {
  local candidate
  for candidate in "${SIGNED[@]}"; do [[ $candidate == "$1" ]] && return 0; done
  return 1
}

if [[ $APPLY != 1 ]]; then
  plan "# dry run: set APPLY=1 to copy (repositories: $(printf '%s\n' "${ROWS[@]}" | cut -d' ' -f1 | sort -u | wc -l))"
  for row in "${ROWS[@]}"; do
    read -r repository tag digest origin <<<"$row"
    [[ $tag == - && $digest == - && $ALL_TAGS != 1 ]] && continue
    sources=$(old_repositories "$repository" "$origin" | paste -sd'|')
    dest="$DEST_REGISTRY/$repository"
    if [[ $ALL_TAGS == 1 && $tag == - && $digest == - ]]; then
      plan "copy all tags  $sources -> $dest"
      if [[ $RESIGN == 1 ]] && is_signed_repository "$dest"; then
        plan "re-sign        every non-cosign tag of $dest"
      fi
      continue
    fi
    [[ $digest == - ]] || plan "copy digest    $sources@$digest -> $dest@$digest (+ cosign tags)"
    [[ $tag == - ]] || plan "copy tag       $sources:$tag -> $dest:$tag"
    if [[ $RESIGN == 1 ]] && is_signed_repository "$dest"; then
      [[ $digest == - ]] || plan "re-sign        $dest@$digest"
      [[ $tag == - ]] || plan "re-sign        $dest@<digest of :$tag>"
    fi
  done
  exit 0
fi

# A missing credential is the likelier operator mistake and needs no tools
# to report, so name it before checking what is installed.
if [[ -z ${DEST_TOKEN:-} ]]; then
  [[ -n ${ACTIONS_ID_TOKEN_REQUEST_URL:-} ]] \
    || die 'set DEST_TOKEN, or run inside a Forgejo Actions job with enable-openid-connect: true'
fi
command -v skopeo >/dev/null || die 'skopeo is required'
command -v jq >/dev/null || die 'jq is required'
if [[ $RESIGN == 1 ]]; then
  command -v cosign >/dev/null || die 'cosign is required for RESIGN=1'
  # Same KMS-only guard and AWS web-identity setup as the release workflow.
  . "$SCAN_ROOT/os/dev/configure-cosign-kms.sh"
fi

work=$(mktemp -d "${XDG_RUNTIME_DIR:-$HOME/.cache}/copy-images.XXXXXX")
trap 'rm -rf "$work" "${AWS_WEB_IDENTITY_TOKEN_FILE:-}"' EXIT
src_creds=()
if [[ -n ${SOURCE_TOKEN:-} ]]; then
  src_creds=(--creds "${SOURCE_USERNAME:?SOURCE_USERNAME is required with SOURCE_TOKEN}:$SOURCE_TOKEN")
fi

# An ID token lives an hour and a copy run can take longer, so every
# destination operation logs in again with a fresh one. The same file is
# cosign's docker config.
export DOCKER_CONFIG="$work/docker"
mkdir -p "$DOCKER_CONFIG"
dest_login() {
  if [[ -n ${DEST_TOKEN:-} ]]; then
    printf '%s' "$DEST_TOKEN"
  else
    "$ID_TOKEN"
  fi | skopeo login --authfile "$DOCKER_CONFIG/config.json" \
         --username "${DEST_USERNAME:-oidc}" --password-stdin "$DEST_REGISTRY" >/dev/null
}
inspect_src() { skopeo inspect --raw "${src_creds[@]}" "docker://$1"; }
inspect_dst() { skopeo inspect --raw "docker://$1"; }  # pulls are anonymous

copied=0 skipped=0 missing=0 failed=0
declare -A DONE=()

copy_one() {  # $1 dest ref, $2... candidate source refs (first that exists wins)
  local dest=$1 source
  shift
  [[ -z ${DONE[$dest]:-} ]] || return 0
  DONE[$dest]=1
  if [[ $dest == *@sha256:* ]] && inspect_dst "$dest" >/dev/null 2>&1; then
    plan "present        $dest"; skipped=$((skipped + 1)); return 0
  fi
  for source in "$@"; do
    inspect_src "$source" >/dev/null 2>&1 || continue
    dest_login
    if skopeo copy --all --preserve-digests --retry-times 3 "${src_creds[@]/--creds/--src-creds}" \
         --dest-authfile "$DOCKER_CONFIG/config.json" "docker://$source" "docker://$dest" >/dev/null; then
      plan "copied         $source -> $dest"; copied=$((copied + 1))
    else
      plan "FAILED         $source -> $dest"; failed=$((failed + 1))
    fi
    return 0
  done
  plan "missing        $dest (not at: $*)"; missing=$((missing + 1))
}

with_suffix() {  # $1 suffix, rest: repositories
  local suffix=$1 each
  shift
  for each in "$@"; do printf '%s%s\n' "$each" "$suffix"; done
}

copy_cosign_tags() {  # $1 dest repo, $2 digest, rest: source repos
  local dest=$1 digest=$2 hex=${2#sha256:} suffix child source manifest
  local -a candidates
  shift 2
  for suffix in sig att sbom; do
    mapfile -t candidates < <(with_suffix ":sha256-$hex.$suffix" "$@")
    for source in "${candidates[@]}"; do
      if inspect_src "$source" >/dev/null 2>&1; then
        copy_one "$dest:sha256-$hex.$suffix" "$source"
        break
      fi
    done
  done
  # Release indexes sign every native child as well as the index itself.
  for source in "$@"; do
    if manifest=$(inspect_src "$source@$digest" 2>/dev/null); then
      while IFS= read -r child; do
        [[ -n $child ]] && copy_cosign_tags "$dest" "$child" "$@"
      done < <(jq -r '.manifests[]?.digest // empty' <<<"$manifest")
      break
    fi
  done
}

resolve_digest() {  # rest: candidate source refs; prints the first one's digest
  local source
  for source in "$@"; do
    skopeo inspect --format '{{.Digest}}' "${src_creds[@]}" "docker://$source" 2>/dev/null && return 0
  done
  return 1
}

declare -a RESIGN_TARGETS=()
for row in "${ROWS[@]}"; do
  read -r repository tag digest origin <<<"$row"
  mapfile -t sources < <(old_repositories "$repository" "$origin")
  dest="$DEST_REGISTRY/$repository"
  if [[ $ALL_TAGS == 1 && $tag == - && $digest == - ]]; then
    for source in "${sources[@]}"; do
      while IFS= read -r each; do
        copy_one "$dest:$each" "$source:$each"
        if is_signed_repository "$dest" && [[ $each != sha256-* ]] \
           && resolved=$(resolve_digest "$source:$each"); then
          RESIGN_TARGETS+=("$dest@$resolved")
        fi
      done < <(skopeo list-tags "${src_creds[@]}" "docker://$source" 2>/dev/null | jq -r '.Tags[]?')
    done
    continue
  fi
  if [[ $digest != - ]]; then
    mapfile -t candidates < <(with_suffix "@$digest" "${sources[@]}")
    copy_one "$dest@$digest" "${candidates[@]}"
    copy_cosign_tags "$dest" "$digest" "${sources[@]}"
    is_signed_repository "$dest" && RESIGN_TARGETS+=("$dest@$digest")
  fi
  if [[ $tag != - ]]; then
    # Tags move; copy what the tag names now and keep it named the same.
    mapfile -t candidates < <(with_suffix ":$tag" "${sources[@]}")
    copy_one "$dest:$tag" "${candidates[@]}"
    if resolved=$(resolve_digest "${candidates[@]}"); then
      copy_cosign_tags "$dest" "$resolved" "${sources[@]}"
      is_signed_repository "$dest" && RESIGN_TARGETS+=("$dest@$resolved")
    fi
  fi
done

if [[ $RESIGN == 1 ]]; then
  for target in $(printf '%s\n' "${RESIGN_TARGETS[@]}" | sort -u); do
    dest_login
    if cosign sign --yes --key "$COSIGN_KEY_REF" "$target" >/dev/null \
       && cosign verify --key "$SCAN_ROOT/keys/cosign.pub" "$target" >/dev/null; then
      plan "re-signed      $target"
    else
      plan "FAILED re-sign $target"; failed=$((failed + 1))
    fi
  done
fi

plan "# copied=$copied present=$skipped missing-at-source=$missing failed=$failed"
((failed == 0))
