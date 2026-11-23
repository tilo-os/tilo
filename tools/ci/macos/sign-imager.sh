#!/usr/bin/env bash
# The signing half of the macOS installer release: take the executable the
# build check produced and the package the package check made from it, refuse
# anything whose digest does not match, then package that same executable
# again with a Developer ID signature, notarize it and staple the ticket. The
# signed DMG is then verified as a person receives it, Gatekeeper included.
#
#   sign-imager.sh <arch> <build-dir> <package-dir> <out>
#
# Runs only on release tags; the version comes from the tag,
# imager-vMAJOR.MINOR.PATCH. The Developer ID certificate arrives as a stored
# secret, is imported into a keychain of its own for the length of the job,
# and that keychain is removed when the job ends. No key is kept on the Mac.
set -euo pipefail

arch="${1:?x86_64 or aarch64}"
build="${2:?build output directory}"
package="${3:?package output directory}"
out="${4:?output directory}"
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"

case "${T_REF:-}" in
  tag:imager-v*) TILO_VERSION="${T_REF#tag:imager-v}" ;;
  *) printf 'signing runs for imager release tags only, not %s\n' "${T_REF:-a working tree}" >&2; exit 1 ;;
esac
export TILO_VERSION

: "${TILO_MAC_SIGNING_P12:?the Developer ID certificate (base64 PKCS#12) is required}"
: "${TILO_MAC_SIGNING_P12_PASSWORD:?the password of the certificate file is required}"
if [ -z "${TILO_NOTARY_KEYCHAIN_PROFILE:-}" ]; then
  : "${TILO_APPLE_ID:?notarization needs TILO_APPLE_ID or TILO_NOTARY_KEYCHAIN_PROFILE}"
  : "${TILO_APPLE_TEAM_ID:?notarization needs TILO_APPLE_TEAM_ID}"
  : "${TILO_APPLE_APP_PASSWORD:?notarization needs TILO_APPLE_APP_PASSWORD}"
fi

# The build's sidecar names the digest it produced; the package records the
# digest of the executable it put in the app.
(cd "$build" && shasum -a 256 --check --strict imager.sha256 >/dev/null)
built="$(cut -d ' ' -f 1 "$build/imager.sha256")"
packaged="$(cut -d ' ' -f 1 "$package/tilo-imager-macos-$arch.bin.sha256")"
[ "$built" = "$packaged" ] \
  || { printf 'packaged executable %s is not the build %s\n' "$packaged" "$built" >&2; exit 1; }

# codesign reads the Developer ID certificate out of a keychain, so the secret
# becomes one for this job alone; the user's search list is put back as it was.
keychain_dir="$(mktemp -d "${TMPDIR:-/tmp}/tilo-signing.XXXXXX")"
chmod 0700 "$keychain_dir"
keychain="$keychain_dir/tilo-signing.keychain-db"
keychain_password="$(python3 -I -c 'import secrets; print(secrets.token_hex(24))')"
before_list="$(security list-keychains -d user | sed -e 's/^[[:space:]]*//' -e 's/"//g')"
cleanup() {
  security delete-keychain "$keychain" >/dev/null 2>&1 || true
  rm -rf -- "$keychain_dir"
  # Restore exactly what was there before, this job's keychain left out.
  security list-keychains -d user -s $before_list >/dev/null 2>&1 || true
}
trap cleanup EXIT
security create-keychain -p "$keychain_password" "$keychain"
security set-keychain-settings -lut 3600 "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"
p12="$keychain_dir/signing.p12"
printf '%s' "$TILO_MAC_SIGNING_P12" | python3 -I -c 'import base64, sys; sys.stdout.buffer.write(base64.b64decode(sys.stdin.read()))' >"$p12"
security import "$p12" -k "$keychain" -P "$TILO_MAC_SIGNING_P12_PASSWORD" \
  -T /usr/bin/codesign >/dev/null
rm -f "$p12"
security set-key-partition-list -S apple-tool:,apple:,codesign: -s \
  -k "$keychain_password" "$keychain" >/dev/null
security list-keychains -d user -s "$keychain" $before_list
identity="${TILO_MAC_SIGNING_IDENTITY:-}"
if [ -z "$identity" ]; then
  identity="$(security find-identity -v -p codesigning "$keychain" \
    | sed -n 's/^[[:space:]]*[0-9]*) \([0-9A-F]\{40\}\) ".*/\1/p' | head -n 1)"
  [ -n "$identity" ] \
    || { printf 'the imported certificate holds no codesigning identity\n' >&2; exit 1; }
fi

mkdir -p "$out"
TILO_MAC_SIGNING_IDENTITY="$identity" TILO_REQUIRE_SIGNING=1 \
  bash "$repo/apps/imager/packaging/package-macos.sh" "$arch" "$build/imager" "$out"

# The artifact that ships is the one verified, Gatekeeper included.
TILO_REQUIRE_SIGNING=1 TILO_VERSION="$TILO_VERSION" \
  bash "$repo/apps/imager/packaging/macos/verify-dmg.sh" "$arch" \
  "$out/tilo-imager-macos-$arch.dmg" "$out/evidence"

printf 'signed %s %s\n' "$out/tilo-imager-macos-$arch.dmg" "$TILO_VERSION"
