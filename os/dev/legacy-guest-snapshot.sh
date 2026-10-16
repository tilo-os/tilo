#!/usr/bin/env bash
# Steps of .forgejo/workflows/build-legacy-guest.yml that touch the pin.
#
#   legacy-guest-snapshot.sh record REF   verify the signature, write refs/x86_64.env
#   legacy-guest-snapshot.sh promote      propose os/legacy-guest-snapshot.env
#                                         through a pull request to protected main
#
# promote reads refs/**/x86_64.env (the build job's artifact), verifies the
# release signature again, refuses when the guest's inputs changed on main since
# the run started, and opens the pin PR with os/dev/open-snapshot-pr.py.
set -euo pipefail

die() { echo "::error::$*" >&2; exit 1; }

readonly IMAGE=registry.tilo.so/tilo/legacy-guest
readonly PINS=os/legacy-guest-snapshot.env
# Everything that ends up in the guest disk.
readonly INPUTS=(
  os/Containerfile.legacy-guest
  os/legacy-guest
  os/base-snapshot.env
  crates/runtime/legacy-agent
  crates/runtime/legacy-agent-protocol
)

valid_ref() {
  [[ $1 =~ ^${IMAGE//./\\.}@sha256:[0-9a-f]{64}$ ]]
}

record() {
  local ref=$1
  valid_ref "$ref" || die "not an immutable legacy guest reference: $ref"
  cosign verify --key keys/cosign.pub "$ref" >/dev/null
  mkdir -p refs
  printf 'LEGACY_GUEST_X86_64_REF=%s\n' "$ref" > refs/x86_64.env
}

promote() {
  local env ref digest branch remote_tip
  env="$(find refs -type f -name x86_64.env -print -quit)"
  test -n "$env" || die "no recorded legacy guest reference"
  ref="$(sed -n 's/^LEGACY_GUEST_X86_64_REF=//p' "$env")"
  valid_ref "$ref" || die "not an immutable legacy guest reference: $ref"
  cosign verify --key keys/cosign.pub "$ref" >/dev/null
  git fetch --no-tags origin main
  if ! git diff --quiet "$GITHUB_SHA..origin/main" -- "${INPUTS[@]}"; then
    die "legacy guest inputs changed after this run started; refusing stale promotion"
  fi
  git checkout --detach origin/main
  python3 os/dev/open-snapshot-pr.py pins legacy-guest --x86-ref "$ref" > "$PINS"
  if git diff --quiet -- "$PINS"; then
    echo "the legacy guest pin is already current"
    return 0
  fi
  git add "$PINS"
  digest="$(sha256sum "$PINS" | awk '{print $1}')"
  git -c user.name='Tilo CI' -c user.email='ci@tilo.invalid' \
    commit -m "build(os): pin the Linux legacy VM guest image"
  branch="automation/legacy-guest-snapshot-${digest:0:16}"
  remote_tip="$(git ls-remote --heads origin "refs/heads/$branch" | awk '{print $1}')"
  git push --force-with-lease="refs/heads/$branch:$remote_tip" origin "HEAD:refs/heads/$branch"
  python3 os/dev/open-snapshot-pr.py \
    --server "$GITHUB_SERVER_URL" --repository "$GITHUB_REPOSITORY" \
    --branch "$branch" --snapshot-digest "$digest" \
    legacy-guest --x86-ref "$ref" | tee -a "${GITHUB_STEP_SUMMARY:-/dev/null}"
}

case "${1:-}" in
  record) [ $# -eq 2 ] || die "usage: record REF"; record "$2" ;;
  promote) [ $# -eq 1 ] || die "usage: promote"; promote ;;
  *) die "usage: record REF | promote" ;;
esac
