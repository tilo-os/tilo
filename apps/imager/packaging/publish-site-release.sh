#!/usr/bin/env bash
# Advance the landing page only after every native artifact and package-manager
# promotion gate has succeeded.
set -euo pipefail

: "${GITHUB_SERVER_URL:?missing Forgejo server URL}"
: "${GITHUB_REF_NAME:?missing release tag}"
: "${QOS_SITE_GIT_TOKEN:?missing short-lived website token}"

version="${GITHUB_REF_NAME#imager-v}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || { printf 'release tag must be imager-vMAJOR.MINOR.PATCH\n' >&2; exit 2; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
askpass="$work/askpass.sh"
cat >"$askpass" <<'SH'
#!/bin/sh
case "$1" in
  *Username*) printf '%s' "${QOS_SITE_GIT_USER:-oauth2}" ;;
  *) printf '%s' "$QOS_SITE_GIT_TOKEN" ;;
esac
SH
chmod 0700 "$askpass"

GIT_ASKPASS="$askpass" GIT_TERMINAL_PROMPT=0 git clone \
  "$GITHUB_SERVER_URL/qos/website.git" "$work/site"
manifest="$work/site/src/data/imager-release.json"
test -f "$manifest"
jq --arg tag "$GITHUB_REF_NAME" '.tag = $tag' "$manifest" >"$manifest.next"
mv "$manifest.next" "$manifest"

npm --prefix "$work/site" ci
npm --prefix "$work/site" run test:release
npm --prefix "$work/site" run build

git -C "$work/site" config user.name "qOS release automation"
git -C "$work/site" config user.email "release-bot@qos.dev"
git -C "$work/site" add src/data/imager-release.json
if git -C "$work/site" diff --cached --quiet; then
  printf 'landing page already targets %s\n' "$GITHUB_REF_NAME"
  exit 0
fi
git -C "$work/site" commit -m "Publish qOS Imager $version"
GIT_ASKPASS="$askpass" GIT_TERMINAL_PROMPT=0 \
  git -C "$work/site" push origin HEAD:main
