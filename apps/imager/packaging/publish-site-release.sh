#!/usr/bin/env bash
# Advance the landing page only after every native artifact and package-manager
# promotion gate has succeeded.
set -euo pipefail

: "${GITHUB_SERVER_URL:?missing Forgejo server URL}"
: "${GITHUB_REF_NAME:?missing release tag}"
: "${TILO_SITE_GIT_TOKEN:?missing short-lived website token}"
: "${TILO_LINUX_SIGNING_FINGERPRINT:?missing protected Linux signing fingerprint}"

release_server="${TILO_RELEASE_SERVER:-https://github.com}"
release_repository="${TILO_RELEASE_REPOSITORY:-tilo-os/releases}"
[[ "$release_repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
  || { printf 'release repository must be OWNER/REPOSITORY\n' >&2; exit 2; }
release_root="${release_server%/}/${release_repository}/releases"

version="${GITHUB_REF_NAME#imager-v}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || { printf 'release tag must be imager-vMAJOR.MINOR.PATCH\n' >&2; exit 2; }
fingerprint="${TILO_LINUX_SIGNING_FINGERPRINT^^}"
fingerprint="${fingerprint//[[:space:]]/}"
[[ "$fingerprint" =~ ^([A-F0-9]{40}|[A-F0-9]{64})$ ]] \
  || { printf 'Linux signing fingerprint must be a full OpenPGP fingerprint\n' >&2; exit 2; }

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

GIT_ASKPASS="$askpass" GIT_TERMINAL_PROMPT=0 git clone \
  "$GITHUB_SERVER_URL/qos/site.git" "$work/site"
manifest="$work/site/src/data/imager-release.json"
contract="$work/site/src/data/install-contract.json"
test -f "$manifest"
# A freshly deployed landing page deliberately starts unpublished with a null
# tag. Treat that as "no previous release" instead of making the first real
# promotion impossible.
current_tag="$(jq -r '.tag // empty' "$manifest")"
test -f "$contract"
version_is_newer() {
  LC_ALL=C awk -v current="$1" -v candidate="$2" '
    function canonical(value) {
      sub(/^0+/, "", value)
      return value == "" ? "0" : value
    }
    BEGIN {
      split(current, old, ".")
      split(candidate, new, ".")
      for (i = 1; i <= 3; i++) {
        left = canonical(old[i])
        right = canonical(new[i])
        if (length(left) != length(right))
          exit length(left) > length(right) ? 0 : 1
        if (left != right)
          exit ("x" left) > ("x" right) ? 0 : 1
      }
      exit 1
    }
  '
}
if [ "$(jq -r '.published' "$manifest")" = true ]; then
  [[ "$current_tag" =~ ^imager-v[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || { printf 'existing landing-page release has invalid tag: %s\n' "$current_tag" >&2; exit 1; }
  current_version="${current_tag#imager-v}"
  if version_is_newer "$current_version" "$version"; then
    printf 'refusing to replace newer landing-page release %s with %s\n' "$current_version" "$version" >&2
    exit 1
  fi
fi

# Opening a WinGet submission makes the release reviewable, not installable.
# Probe the exact version manifest in Microsoft's accepted index so the landing
# page exposes the command only when `winget install` can really resolve it.
# A transient GitHub failure stays fail-safe as "submitted" and a later rerun
# of this idempotent promotion can advance the same release to "available".
winget_state=submitted
winget_manifest="$work/Tilo.Imager.yaml"
winget_url="https://raw.githubusercontent.com/microsoft/winget-pkgs/master/manifests/q/Tilo/Imager/${version}/Tilo.Imager.yaml"
winget_http="$(curl --silent --show-error --location \
    --retry 3 --retry-all-errors --retry-max-time 45 \
    --connect-timeout 10 --max-time 20 \
    --output "$winget_manifest" --write-out '%{http_code}' "$winget_url" || true)"
if [ "$winget_http" = 200 ] \
  && grep -Fxq 'PackageIdentifier: Tilo.Imager' "$winget_manifest" \
  && grep -Fxq "PackageVersion: $version" "$winget_manifest"; then
  winget_state=available
fi

jq --arg tag "$GITHUB_REF_NAME" --arg fingerprint "$fingerprint" \
   --arg release_root "$release_root" \
   --arg winget_state "$winget_state" \
  '.tag = $tag
   | .published = true
   | .releaseRoot = $release_root
   | .linuxSigningFingerprint = $fingerprint
   | .channels.homebrew = "available"
   | .channels.winget = $winget_state' \
  "$manifest" >"$manifest.next"
mv "$manifest.next" "$manifest"
cp docs/install-contract.json "$contract"

npm --prefix "$work/site" ci
npm --prefix "$work/site" run test:release
npm --prefix "$work/site" run build

# The manifest is the release-origin contract. Refuse to publish a manifest a
# site build ignores: that would turn every attractive download action into an
# authentication-gated or dead link even though its own tests passed.
expected_download="$release_root/download/$GITHUB_REF_NAME/$(jq -r '.assets.windowsX64Setup' "$manifest")"
grep -R -Fq -- "href=\"$expected_download\"" "$work/site/dist" \
  || { printf 'landing page does not consume the public releaseRoot manifest field\n' >&2; exit 1; }
for asset_key in macosX64Dmg macosX64Portable macosArm64Dmg macosArm64Portable; do
  asset="$(jq -r --arg key "$asset_key" '.assets[$key] // empty' "$manifest")"
  [ -n "$asset" ] \
    || { printf 'landing page manifest is missing required macOS asset %s\n' "$asset_key" >&2; exit 1; }
  expected_download="$release_root/download/$GITHUB_REF_NAME/$asset"
  grep -R -Fq -- "href=\"$expected_download\"" "$work/site/dist" \
    || { printf 'landing page does not expose required macOS asset %s\n' "$asset_key" >&2; exit 1; }
done
if grep -R -Fq -- 'https://code.sandbox.example/qos/qos/releases/download/' "$work/site/dist" \
   || grep -R -Fq -- 'https://code.sandbox.example/tilo/releases/download/' "$work/site/dist"; then
  printf 'landing page still contains private-source release downloads\n' >&2
  exit 1
fi

git -C "$work/site" config user.name "Tilo release automation"
git -C "$work/site" config user.email "release-bot@tilo.dev"
git -C "$work/site" add src/data/imager-release.json src/data/install-contract.json
if git -C "$work/site" diff --cached --quiet; then
  printf 'landing page already targets %s\n' "$GITHUB_REF_NAME"
  exit 0
fi
git -C "$work/site" commit -m "Publish Tilo Imager $version"
GIT_ASKPASS="$askpass" GIT_TERMINAL_PROMPT=0 \
  git -C "$work/site" push origin HEAD:main
