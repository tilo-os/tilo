#!/usr/bin/env bash
# Copy every container image this repository references from the old forge
# host/owner to code.tilo.so, byte for byte, so pinned @sha256 references keep
# resolving after the rename.
#
# Dry run by default: it prints the plan and contacts no registry. Set
# APPLY=1 to copy. Re-running is safe: a digest already present at the
# destination is skipped, and skopeo copies only missing blobs.
#
#   OWNER_MOVES   required. Space-separated OLD:NEW owner pairs, e.g.
#                 "<old-owner>:tilo public:public". Only these owners move.
#   SOURCE_REGISTRY   default code.sandbox.example
#   DEST_REGISTRY     default code.tilo.so
#   SOURCE_USERNAME/SOURCE_TOKEN, DEST_USERNAME/DEST_TOKEN
#                 optional credentials; otherwise the caller's existing
#                 containers auth file (skopeo login / podman login) is used.
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
DEST_REGISTRY=${DEST_REGISTRY:-code.tilo.so}
SCAN_ROOT=${SCAN_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}
APPLY=${APPLY:-0}
ALL_TAGS=${ALL_TAGS:-0}
RESIGN=${RESIGN:-0}
: "${OWNER_MOVES:?set OWNER_MOVES to space-separated OLD:NEW owner pairs}"

die() { printf 'copy-images: %s\n' "$*" >&2; exit 1; }
plan() { printf '%s\n' "$*"; }

declare -A OWNER_NEW=() OWNER_OLD=()
for pair in $OWNER_MOVES; do
  [[ $pair =~ ^([a-z0-9][a-z0-9._-]*):([a-z0-9][a-z0-9._-]*)$ ]] \
    || die "malformed OWNER_MOVES entry: $pair"
  OWNER_NEW[${BASH_REMATCH[1]}]=${BASH_REMATCH[2]}
  OWNER_OLD[${BASH_REMATCH[2]}]=${BASH_REMATCH[1]}
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
        ':(exclude,glob)experiments/**' ':(exclude)tools/ci/copy-images-to-tilo-owner.sh' || true
  done | sed -E 's/[^A-Za-z0-9_]$//' | sort -u | while IFS= read -r ref; do
    local rest=${ref#*/} owner name tag=- digest=-
    owner=${rest%%/*}
    rest=${rest#*/}
    if [[ $rest == *@sha256:* ]]; then digest=${rest##*@}; rest=${rest%@*}; fi
    if [[ $rest == *:* ]]; then tag=${rest##*:}; rest=${rest%:*}; fi
    name=$rest
    case $name in *.git) continue ;; esac
    if [[ $ref == "$SOURCE_REGISTRY"/* ]]; then
      [[ -n ${OWNER_NEW[$owner]:-} ]] || continue
      owner=${OWNER_NEW[$owner]}
    else
      [[ -n ${OWNER_OLD[$owner]:-} ]] || continue
    fi
    printf '%s/%s %s %s\n' "$owner" "$name" "$tag" "$digest"
  done | sort -u
}

old_repository() {  # $1 = <new-owner>/<name>
  printf '%s/%s/%s' "$SOURCE_REGISTRY" "${OWNER_OLD[${1%%/*}]}" "${1#*/}"
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
    read -r repository tag digest <<<"$row"
    [[ $tag == - && $digest == - && $ALL_TAGS != 1 ]] && continue
    source=$(old_repository "$repository")
    dest="$DEST_REGISTRY/$repository"
    if [[ $ALL_TAGS == 1 && $tag == - && $digest == - ]]; then
      plan "copy all tags  $source -> $dest"
      if [[ $RESIGN == 1 ]] && is_signed_repository "$dest"; then
        plan "re-sign        every non-cosign tag of $dest"
      fi
      continue
    fi
    [[ $digest == - ]] || plan "copy digest    $source@$digest -> $dest@$digest (+ cosign tags)"
    [[ $tag == - ]] || plan "copy tag       $source:$tag -> $dest:$tag"
    if [[ $RESIGN == 1 ]] && is_signed_repository "$dest"; then
      [[ $digest == - ]] || plan "re-sign        $dest@$digest"
      [[ $tag == - ]] || plan "re-sign        $dest@<digest of :$tag>"
    fi
  done
  exit 0
fi

command -v skopeo >/dev/null || die 'skopeo is required'
command -v jq >/dev/null || die 'jq is required'
if [[ $RESIGN == 1 ]]; then
  command -v cosign >/dev/null || die 'cosign is required for RESIGN=1'
  # Same KMS-only guard and AWS web-identity setup as the release workflow.
  . "$SCAN_ROOT/os/dev/configure-cosign-kms.sh"
fi

auth=$(mktemp -d "${XDG_RUNTIME_DIR:-$HOME/.cache}/copy-images.XXXXXX")
trap 'rm -rf "$auth"' EXIT
src_creds=() dst_creds=()
if [[ -n ${SOURCE_TOKEN:-} ]]; then
  src_creds=(--src-creds "${SOURCE_USERNAME:?SOURCE_USERNAME is required with SOURCE_TOKEN}:$SOURCE_TOKEN")
fi
if [[ -n ${DEST_TOKEN:-} ]]; then
  dst_creds=(--dest-creds "${DEST_USERNAME:?DEST_USERNAME is required with DEST_TOKEN}:$DEST_TOKEN")
  printf '%s' "$DEST_TOKEN" | skopeo login --authfile "$auth/dest.json" \
    --username "$DEST_USERNAME" --password-stdin "$DEST_REGISTRY" >/dev/null
  export REGISTRY_AUTH_FILE="$auth/dest.json"  # cosign reads the same file
fi
inspect_src() { skopeo inspect --raw "${src_creds[@]/--src-creds/--creds}" "docker://$1"; }
inspect_dst() { skopeo inspect --raw "${dst_creds[@]/--dest-creds/--creds}" "docker://$1"; }

copied=0 skipped=0 missing=0 failed=0
declare -A DONE=()

copy_one() {  # $1 source ref, $2 dest ref
  [[ -z ${DONE[$2]:-} ]] || return 0
  DONE[$2]=1
  if [[ $2 == *@sha256:* ]] && inspect_dst "$2" >/dev/null 2>&1; then
    plan "present        $2"; skipped=$((skipped + 1)); return 0
  fi
  if ! inspect_src "$1" >/dev/null 2>&1; then
    plan "missing        $1"; missing=$((missing + 1)); return 0
  fi
  if skopeo copy --all --preserve-digests --retry-times 3 "${src_creds[@]}" "${dst_creds[@]}" \
       "docker://$1" "docker://$2" >/dev/null; then
    plan "copied         $1 -> $2"; copied=$((copied + 1))
  else
    plan "FAILED         $1 -> $2"; failed=$((failed + 1))
  fi
}

copy_cosign_tags() {  # $1 source repo, $2 dest repo, $3 digest
  local hex=${3#sha256:} suffix child
  for suffix in sig att sbom; do
    if inspect_src "$1:sha256-$hex.$suffix" >/dev/null 2>&1; then
      copy_one "$1:sha256-$hex.$suffix" "$2:sha256-$hex.$suffix"
    fi
  done
  # Release indexes sign every native child as well as the index itself.
  while IFS= read -r child; do
    [[ -n $child ]] || continue
    copy_cosign_tags "$1" "$2" "$child"
  done < <(inspect_src "$1@$3" | jq -r '.manifests[]?.digest // empty' 2>/dev/null)
}

declare -a RESIGN_TARGETS=()
for row in "${ROWS[@]}"; do
  read -r repository tag digest <<<"$row"
  source=$(old_repository "$repository")
  dest="$DEST_REGISTRY/$repository"
  if [[ $ALL_TAGS == 1 && $tag == - && $digest == - ]]; then
    while IFS= read -r each; do
      copy_one "$source:$each" "$dest:$each"
      if is_signed_repository "$dest" && [[ $each != sha256-* ]] \
         && resolved=$(skopeo inspect --format '{{.Digest}}' \
              "${src_creds[@]/--src-creds/--creds}" "docker://$source:$each" 2>/dev/null); then
        RESIGN_TARGETS+=("$dest@$resolved")
      fi
    done < <(skopeo list-tags "${src_creds[@]/--src-creds/--creds}" "docker://$source" | jq -r '.Tags[]')
    continue
  fi
  if [[ $digest != - ]]; then
    copy_one "$source@$digest" "$dest@$digest"
    copy_cosign_tags "$source" "$dest" "$digest"
    is_signed_repository "$dest" && RESIGN_TARGETS+=("$dest@$digest")
  fi
  if [[ $tag != - ]]; then
    # Tags move; copy what the tag names now and keep it named the same.
    copy_one "$source:$tag" "$dest:$tag"
    if is_signed_repository "$dest" && resolved=$(skopeo inspect --format '{{.Digest}}' \
         "${src_creds[@]/--src-creds/--creds}" "docker://$source:$tag" 2>/dev/null); then
      copy_cosign_tags "$source" "$dest" "$resolved"
      RESIGN_TARGETS+=("$dest@$resolved")
    fi
  fi
done

if [[ $RESIGN == 1 ]]; then
  for target in $(printf '%s\n' "${RESIGN_TARGETS[@]}" | sort -u); do
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
