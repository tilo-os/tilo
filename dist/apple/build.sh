#!/usr/bin/env bash
# Build the Mac and the iPhone app from the Swift package beside this script, as signed app
# bundles under target/apple.noindex/, where Spotlight does not look: a build is offered to a
# person as an app only once someone puts it where apps go. README.md says what the apps are.
#
#   dist/apple/build.sh test                    the package's tests; they open no window
#   dist/apple/build.sh mac [--check [--live]]  target/apple.noindex/mac/Tilo Assistant.app, and, asked
#                                               to, what it can tell of itself with no window
#   dist/apple/build.sh iphone [--install]      target/apple.noindex/iphone/Tilo Assistant.app, and,
#                                               asked to, onto the one iPhone that is reachable
#   dist/apple/build.sh simulator [--live]      the iPhone app in a simulator that shows no
#                                               window, asked what it can tell of itself
#
# Environment:
#   TILO_APPLE_SIGNING_IDENTITY  who signs ("Apple Development: ..."). Without one the Mac app
#                                is signed by nobody: it runs on this Mac, a later build cannot
#                                read the keys it kept, and it has no share extension. An
#                                iPhone app needs one.
#   TILO_APPLE_PROFILE           the iPhone app's provisioning profile (a .mobileprovision)
#   TILO_APPLE_DEVICE            the iPhone to install on, when more than one is reachable
#   TILO_APPLE_SIMULATOR         the simulator to ask (default: the newest iPhone installed)
#   TILO_SHELL_SITE              the Tilo site the build opens (default https://tilo.sh)
#   TILO_SHELL_APP               the app it shows there (default so.tilo.chat)
#   TILO_VERSION                 the version (default dist/VERSION)
#   DEVELOPER_DIR                the Xcode to build with, when it is not the selected one
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
here="$repo/dist/apple"
bundle="$here/Bundle"
out="$repo/target/apple.noindex"
name="Tilo Assistant"
# The oldest system whose web view can show Tilo's page; Package.swift says why.
minimum=26.4

say() { printf 'dist/apple: %s\n' "$*" >&2; }
die() { say "$*"; exit 1; }
plist() { /usr/libexec/PlistBuddy "$@"; }
usage() { sed -n '2,/^set /p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//' >&2; exit 2; }

[ "$(uname -s)" = Darwin ] || die "this builds on macOS only"

identity="${TILO_APPLE_SIGNING_IDENTITY:--}"
site="${TILO_SHELL_SITE:-https://tilo.sh}"
shown="${TILO_SHELL_APP:-so.tilo.chat}"
version="${TILO_VERSION:-$(cat "$repo/dist/VERSION")}"
mark="$bundle/AppIcon.svg"

[[ "$site" =~ ^https://[a-z0-9.-]+(:[0-9]+)?$ \
  || "$site" =~ ^http://(localhost|[a-z0-9.-]+\.localhost|127\.[0-9]+\.[0-9]+\.[0-9]+)(:[0-9]+)?$ ]] \
  || die "TILO_SHELL_SITE is the origin of a Tilo site: https://..., or http:// on this computer"
[[ "$shown" =~ ^[a-z0-9]+(\.[a-z0-9][a-z0-9-]*)+$ ]] \
  || die "TILO_SHELL_APP is an application id such as so.tilo.chat"

# The team that signs. The app and its share extension keep their one shared place under it.
team=""
if [ "$identity" != - ]; then
  team="$(security find-certificate -c "$identity" -p 2>/dev/null \
    | openssl x509 -noout -subject -nameopt multiline 2>/dev/null \
    | sed -n 's/^ *organizationalUnitName *= *//p' | head -n 1)" || true
  [ -n "$team" ] || die "no certificate named \"$identity\" is in the keychain"
fi

# Build the package for the Mac, an iPhone or a simulator, and print where its programs are.
products() {
  local flags=(-c release --scratch-path "$out/swiftpm-$1")
  case "$1" in
    iphone)
      flags+=(--triple "arm64-apple-ios$minimum" --sdk "$(xcrun --sdk iphoneos --show-sdk-path)")
      ;;
    simulator)
      # A simulator's programs run as this Mac's own, and a Mac refuses a signature that claims
      # a keychain group with no profile behind it. There the entitlements ride in the program,
      # where the simulator reads them.
      flags+=(--triple "$(uname -m)-apple-ios$minimum-simulator" --sdk "$(xcrun --sdk iphonesimulator --show-sdk-path)"
        -Xlinker -sectcreate -Xlinker __TEXT -Xlinker __entitlements -Xlinker "$out/simulator/rights.plist")
      ;;
  esac
  (cd "$here" && xcrun swift build "${flags[@]}" >&2 && xcrun swift build "${flags[@]}" --show-bin-path)
}

# Whether someone has the app bundle open. A bundle in use is theirs: it is never replaced
# under them.
running() {
  local programs
  programs="$(ps -axo command=)"
  grep -qF -- "$1/Contents/MacOS/" <<<"$programs"
}

# Write what a build knows into a bundle's Info.plist: the version and the signing team, and
# for an app the site it opens and the app it shows.
stamp() {
  plist -c "Set :CFBundleShortVersionString $version" -c "Set :CFBundleVersion $version" "$1"
  [ -z "$team" ] || plist -c "Set :TiloTeam $team" "$1"
  [ "${2:-}" != app ] || plist -c "Set :TiloSite $site" -c "Set :TiloApp $shown" "$1"
}

# An entitlements file with the signing team written in.
entitle() { sed "s/__TEAM__/$team/g" "$1" > "$2"; }

# Tilo's mark at so many pixels. No picture is kept in the repository: the one drawing in
# Bundle/ is drawn at each size when a bundle is made.
draw() { sips -s format png -z "$2" "$2" "$mark" --out "$1" >/dev/null; }

build_mac() {
  local bin app="$out/mac/$name.app" size
  ! running "$app" || die "$app is open: quit it first, or it would be replaced under whoever uses it"
  bin="$(products mac)"
  local contents="$app/Contents" icons="$out/mac/AppIcon.iconset" rights="$out/mac/app.entitlements"
  rm -rf "$out/mac"
  mkdir -p "$contents/MacOS" "$contents/Resources" "$icons"
  cp "$bin/TiloMac" "$contents/MacOS/TiloAssistant"
  cp "$bundle/Mac-Info.plist" "$contents/Info.plist"
  stamp "$contents/Info.plist" app
  cp -R "$bundle/en.lproj" "$contents/Resources/"
  for size in 16 32 128 256 512; do
    draw "$icons/icon_${size}x${size}.png" "$size"
    draw "$icons/icon_${size}x${size}@2x.png" "$((size * 2))"
  done
  iconutil -c icns "$icons" -o "$contents/Resources/AppIcon.icns"
  rm -rf "$icons"

  local sign=(codesign --force --options runtime --timestamp=none --sign "$identity")
  entitle "$bundle/Mac.entitlements" "$rights"
  if [ -n "$team" ]; then
    local appex="$contents/PlugIns/TiloShare.appex"
    mkdir -p "$appex/Contents/MacOS" "$appex/Contents/Resources"
    cp "$bin/TiloShare" "$appex/Contents/MacOS/TiloShare"
    cp "$bundle/MacShare-Info.plist" "$appex/Contents/Info.plist"
    stamp "$appex/Contents/Info.plist"
    cp -R "$bundle/en.lproj" "$appex/Contents/Resources/"
    entitle "$bundle/MacShare.entitlements" "$out/mac/share.entitlements"
    # Inner code first, then the bundle that seals it; never --deep.
    "${sign[@]}" --entitlements "$out/mac/share.entitlements" "$appex"
  else
    # Signed by nobody, the app and an extension share no place: it is built without one.
    plist -c "Delete :com.apple.security.application-groups" "$rights"
    say "signed by nobody: no share extension, and the next build cannot read this one's keys"
  fi
  "${sign[@]}" --entitlements "$rights" "$app"
  codesign --verify --deep --strict "$app"
}

# What the Mac app can tell of itself with no window. Twice: the first run may store the key
# the second has to find again.
check_mac() {
  local program="$out/mac/$name.app/Contents/MacOS/TiloAssistant"
  "$program" --check "$@" >/dev/null || true
  "$program" --check "$@"
}

# One bundle of the iPhone app: what it says of itself, its sentences, the profile that lets
# it run on the phone, and its signature. The program is already in it.
phone_part() { # directory, bundle id, Info.plist, entitlements, "app" for the app itself
  local dir="$1" rights="$out/iphone/$2.entitlements" sdk covers
  sdk="$(xcrun --sdk iphoneos --show-sdk-version)"
  covers="$(plist -c 'Print :Entitlements:application-identifier' "$out/iphone/profile.plist")"
  # shellcheck disable=SC2254  # the profile's identifier is a pattern ("TEAM.*")
  case "$team.$2" in $covers) ;; *) die "the profile covers $covers, not $2" ;; esac
  cp "$3" "$dir/Info.plist"
  stamp "$dir/Info.plist" "${5:-}"
  plist -c "Add :DTPlatformName string iphoneos" -c "Add :DTPlatformVersion string $sdk" \
    -c "Add :DTSDKName string iphoneos$sdk" "$dir/Info.plist"
  cp -R "$bundle/en.lproj" "$dir/"
  cp "$TILO_APPLE_PROFILE" "$dir/embedded.mobileprovision"
  entitle "$4" "$rights"
  # A development profile lets a debugger attach, and the signature has to say the same.
  if [ "$(plist -c 'Print :Entitlements:get-task-allow' "$out/iphone/profile.plist" 2>/dev/null)" = true ]; then
    plist -c "Add :get-task-allow bool true" "$rights"
  fi
  codesign --force --timestamp=none --sign "$identity" --entitlements "$rights" "$dir"
}

build_iphone() {
  [ -n "$team" ] || die "an iPhone app is signed by someone: name them in TILO_APPLE_SIGNING_IDENTITY"
  [ -f "${TILO_APPLE_PROFILE:-}" ] || die "name the iPhone app's provisioning profile in TILO_APPLE_PROFILE"
  local bin app="$out/iphone/$name.app" scale
  bin="$(products iphone)"
  local appex="$app/PlugIns/TiloShare.appex"
  rm -rf "$out/iphone"
  mkdir -p "$appex"
  security cms -D -i "$TILO_APPLE_PROFILE" > "$out/iphone/profile.plist" 2>/dev/null \
    || die "$TILO_APPLE_PROFILE is no provisioning profile"
  [ "$(plist -c 'Print :TeamIdentifier:0' "$out/iphone/profile.plist")" = "$team" ] \
    || die "the profile is another team's than the signing identity's"

  # The extension first, then the app that seals it.
  cp "$bin/TiloShare" "$appex/TiloShare"
  phone_part "$appex" so.tilo.assistant.shell.share "$bundle/PhoneShare-Info.plist" "$bundle/PhoneShare.entitlements"
  cp "$bin/TiloPhone" "$app/TiloAssistant"
  cp -R "$bundle/Settings.bundle" "$app/"
  for scale in 2 3; do
    draw "$out/iphone/icon.png" "$((60 * scale))"
    # An icon on a phone carries no transparency.
    sips -s format jpeg -s formatOptions best "$out/iphone/icon.png" --out "$out/iphone/icon.jpg" >/dev/null
    sips -s format png "$out/iphone/icon.jpg" --out "$app/AppIcon60x60@${scale}x.png" >/dev/null
  done
  rm -f "$out/iphone/icon.png" "$out/iphone/icon.jpg"
  phone_part "$app" so.tilo.assistant.shell "$bundle/Phone-Info.plist" "$bundle/Phone.entitlements" app
  codesign --verify --deep --strict "$app"
}

# Onto the one paired iPhone that is reachable, or the one TILO_APPLE_DEVICE names. The app is
# installed and not opened.
install_iphone() {
  local device="${TILO_APPLE_DEVICE:-}" list="$out/iphone/devices.json"
  if [ -z "$device" ]; then
    xcrun devicectl list devices --json-output "$list" >/dev/null 2>&1 || die "the devices cannot be listed"
    device="$(python3 -I -c '
import json, sys
found = [d["hardwareProperties"]["udid"] for d in json.load(open(sys.argv[1]))["result"]["devices"]
         if d.get("hardwareProperties", {}).get("reality") == "physical"
         and d["hardwareProperties"].get("deviceType") == "iPhone"
         and d.get("connectionProperties", {}).get("pairingState") == "paired"
         and d["connectionProperties"].get("tunnelState") != "unavailable"]
print(found[0] if len(found) == 1 else "")
' "$list")"
  fi
  [ -n "$device" ] \
    || die "not exactly one paired iPhone is reachable: unlock it and plug it in, or name one in TILO_APPLE_DEVICE"
  xcrun devicectl device install app --device "$device" "$out/iphone/$name.app"
}

# The iPhone app in a simulator, asked what it can tell. No simulator has a graphics adapter,
# so none can show Tilo: this says whether the app starts on a phone's system, whether the
# script that joins page and system works in a phone's web view, whether the page's key is
# kept, and whether the site answers. The simulator runs without its window and is shut down
# again if this started it.
check_simulator() {
  local bin app="$out/simulator/$name.app" device started="" failed="" run
  rm -rf "$out/simulator"
  mkdir -p "$app"
  sed "s/__TEAM__/SIMULATOR0/g" "$bundle/Phone.entitlements" > "$out/simulator/rights.plist"
  bin="$(products simulator)"
  cp "$bin/TiloPhone" "$app/TiloAssistant"
  cp "$bundle/Phone-Info.plist" "$app/Info.plist"
  stamp "$app/Info.plist" app
  # The team its entitlements name, so the app finds the keychain group they grant.
  plist -c "Set :CFBundleSupportedPlatforms:0 iPhoneSimulator" -c "Delete :CFBundleIcons" \
    -c "Set :TiloTeam SIMULATOR0" "$app/Info.plist"
  cp -R "$bundle/en.lproj" "$bundle/Settings.bundle" "$app/"
  codesign --force --timestamp=none --sign - "$app"

  device="${TILO_APPLE_SIMULATOR:-$(xcrun simctl list devices available -j | python3 -I -c '
import json, sys
systems = json.load(sys.stdin)["devices"]
found = [device["udid"] for system, devices in sorted(systems.items(), reverse=True) if "iOS" in system
         for device in devices if device["name"].startswith("iPhone")]
print(found[0] if found else "")
')}"
  [ -n "$device" ] || die "no iPhone simulator is installed"
  if ! xcrun simctl list devices booted | grep -q "$device"; then
    xcrun simctl boot "$device"
    started=1
  fi
  {
    xcrun simctl bootstatus "$device" -b >/dev/null && xcrun simctl install "$device" "$app" &&
      # Twice: the first run may store the key the second has to find again.
      for run in first second; do
        xcrun simctl launch --console --terminate-running-process "$device" so.tilo.assistant.shell \
          --check --quit "$@" > "$out/simulator/$run.txt" 2>&1 || true
      done
  } || failed=1
  xcrun simctl uninstall "$device" so.tilo.assistant.shell >/dev/null 2>&1 || true
  [ -z "$started" ] || xcrun simctl shutdown "$device" >/dev/null 2>&1 || true
  [ -z "$failed" ] || die "the app could not be put into the simulator"
  grep '^CHECK ' "$out/simulator/second.txt" | sed 's/^CHECK //' | grep -v '^end$' \
    || die "the app said nothing in the simulator"
  ! grep '^CHECK FAIL' "$out/simulator/second.txt" | grep -qv 'WebGPU'
}

command="${1:-}"
[ $# -eq 0 ] || shift
case "$command" in
  test)
    (cd "$here" && xcrun swift test --scratch-path "$out/swiftpm-mac")
    ;;
  mac)
    build_mac
    say "built $out/mac/$name.app"
    case "${1:-}" in
      --check) shift; check_mac "$@" ;;
      "") ;;
      *) usage ;;
    esac
    ;;
  iphone)
    build_iphone
    say "built $out/iphone/$name.app"
    case "${1:-}" in
      --install) install_iphone ;;
      "") ;;
      *) usage ;;
    esac
    ;;
  simulator)
    check_simulator "$@"
    ;;
  *) usage ;;
esac
