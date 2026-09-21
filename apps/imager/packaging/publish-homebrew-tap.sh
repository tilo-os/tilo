#!/usr/bin/env bash
# Update the first-party tilo-os/homebrew-tilo tap from immutable release assets.
set -euo pipefail

: "${GITHUB_REF_NAME:?missing release tag}"
: "${HOMEBREW_TAP_TOKEN:?missing protected tap token}"
command -v brew >/dev/null 2>&1 \
  || { printf 'Homebrew is required to audit the tap before publication\n' >&2; exit 1; }

assets="${1:-release-assets}"
version="${GITHUB_REF_NAME#imager-v}"
[ "$version" != "$GITHUB_REF_NAME" ] || { printf 'expected imager-v* tag\n' >&2; exit 2; }
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || { printf 'release tag must be imager-vMAJOR.MINOR.PATCH\n' >&2; exit 2; }
release_server="${TILO_RELEASE_SERVER:-https://github.com}"
release_repository="${TILO_RELEASE_REPOSITORY:-tilo-os/releases}"
tap_repository="${TILO_HOMEBREW_TAP_REPOSITORY:-tilo-os/homebrew-tilo}"
[[ "$release_repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
  || { printf 'release repository must be OWNER/REPOSITORY\n' >&2; exit 2; }
[[ "$tap_repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
  || { printf 'Homebrew tap repository must be OWNER/REPOSITORY\n' >&2; exit 2; }
base_url="${release_server%/}/${release_repository}/releases/download/${GITHUB_REF_NAME}"
release_page="${release_server%/}/${release_repository}"

digest() {
  local name="$1" path
  path="$(find "$assets" -type f -name "$name" -print -quit)"
  [ -n "$path" ] || { printf 'missing release asset %s\n' "$name" >&2; exit 1; }
  shasum -a 256 "$path" | awk '{print $1}'
}

linux_x64="$(digest tilo-imager-linux-x86_64.tar.gz)"
linux_arm="$(digest tilo-imager-linux-aarch64.tar.gz)"
macos_enabled="${TILO_IMAGER_MACOS_ENABLED:-0}"
case "$macos_enabled" in 0|1) ;; *) printf 'TILO_IMAGER_MACOS_ENABLED must be 0 or 1\n' >&2; exit 2 ;; esac
mac_x64= mac_arm=
if [ "$macos_enabled" = 1 ]; then
  mac_x64="$(digest tilo-imager-macos-x86_64.dmg)"
  mac_arm="$(digest tilo-imager-macos-aarch64.dmg)"
fi

GITHUB_INSTALLATION_TOKEN="$HOMEBREW_TAP_TOKEN" \
  "$(dirname "$0")/assert-github-installation-token.sh" "$tap_repository"

work="$(mktemp -d)"
audit_tapped=0
cleanup() {
  if [ "$audit_tapped" = 1 ]; then
    HOMEBREW_NO_AUTO_UPDATE=1 brew untap tilo/release-audit >/dev/null 2>&1 || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT
askpass="$work/askpass.sh"
cat >"$askpass" <<'SH'
#!/bin/sh
case "$1" in
  *Username*) printf '%s' x-access-token ;;
  *) printf '%s' "$HOMEBREW_TAP_TOKEN" ;;
esac
SH
chmod 0700 "$askpass"
GIT_ASKPASS="$askpass" GIT_TERMINAL_PROMPT=0 git clone \
  "${release_server%/}/${tap_repository}.git" "$work/tap"
formula="$work/tap/Formula/tilo-imager.rb"
current_version=
if [ -f "$formula" ]; then
  current_version="$(sed -n 's/^[[:space:]]*version "\([^"]*\)".*/\1/p' \
    "$formula" | head -1)"
  [ -n "$current_version" ] \
    || { printf 'existing Homebrew formula has no parseable version\n' >&2; exit 1; }
fi
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
if [ -n "$current_version" ]; then
  [[ "$current_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || { printf 'existing Homebrew release has an invalid version: %s\n' "$current_version" >&2; exit 1; }
  if version_is_newer "$current_version" "$version"; then
    printf 'refusing to replace newer Homebrew release %s with %s\n' "$current_version" "$version" >&2
    exit 1
  fi
fi
mkdir -p "$work/tap/Formula" "$work/tap/Casks"

render() {
  local input="$1" output="$2"
  sed \
    -e "s|@VERSION@|$version|g" \
    -e "s|@BASE_URL@|$base_url|g" \
    -e "s|@RELEASE_PAGE@|$release_page|g" \
    -e "s|@LINUX_X86_64_SHA256@|$linux_x64|g" \
    -e "s|@LINUX_ARM64_SHA256@|$linux_arm|g" \
    -e "s|@MACOS_X86_64_SHA256@|$mac_x64|g" \
    -e "s|@MACOS_ARM64_SHA256@|$mac_arm|g" \
    "$input" > "$output"
}
render apps/imager/packaging/homebrew/tilo-imager.rb.in "$work/tap/Formula/tilo-imager.rb"
if [ "$macos_enabled" = 1 ]; then
  render apps/imager/packaging/homebrew/tilo-imager-cask.rb.in "$work/tap/Casks/tilo-imager.rb"
fi

if grep -REn '@[A-Z0-9_]+@' "$work/tap/Formula" "$work/tap/Casks"; then
  printf 'unresolved Homebrew template placeholder\n' >&2
  exit 1
fi
ruby -c "$work/tap/Formula/tilo-imager.rb"
if [ "$macos_enabled" = 1 ]; then
  ruby -c "$work/tap/Casks/tilo-imager.rb"
fi
# Modern Homebrew audits names, not arbitrary paths. Tap the unpushed local
# commit under a disposable name so the exact files about to be pushed are
# audited without depending on stale remote state.
HOMEBREW_NO_AUTO_UPDATE=1 brew tap tilo/release-audit "$work/tap"
audit_tapped=1
HOMEBREW_NO_AUTO_UPDATE=1 brew audit --strict --formula tilo/release-audit/tilo-imager
if [ "$macos_enabled" = 1 ]; then
  HOMEBREW_NO_AUTO_UPDATE=1 brew audit --strict --cask tilo/release-audit/tilo-imager
fi
HOMEBREW_NO_AUTO_UPDATE=1 brew untap tilo/release-audit >/dev/null
audit_tapped=0

if [ ! -f "$work/tap/README.md" ]; then
  printf '# Tilo Homebrew tap\n\n```sh\nbrew tap tilo/tilo %s/%s.git\nbrew install tilo-imager\n```\n' \
    "$release_server" "$tap_repository" > "$work/tap/README.md"
  if [ "$macos_enabled" = 1 ]; then
    printf '\nmacOS GUI:\n```sh\nbrew install --cask tilo-imager\n```\n' >> "$work/tap/README.md"
  fi
fi
git -C "$work/tap" config user.name "Tilo release automation"
git -C "$work/tap" config user.email "release-bot@tilo.dev"
git -C "$work/tap" add Formula Casks README.md
if git -C "$work/tap" diff --cached --quiet; then
  printf 'Homebrew tap already contains Tilo Imager %s\n' "$version"
  exit 0
fi
git -C "$work/tap" commit -m "tilo-imager $version"
GIT_ASKPASS="$askpass" GIT_TERMINAL_PROMPT=0 git -C "$work/tap" push origin HEAD:main
