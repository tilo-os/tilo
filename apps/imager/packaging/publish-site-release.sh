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

# The landing page names the Homebrew destination in the installation commands
# it prints. Derive it from the destination the tap publisher pushes to and the
# name it tells users to tap, so the page cannot advertise a tap the release
# never updated.
tap_repository="${TILO_HOMEBREW_TAP_REPOSITORY:-tilo-os/homebrew-tilo}"
[[ "$tap_repository" =~ ^[A-Za-z0-9_.-]+/homebrew-[A-Za-z0-9_.-]+$ ]] \
  || { printf 'Homebrew tap repository must be OWNER/homebrew-NAME\n' >&2; exit 2; }
tap_name="${TILO_HOMEBREW_TAP_NAME:-tilo/tilo}"
[[ "$tap_name" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
  || { printf 'Homebrew tap name must be OWNER/NAME\n' >&2; exit 2; }
tap_command="brew tap $tap_name ${release_server%/}/${tap_repository}.git"

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
  "$GITHUB_SERVER_URL/tilo/site.git" "$work/site"
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
# The contract carries the Homebrew destination the page prints, so it has to
# describe the tap this promotion actually updates.
contract_tap="$(jq -r '.imager.homebrew.tap.command // empty' "$contract")"
contract_tap_name="$(jq -r '.imager.homebrew.tap.name // empty' "$contract")"
contract_tap_repository="$(jq -r '.imager.homebrew.tap.repository // empty' "$contract")"
[ "$contract_tap_repository" = "$tap_repository" ] \
  || { printf 'install contract expects Homebrew tap %s but this release publishes %s\n' \
         "$contract_tap_repository" "$tap_repository" >&2; exit 1; }
[ "$contract_tap_name" = "$tap_name" ] \
  || { printf 'install contract expects Homebrew tap name %s but this release taps %s\n' \
         "$contract_tap_name" "$tap_name" >&2; exit 1; }
[ "$contract_tap" = "$tap_command" ] \
  || { printf 'install contract Homebrew tap command does not match the release tap\n' >&2; exit 1; }

npm --prefix "$work/site" ci
npm --prefix "$work/site" run test:release
npm --prefix "$work/site" run build

# The manifest is the release-origin contract. Refuse to publish a manifest a
# site build ignores: that would turn every attractive download action into an
# authentication-gated or dead link even though its own tests passed.
# Verify the actual rendered release surface independently of the site's own
# tests. A simultaneous site-test regression must not let promotion silently
# drop one supported architecture, format, trust anchor, or package-manager
# route while still publishing an attractive partial page.
for asset_key in \
  windowsX64Setup windowsX64Standalone windowsX64Portable \
  macosX64Dmg macosX64Portable macosArm64Dmg macosArm64Portable \
  linuxX64Flatpak linuxArm64Flatpak linuxX64AppImage linuxArm64AppImage \
  linuxX64Portable linuxArm64Portable releaseKey releaseFingerprint installerCatalog; do
  asset="$(jq -r --arg key "$asset_key" '.assets[$key] // empty' "$manifest")"
  [ -n "$asset" ] \
    || { printf 'landing page manifest is missing required release asset %s\n' "$asset_key" >&2; exit 1; }
  expected_download="$release_root/download/$GITHUB_REF_NAME/$asset"
  grep -R -Fq -- "href=\"$expected_download\"" "$work/site/dist" \
    || { printf 'landing page does not expose required release asset %s\n' "$asset_key" >&2; exit 1; }
done
for command in 'brew install --cask tilo-imager' 'brew install tilo-imager'; do
  grep -R -Fq -- "$command" "$work/site/dist" \
    || { printf 'landing page does not expose package-manager command: %s\n' "$command" >&2; exit 1; }
done
# A page that installs through a tap prints that tap. Require exactly the tap
# this promotion updated and refuse any other, so a stale destination in the
# site template cannot ship as the instructions people follow.
rendered_taps="$(grep -R -h -o -- 'brew tap [^<"]*' "$work/site/dist" | sort -u || true)"
[ "$rendered_taps" = "$tap_command" ] \
  || { printf 'landing page Homebrew tap does not match the release tap: %s\n' \
         "${rendered_taps:-none}" >&2; exit 1; }
if [ "$winget_state" = available ]; then
  grep -R -Fq -- 'winget install Tilo.Imager' "$work/site/dist" \
    || { printf 'landing page hides an accepted WinGet release\n' >&2; exit 1; }
else
  grep -R -Fq -- 'WinGet submission in review.' "$work/site/dist" \
    || { printf 'landing page does not explain the pending WinGet route\n' >&2; exit 1; }
fi
if grep -R -Fq -- 'https://code.tilo.so/tilo/tilo/releases/download/' "$work/site/dist" \
   || grep -R -Fq -- 'https://code.tilo.so/tilo/releases/download/' "$work/site/dist"; then
  printf 'landing page still contains private-source release downloads\n' >&2
  exit 1
fi

git -C "$work/site" config user.name "Tilo release automation"
git -C "$work/site" config user.email "release-bot@tilo.so"
git -C "$work/site" add src/data/imager-release.json src/data/install-contract.json
if git -C "$work/site" diff --cached --quiet; then
  printf 'landing page already targets %s\n' "$GITHUB_REF_NAME"
  exit 0
fi
git -C "$work/site" commit -m "Publish Tilo Imager $version"
GIT_ASKPASS="$askpass" GIT_TERMINAL_PROMPT=0 \
  git -C "$work/site" push origin HEAD:main
