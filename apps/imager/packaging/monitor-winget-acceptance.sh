#!/usr/bin/env bash
# Reconcile an already-published landing-page release after Microsoft accepts
# its exact WinGet manifest. This never selects or publishes a new release.
set -euo pipefail

: "${GITHUB_SERVER_URL:?missing Forgejo server URL}"
: "${TILO_SITE_GIT_TOKEN:?missing short-lived website token}"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
askpass="$work/askpass.sh"
cat >"$askpass" <<'SH'
#!/bin/sh
case "$1" in
  *Username*) printf '%s' "${TILO_SITE_GIT_USER:-oauth2}" ;;
  *) printf '%s' "$TILO_SITE_GIT_TOKEN" ;;
esac
SH
chmod 0700 "$askpass"

GIT_ASKPASS="$askpass" GIT_TERMINAL_PROMPT=0 git clone --depth 1 \
  "$GITHUB_SERVER_URL/tilo/site.git" "$work/site"
manifest="$work/site/src/data/imager-release.json"
test -f "$manifest"

published="$(jq -r '.published' "$manifest")"
tag="$(jq -r '.tag // empty' "$manifest")"
state="$(jq -r '.channels.winget // empty' "$manifest")"
if [ "$published" != true ]; then
  printf 'landing page has no published Tilo Imager release; nothing to reconcile\n'
  exit 0
fi
[[ "$tag" =~ ^imager-v[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || { printf 'published landing-page release has invalid tag: %s\n' "$tag" >&2; exit 1; }
case "$state" in
  available)
    printf 'WinGet is already available for %s\n' "$tag"
    exit 0
    ;;
  submitted) ;;
  *)
    printf 'published landing-page release has invalid WinGet state: %s\n' "$state" >&2
    exit 1
    ;;
esac

# publish-site-release.sh rechecks the exact accepted Microsoft manifest,
# preserves a fail-safe submitted state on transient failure, runs the website
# release contract/build, and pushes only when the state actually advances.
GITHUB_REF_NAME="$tag" apps/imager/packaging/publish-site-release.sh
