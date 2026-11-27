#!/usr/bin/env bash
# Verify a built Tilo DMG as a user receives it: mount it read-only, check the
# bundle layout, architectures, plists and signatures, then start the bundled
# runtime daemon on a private throwaway profile and require it to answer its
# native status probe. TILO_REQUIRE_SIGNING=1 additionally requires a stapled,
# Gatekeeper-accepted Developer ID signature.
set -euo pipefail

dmg="${1:?usage: verify-app.sh TILO_DMG}"
[ -f "$dmg" ] || { printf '%s is not a file\n' "$dmg" >&2; exit 2; }
require_signing="${TILO_REQUIRE_SIGNING:-0}"

work="$(mktemp -d "${TMPDIR:-/tmp}/tilo-verify.XXXXXX")"
chmod 0700 "$work"
mountpoint="$work/mount"
mkdir "$mountpoint"
daemon=
cleanup() {
  local cert
  if [ -n "$daemon" ]; then
    kill "$daemon" 2>/dev/null || true
    wait "$daemon" 2>/dev/null || true
  fi
  # The daemon keeps the profile's signing key in the login Keychain under
  # the device certificate's locator; remove this throwaway profile's item.
  for cert in "$work"/profile/identity/device/certs/*.bin; do
    [ -f "$cert" ] || continue
    security delete-generic-password -s so.tilo.identity \
      -a "tilo-identity-signing-v1.$(basename "$cert" .bin)" >/dev/null 2>&1 || true
  done
  hdiutil detach "$mountpoint" -quiet 2>/dev/null || true
  rm -rf -- "$work"
}
trap cleanup EXIT

fail() { printf 'verify-app: %s\n' "$*" >&2; exit 1; }

hdiutil verify -quiet "$dmg"
hdiutil attach "$dmg" -quiet -nobrowse -readonly -mountpoint "$mountpoint"
app="$mountpoint/Tilo.app"
[ "$(readlink "$mountpoint/Applications")" = /Applications ] \
  || fail 'DMG lacks the /Applications symlink'

contents="$app/Contents"
agent="$contents/Library/LaunchAgents/so.tilo.runtime.plist"
for path in "$contents/MacOS/Tilo" "$contents/MacOS/tilo-runtime" \
            "$contents/MacOS/tilo-runtime-update"; do
  [ -f "$path" ] && [ -x "$path" ] || fail "missing executable ${path#"$mountpoint"/}"
  [ "$(lipo -archs "$path" | tr ' ' '\n' | sort | xargs)" = "arm64 x86_64" ] \
    || fail "${path#"$mountpoint"/} is not universal"
  signature="$(codesign -dv "$path" 2>&1)"
  grep -Eq '^CodeDirectory .*flags=.*runtime' <<<"$signature" \
    || fail "${path#"$mountpoint"/} is not signed with the hardened runtime"
done
# The runtime packages the daemon installs on its first start, each named by its package root: the
# Tilo runtime the app is and the tilolang runtime its apps run on. One trusted runtime publisher.
runtimes=("$contents"/Resources/runtimes/*.tapp)
[ "${#runtimes[@]}" = 2 ] \
  || fail 'Tilo.app does not carry the Tilo and tilolang runtime packages'
for runtime in "${runtimes[@]}"; do
  [[ "$(basename "$runtime")" =~ ^[0-9a-f]{64}\.tapp$ ]] \
    || fail 'a runtime package Tilo.app carries is not named by its root'
done
[[ "$(cat "$contents/Resources/runtime-bundle-trusted-publishers.txt")" =~ ^[0-9a-f]{64}$ ]] \
  || fail 'Tilo.app does not list exactly one trusted runtime publisher'
update_agent="$contents/Library/LaunchAgents/so.tilo.runtime.update.plist"
plutil -lint -s "$contents/Info.plist" "$agent" "$update_agent"
plist() { /usr/libexec/PlistBuddy -c "Print :$2" "$1"; }
[ "$(plist "$contents/Info.plist" CFBundleExecutable)" = Tilo ] || fail 'CFBundleExecutable is not Tilo'
[ "$(plist "$contents/Info.plist" CFBundleIdentifier)" = so.tilo.runtime ] || fail 'bundle id is not so.tilo.runtime'
[ "$(plist "$agent" Label)" = so.tilo.runtime ] || fail 'agent label is not so.tilo.runtime'
[ "$(plist "$agent" BundleProgram)" = Contents/MacOS/tilo-runtime ] || fail 'agent BundleProgram does not name the runtime'
[ "$(plist "$update_agent" BundleProgram)" = Contents/MacOS/tilo-runtime-update ] \
  || fail 'update agent BundleProgram does not name the updater'
# A bundle without a feed key must decline to update itself, from inside the
# bundle, without touching the network.
if ! plist "$contents/Info.plist" TiloUpdateFeedURL >/dev/null 2>&1; then
  [ "$("$contents/MacOS/tilo-runtime-update" --check)" = 'this build updates through its installer' ] \
    || fail 'an update-less bundle did not decline to update itself'
elif [ "$require_signing" = 1 ]; then
  [[ "$(plist "$contents/Info.plist" TiloUpdatePublicKey)" =~ ^[0-9a-f]{64}$ ]] \
    || fail 'the update feed has no public key'
fi
codesign --verify --deep --strict --verbose=2 "$app"

if [ "$require_signing" = 1 ]; then
  signature="$(codesign -dvv "$app" 2>&1)"
  grep -q '^Authority=Developer ID Application:' <<<"$signature" \
    || fail 'bundle is not signed with a Developer ID Application certificate'
  xcrun stapler validate "$app"
  xcrun stapler validate "$dmg"
  spctl --assess --type execute --verbose=2 "$app"
fi

# The profile is a private throwaway. HOME stays the user's: the identity key
# goes to the login Keychain, which a redirected HOME turns into a blocking
# "keychain not found" prompt. The app and runtime store is private too, so
# first start's carried installs land here and not in the user's store.
runtime_release="${TILO_RUNTIME_RELEASE:-2026.07}"
mkdir -m 0700 "$work/profile" "$work/store"
TILO_APPS_DATA_DIR="$work/store" "$contents/MacOS/tilo-runtime" --profile "$work/profile" \
  </dev/null >"$work/daemon.log" 2>&1 &
daemon=$!
socket="$("$contents/MacOS/tilo-runtime" --profile "$work/profile" --socket)" \
  || fail 'the runtime does not say where its daemon listens'
status=
for _ in $(seq 1 120); do
  kill -0 "$daemon" 2>/dev/null || { cat "$work/daemon.log" >&2; fail 'daemon exited during startup'; }
  if [ -S "$socket" ]; then
    status="$(python3 - "$socket" <<'PY' 2>/dev/null || true
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(5)
s.connect(sys.argv[1])
s.sendall(b'{"native":"status"}\n')
print(s.makefile().readline().strip())
PY
)"
    [ -n "$status" ] && break
  fi
  sleep 0.5
done
[ -n "$status" ] || { cat "$work/daemon.log" >&2; fail 'daemon never answered its status probe'; }
python3 -c 'import json, sys; s = json.loads(sys.argv[1]); assert s["version"] == 1 and s["pid"] == int(sys.argv[2]) and s["room"], s' \
  "$status" "$daemon" || fail "unexpected daemon status: $status"

# First start installed the runtime packages Tilo.app carries into the store the installer reads:
# the Tilo runtime it is, named by the release its apps are built for, and the tilolang runtime
# every tilolang package runs on. A package naming either finds it; one naming another release is
# refused by name.
runtimes_in_store="$work/store/runtimes"
for _ in $(seq 1 60); do
  if [ -f "$runtimes_in_store/$runtime_release/bundle.toml" ] \
     && compgen -G "$runtimes_in_store/*+tilolang.*/bundle.toml" >/dev/null; then
    break
  fi
  sleep 0.5
done
[ -f "$runtimes_in_store/$runtime_release/bundle.toml" ] \
  || { cat "$work/daemon.log" >&2; fail "first start did not install the Tilo runtime $runtime_release it carries"; }
compgen -G "$runtimes_in_store/*+tilolang.*/bundle.toml" >/dev/null \
  || { cat "$work/daemon.log" >&2; fail 'first start did not install the tilolang runtime it carries'; }

# One of the collection's packages installs beside them. TILO_TAPP_TOOLS names the tilolang and
# tilo-install binaries built from this tree; the package's own runtime is the one first start
# installed, so nothing is fetched. The daemon stops first so the store has one writer.
tools="${TILO_TAPP_TOOLS:-}"
if [ -n "$tools" ]; then
  kill "$daemon" 2>/dev/null || true
  wait "$daemon" 2>/dev/null || true
  daemon=
  repo="$(cd "$(dirname "$0")/../.." && pwd)"
  tapps="$work/tapps"
  mkdir -p "$tapps"
  ( cd "$repo" && "$tools/tilolang" build --locked "$repo/apps/tl/notes" --out "$tapps" >/dev/null ) \
    || fail 'the notes package did not build'
  tapp="$(ls "$tapps"/*.tapp | head -n 1)"
  TILO_APPS_DATA_DIR="$work/store" \
  TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE="$contents/Resources/runtime-bundle-trusted-publishers.txt" \
    "$tools/tilo-install" install "$tapp" --allow-self-signed \
    || fail 'a collection package did not install on the runtime first start installed'
  TILO_APPS_DATA_DIR="$work/store" "$tools/tilo-install" list | grep -q 'so.tilo.notes' \
    || fail 'the collection package is not in the store after installing'
fi

printf 'verify-app: %s ok; bundled daemon pid %s answered %s; carried runtime %s and the tilolang runtime installed on first start\n' \
  "$dmg" "$daemon" "$status" "$runtime_release"
