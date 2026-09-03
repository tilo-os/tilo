#!/usr/bin/env bash
# Advance the landing page only after every native artifact and package-manager
# promotion gate has succeeded.
set -euo pipefail

: "${GITHUB_SERVER_URL:?missing Forgejo server URL}"
: "${GITHUB_REF_NAME:?missing release tag}"
: "${QOS_SITE_GIT_TOKEN:?missing short-lived website token}"
: "${QOS_LINUX_SIGNING_FINGERPRINT:?missing protected Linux signing fingerprint}"

version="${GITHUB_REF_NAME#imager-v}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || { printf 'release tag must be imager-vMAJOR.MINOR.PATCH\n' >&2; exit 2; }
fingerprint="${QOS_LINUX_SIGNING_FINGERPRINT^^}"
fingerprint="${fingerprint//[[:space:]]/}"
[[ "$fingerprint" =~ ^([A-F0-9]{40}|[A-F0-9]{64})$ ]] \
  || { printf 'Linux signing fingerprint must be a full OpenPGP fingerprint\n' >&2; exit 2; }

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
contract="$work/site/src/data/install-contract.json"
test -f "$manifest"
# A freshly deployed landing page deliberately starts unpublished with a null
# tag. Treat that as "no previous release" instead of making the first real
# promotion impossible.
current_tag="$(jq -r '.tag // empty' "$manifest")"
test -f "$contract"
current_version="${current_tag#imager-v}"
if [[ "$current_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
   && [ "$(printf '%s\n%s\n' "$current_version" "$version" | sort -V | tail -1)" != "$version" ]; then
  printf 'refusing to replace newer landing-page release %s with %s\n' "$current_version" "$version" >&2
  exit 1
fi
jq --arg tag "$GITHUB_REF_NAME" --arg fingerprint "$fingerprint" \
  '.tag = $tag | .published = true | .linuxSigningFingerprint = $fingerprint' \
  "$manifest" >"$manifest.next"
mv "$manifest.next" "$manifest"
cp docs/install-contract.json "$contract"

npm --prefix "$work/site" ci
npm --prefix "$work/site" run test:release
npm --prefix "$work/site" run build

git -C "$work/site" config user.name "qOS release automation"
git -C "$work/site" config user.email "release-bot@qos.dev"
git -C "$work/site" add src/data/imager-release.json src/data/install-contract.json
if git -C "$work/site" diff --cached --quiet; then
  printf 'landing page already targets %s\n' "$GITHUB_REF_NAME"
  exit 0
fi
git -C "$work/site" commit -m "Publish qOS Imager $version"
GIT_ASKPASS="$askpass" GIT_TERMINAL_PROMPT=0 \
  git -C "$work/site" push origin HEAD:main
