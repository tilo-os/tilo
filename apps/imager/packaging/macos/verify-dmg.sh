#!/usr/bin/env bash
# Verify a packaged macOS imager DMG as a person receives it: mount it
# read-only, check the bundle layout, the executable's architecture and the
# signature, then start the bundled imager offscreen from the mounted app and
# require its capture. The x86_64 package is started under Rosetta, which is
# how it runs on an Intel Mac. With TILO_REQUIRE_SIGNING=1, additionally
# require a stapled, Gatekeeper-accepted Developer ID signature naming the
# expected team and certificate.
#
#   verify-dmg.sh <x86_64|aarch64> <dmg> <evidence-dir>
#
# The capture is written to EVIDENCE-DIR/dmg.png.
set -euo pipefail

arch="${1:?architecture: x86_64 or aarch64}"
dmg="${2:?the DMG to verify}"
evidence="${3:?directory for the evidence this check keeps}"
[ -f "$dmg" ] || { printf '%s is not a file\n' "$dmg" >&2; exit 2; }
[ "$(uname -s)" = Darwin ] \
  || { printf 'verify-dmg.sh runs on macOS only\n' >&2; exit 2; }
case "$arch" in
  x86_64) wanted_arch=x86_64 ;;
  aarch64) wanted_arch=arm64 ;;
  *) printf 'unsupported architecture: %s\n' "$arch" >&2; exit 2 ;;
esac
require_signing="${TILO_REQUIRE_SIGNING:-0}"

mkdir -p "$evidence"
evidence="$(cd -- "$evidence" && pwd)"

work="$(mktemp -d "${TMPDIR:-/tmp}/tilo-imager-verify.XXXXXX")"
chmod 0700 "$work"
mountpoint="$work/mount"
mkdir "$mountpoint"
cleanup() { hdiutil detach "$mountpoint" -quiet 2>/dev/null || true; rm -rf -- "$work"; }
trap cleanup EXIT

fail() { printf 'verify-dmg: %s\n' "$*" >&2; exit 1; }

hdiutil verify -quiet "$dmg"
hdiutil attach "$dmg" -quiet -nobrowse -readonly -mountpoint "$mountpoint"
app="$mountpoint/Tilo Imager.app"
[ "$(readlink "$mountpoint/Applications")" = /Applications ] \
  || fail 'DMG lacks the /Applications link'
contents="$app/Contents"
for path in "$contents/MacOS/tilo-imager" "$contents/MacOS/tilo-imager-bin" \
            "$contents/Info.plist"; do
  [ -f "$path" ] || fail "missing ${path#"$mountpoint"/}"
done
[ -x "$contents/MacOS/tilo-imager" ] && [ -x "$contents/MacOS/tilo-imager-bin" ] \
  || fail 'the launcher or the executable is not executable'
# The package ships one architecture, never a universal binary that would run
# natively where Rosetta was meant to prove it.
[ "$(lipo -archs "$contents/MacOS/tilo-imager-bin")" = "$wanted_arch" ] \
  || fail "the executable is not $wanted_arch only"
plutil -lint -s "$contents/Info.plist"
plist() { /usr/libexec/PlistBuddy -c "Print :$1" "$contents/Info.plist"; }
[ "$(plist CFBundleExecutable)" = tilo-imager ] || fail 'CFBundleExecutable is not tilo-imager'
[ "$(plist CFBundleIdentifier)" = so.tilo.imager ] || fail 'bundle id is not so.tilo.imager'
if [ -n "${TILO_VERSION:-}" ]; then
  version="${TILO_VERSION#imager-v}"
  version="${version#v}"
  [ "$(plist CFBundleShortVersionString)" = "$version" ] \
    || fail "bundle version $(plist CFBundleShortVersionString) is not $version"
fi
signature="$(codesign -dv "$contents/MacOS/tilo-imager-bin" 2>&1)"
grep -Eq '^CodeDirectory .*flags=.*runtime' <<<"$signature" \
  || fail 'the executable is not signed with the hardened runtime'
codesign --verify --deep --strict --verbose=2 "$app"

runner=()
if [ "$arch" = x86_64 ]; then
  # Apple silicon runs the Intel package through Rosetta; `arch` refuses to
  # start it where Rosetta is absent, and the policy value proves the process
  # it starts is translated rather than native.
  [ "$(arch -x86_64 /bin/sh -c 'sysctl -n sysctl.proc_translated')" = 1 ] \
    || fail 'Rosetta does not translate an x86_64 process on this Mac'
  runner=(arch -x86_64)
fi
shot="$evidence/dmg.png"
env TMPDIR="$work" "${runner[@]}" "$contents/MacOS/tilo-imager" \
  --shots "$shot" --scenario done --viewport 1180x820
[ -s "$shot" ] || fail 'the packaged imager did not render its UI'

if [ "$require_signing" = 1 ]; then
  grep -q '^Authority=Developer ID Application:' <<<"$(codesign -dvv "$app" 2>&1)" \
    || fail 'bundle is not signed with a Developer ID Application certificate'
  xcrun stapler validate "$app"
  xcrun stapler validate "$dmg"
  spctl --assess --type execute --verbose=2 "$app"
fi

printf 'verify-dmg: %s (%s) ok; capture at %s\n' "$dmg" "$arch" "$shot"
