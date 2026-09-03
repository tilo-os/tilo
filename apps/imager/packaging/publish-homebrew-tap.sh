#!/usr/bin/env bash
# Create/update the first-party qos/homebrew-qos tap from immutable release assets.
set -euo pipefail

: "${GITHUB_SERVER_URL:?missing Forgejo server URL}"
: "${GITHUB_REF_NAME:?missing release tag}"
: "${HOMEBREW_TAP_TOKEN:?missing protected tap token}"
command -v brew >/dev/null 2>&1 \
  || { printf 'Homebrew is required to audit the tap before publication\n' >&2; exit 1; }

assets="${1:-release-assets}"
version="${GITHUB_REF_NAME#imager-v}"
[ "$version" != "$GITHUB_REF_NAME" ] || { printf 'expected imager-v* tag\n' >&2; exit 2; }
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || { printf 'release tag must be imager-vMAJOR.MINOR.PATCH\n' >&2; exit 2; }
base_url="${GITHUB_SERVER_URL}/qos/qos/releases/download/${GITHUB_REF_NAME}"

digest() {
  local name="$1" path
  path="$(find "$assets" -type f -name "$name" -print -quit)"
  [ -n "$path" ] || { printf 'missing release asset %s\n' "$name" >&2; exit 1; }
  shasum -a 256 "$path" | awk '{print $1}'
}

linux_x64="$(digest qos-imager-linux-x86_64.tar.gz)"
linux_arm="$(digest qos-imager-linux-aarch64.tar.gz)"
mac_x64="$(digest qos-imager-macos-x86_64.dmg)"
mac_arm="$(digest qos-imager-macos-aarch64.dmg)"

api="${GITHUB_SERVER_URL}/api/v1"
auth="Authorization: token ${HOMEBREW_TAP_TOKEN}"
if ! curl --fail --silent -H "$auth" "$api/repos/qos/homebrew-qos" >/dev/null; then
  curl --fail --silent --show-error -H "$auth" -H 'Content-Type: application/json' \
    --data '{"name":"homebrew-qos","description":"Official qOS Homebrew tap","private":false,"auto_init":true,"default_branch":"main"}' \
    "$api/orgs/qos/repos" >/dev/null
fi

work="$(mktemp -d)"
audit_tapped=0
cleanup() {
  if [ "$audit_tapped" = 1 ]; then
    HOMEBREW_NO_AUTO_UPDATE=1 brew untap qos/release-audit >/dev/null 2>&1 || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT
askpass="$work/askpass.sh"
printf '#!/bin/sh\nprintf "%%s" "$HOMEBREW_TAP_TOKEN"\n' > "$askpass"
chmod 0700 "$askpass"
GIT_ASKPASS="$askpass" GIT_TERMINAL_PROMPT=0 git clone \
  "${GITHUB_SERVER_URL}/qos/homebrew-qos.git" "$work/tap"
current_version="$(sed -n 's/^[[:space:]]*version "\([0-9][0-9.]*\)"/\1/p' \
  "$work/tap/Formula/qos-imager.rb" 2>/dev/null | head -1)"
if [ -n "$current_version" ] \
   && [ "$(printf '%s\n%s\n' "$current_version" "$version" | sort -V | tail -1)" != "$version" ]; then
  printf 'refusing to replace newer Homebrew release %s with %s\n' "$current_version" "$version" >&2
  exit 1
fi
mkdir -p "$work/tap/Formula" "$work/tap/Casks"

render() {
  local input="$1" output="$2"
  sed \
    -e "s|@VERSION@|$version|g" \
    -e "s|@BASE_URL@|$base_url|g" \
    -e "s|@LINUX_X86_64_SHA256@|$linux_x64|g" \
    -e "s|@LINUX_ARM64_SHA256@|$linux_arm|g" \
    -e "s|@MACOS_X86_64_SHA256@|$mac_x64|g" \
    -e "s|@MACOS_ARM64_SHA256@|$mac_arm|g" \
    "$input" > "$output"
}
render apps/imager/packaging/homebrew/qos-imager.rb.in "$work/tap/Formula/qos-imager.rb"
render apps/imager/packaging/homebrew/qos-imager-cask.rb.in "$work/tap/Casks/qos-imager.rb"

if grep -REn '@[A-Z0-9_]+@' "$work/tap/Formula" "$work/tap/Casks"; then
  printf 'unresolved Homebrew template placeholder\n' >&2
  exit 1
fi
ruby -c "$work/tap/Formula/qos-imager.rb"
ruby -c "$work/tap/Casks/qos-imager.rb"
# Modern Homebrew audits names, not arbitrary paths. Tap the unpushed local
# commit under a disposable name so the exact files about to be pushed are
# audited without depending on stale remote state.
HOMEBREW_NO_AUTO_UPDATE=1 brew tap qos/release-audit "$work/tap"
audit_tapped=1
HOMEBREW_NO_AUTO_UPDATE=1 brew audit --strict --formula qos/release-audit/qos-imager
HOMEBREW_NO_AUTO_UPDATE=1 brew audit --strict --cask qos/release-audit/qos-imager
HOMEBREW_NO_AUTO_UPDATE=1 brew untap qos/release-audit >/dev/null
audit_tapped=0

if [ ! -f "$work/tap/README.md" ]; then
  printf '# qOS Homebrew tap\n\n```sh\nbrew tap qos/qos %s/qos/homebrew-qos.git\nbrew install qos-imager\n# macOS GUI:\nbrew install --cask qos-imager\n```\n' \
    "$GITHUB_SERVER_URL" > "$work/tap/README.md"
fi
git -C "$work/tap" config user.name "qOS release automation"
git -C "$work/tap" config user.email "release-bot@qos.dev"
git -C "$work/tap" add Formula Casks README.md
if git -C "$work/tap" diff --cached --quiet; then
  printf 'Homebrew tap already contains qOS Imager %s\n' "$version"
  exit 0
fi
git -C "$work/tap" commit -m "qos-imager $version"
GIT_ASKPASS="$askpass" GIT_TERMINAL_PROMPT=0 git -C "$work/tap" push origin HEAD:main
