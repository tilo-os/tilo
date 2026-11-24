#!/usr/bin/env bash
# Build Tilo Shell, the app of the Swift package beside this script, for the Mac and the
# iPhone, as signed app bundles under target/apple.noindex/, where Spotlight does not look: a
# build is offered to a person as an app only once someone puts it where apps go. README.md
# says what it is.
#
#   dist/apple/build.sh test                     the package's tests; they open no window
#   dist/apple/build.sh mac [--check [--live]]   the Mac app, and, asked to, what it can tell
#                                                of itself with no window
#   dist/apple/build.sh iphone [--install]       the iPhone app, and, asked to, onto the one
#                                                iPhone that is reachable
#   dist/apple/build.sh simulator [--live]       the iPhone app in a simulator that shows no
#                                                window, asked what it can tell of itself
#
# A bundle lands in target/apple.noindex/shell/mac/ or target/apple.noindex/shell/iphone/
# under its app's name.
#
# Environment:
#   TILO_APPLE_SIGNING_IDENTITY  who signs ("Apple Development: ..."). Without one the Mac app
#                                is signed by nobody: it runs on this Mac, a later build cannot
#                                read the keys it kept, and it has no share extension. An
#                                iPhone app needs one.
#   TILO_APPLE_PROFILE           the iPhone app's provisioning profile (a .mobileprovision)
#   TILO_APPLE_SHARE_PROFILE     the profile of the iPhone app's share extension (default: the
#                                app's). A profile made for the app's own identifier, which
#                                passkeys take, covers the app alone.
#   TILO_APPLE_MAC_PROFILE       the Mac app's provisioning profile (a .provisionprofile). A
#                                Mac app carries one only to use passkeys.
#   TILO_APPLE_PASSKEY_SITES     the sites whose passkeys the app may use, as host names with
#                                spaces between them (default: the host of TILO_SHELL_SITE,
#                                unless that is this computer). The build names them only
#                                when its profile grants associated domains, and they work
#                                only once the site names the app: README.md, "Passkeys". A
#                                name ending in ?mode=developer is a site Apple's servers
#                                cannot reach, for a build signed for development.
#   TILO_APPLE_DEVICE            the iPhone to install on, when more than one is reachable
#   TILO_APPLE_SIMULATOR         the simulator to ask (default: the newest iPhone installed)
#   TILO_SHELL_SITE              the Tilo site the build opens (default https://tilo.sh)
#   TILO_SHELL_APP               an app Tilo Shell opens in front on the desktop (default:
#                                none)
#   TILO_VERSION                 the version, which is the release's name (default
#                                dist/VERSION). The build number beside it is no word of
#                                the environment: it is the number of commits the commit
#                                that is built stands on.
#   DEVELOPER_DIR                the Xcode to build with, when it is not the selected one
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
here="$repo/dist/apple"
bundle="$here/Bundle"
out="$repo/target/apple.noindex"
# The oldest system whose web view can show Tilo's page; Package.swift says why.
minimum=26.4

say() { printf 'dist/apple: %s\n' "$*" >&2; }
die() { say "$*"; exit 1; }
plist() { /usr/libexec/PlistBuddy "$@"; }
usage() { sed -n '2,/^set /p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//' >&2; exit 2; }

[ "$(uname -s)" = Darwin ] || die "this builds on macOS only"

# The app: a bundle's name, its identifier and what it shows of Tilo are all written here.
app=shell
name="Tilo Shell"
program=TiloShell
# The identifier the app was first built under. It stays, because whoever uses the app
# is signed in under it; README.md says what an identifier of its own takes.
id=so.tilo.assistant.shell
alone=false
shown="${TILO_SHELL_APP:-}"
made="$out/$app"

identity="${TILO_APPLE_SIGNING_IDENTITY:--}"
site="${TILO_SHELL_SITE:-https://tilo.sh}"
version="${TILO_VERSION:-$(cat "$repo/dist/VERSION")}"
# The app's icon: Tilo's mark on its light ground.
mark="$bundle/AppIcon.svg"

[[ "$site" =~ ^https://[a-z0-9.-]+(:[0-9]+)?$ \
  || "$site" =~ ^http://(localhost|[a-z0-9.-]+\.localhost|127\.[0-9]+\.[0-9]+\.[0-9]+)(:[0-9]+)?$ ]] \
  || die "TILO_SHELL_SITE is the origin of a Tilo site: https://..., or http:// on this computer"
[[ -z "$shown" || "$shown" =~ ^[a-z0-9]+(\.[a-z0-9][a-z0-9-]*)+$ ]] \
  || die "TILO_SHELL_APP is an application id such as so.tilo.chat"

# The sites whose passkeys the app may use. A system web view lets a page ask for a passkey
# only for a site its app is associated with: the app's signature names the site, and the
# site names the app. A site on this computer can be neither, so a build for one names none.
passkey_sites=()
if [ -n "${TILO_APPLE_PASSKEY_SITES+set}" ]; then
  read -r -a passkey_sites <<<"$TILO_APPLE_PASSKEY_SITES"
elif [[ "$site" == https://* ]]; then
  passkey_sites=("$(sed -e 's|^https://||' -e 's|:[0-9]*$||' <<<"$site")")
fi
a_site='^(\*\.)?[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+(\?mode=(developer|managed|developer\+managed))?$'
if [ "${#passkey_sites[@]}" -gt 0 ]; then
  for named in "${passkey_sites[@]}"; do
    [[ "$named" =~ $a_site ]] \
      || die "TILO_APPLE_PASSKEY_SITES names sites by host, such as tilo.sh or tilo.example?mode=developer: $named"
  done
fi

# The team that signs. The app and its share extension keep their one shared place under it.
team=""
if [ "$identity" != - ]; then
  team="$(security find-certificate -c "$identity" -p 2>/dev/null \
    | openssl x509 -noout -subject -nameopt multiline 2>/dev/null \
    | sed -n 's/^ *organizationalUnitName *= *//p' | head -n 1)" || true
  [ -n "$team" ] || die "no certificate named \"$identity\" is in the keychain"
fi

# Build the package for the Mac, an iPhone or a simulator, and print where its programs are.
# The Mac's and the iPhone's programs are the same for both apps, so they are built once.
products() {
  local scratch="$out/swiftpm-$1" flags=(-c release)
  case "$1" in
    iphone)
      flags+=(--triple "arm64-apple-ios$minimum" --sdk "$(xcrun --sdk iphoneos --show-sdk-path)")
      ;;
    simulator)
      # A simulator's programs run as this Mac's own, and a Mac refuses a signature that claims
      # a keychain group with no profile behind it. There the entitlements ride in the program,
      # where the simulator reads them, so each app has programs of its own.
      scratch="$made/swiftpm-simulator"
      flags+=(--triple "$(uname -m)-apple-ios$minimum-simulator" --sdk "$(xcrun --sdk iphonesimulator --show-sdk-path)"
        -Xlinker -sectcreate -Xlinker __TEXT -Xlinker __entitlements -Xlinker "$made/simulator/rights.plist")
      ;;
  esac
  flags+=(--scratch-path "$scratch")
  (cd "$here" && xcrun swift build "${flags[@]}" >&2 && xcrun swift build "${flags[@]}" --show-bin-path)
}

# Whether someone has the app bundle open: whether a program that runs is one of the bundle's
# own, the app's or its extension's. Only the programs' own paths are read, so a search that
# merely names the bundle is not taken for it. A bundle in use is theirs: it is never
# replaced under them.
running() {
  local programs
  programs="$(ps -axo comm=)"
  grep -qF -- "$1/Contents/" <<<"$programs"
}

# A file of Bundle/ as this app's: its name, its program, its identifier and the team that
# signs it, written where the file leaves room for them.
fill() {
  sed -e "s/__NAME__/$name/g" -e "s/__PROGRAM__/$program/g" -e "s/__ID__/$id/g" -e "s/__TEAM__/$team/g" "$1" > "$2"
}

# Which build this is, asked of the commit that is built and set by no hand. The version is
# the release's name, and many builds are of one release. The build number is the number of
# commits the built commit stands on: every later head of main reads a higher one, so two
# builds are told apart wherever a version is shown with its build, as in the Mac's About
# window. The commit's own name is kept beside the two, and a tree that differs from its
# commit says that it does. A tree that cannot say its commit builds nothing.
build=""
commit=""
built_from() {
  [ -z "$build" ] || return 0
  commit="$(git -C "$repo" rev-parse --short=11 HEAD 2>/dev/null)" \
    || die "a build says which commit it is of, and $repo is no git checkout"
  [ "$(git -C "$repo" rev-parse --is-shallow-repository)" = false ] \
    || die "a build's number counts the commits under $commit, and this checkout holds only some of them: git fetch --unshallow"
  build="$(git -C "$repo" rev-list --count HEAD)"
  [ -z "$(git -C "$repo" status --porcelain)" ] || commit="$commit+changes"
  say "version $version ($build), built from $commit"
}

# Write what a build knows into a bundle's Info.plist: the version, the build number and the
# commit it is of, and the signing team, and for an app the site it opens and what it shows
# there.
stamp() {
  built_from
  plist -c "Set :CFBundleShortVersionString $version" -c "Set :CFBundleVersion $build" "$1"
  plist -c "Delete :TiloCommit" "$1" >/dev/null 2>&1 || true
  plist -c "Add :TiloCommit string $commit" "$1"
  [ -z "$team" ] || plist -c "Set :TiloTeam $team" "$1"
  [ "${2:-}" = app ] || return 0
  plist -c "Set :TiloSite $site" -c "Set :TiloAlone $alone" "$1"
  [ -z "$shown" ] || plist -c "Set :TiloApp $shown" "$1"
}

# Let a bundle through to a site on this computer that is served by plain http, and to nothing
# else by plain http. Inside a bundle the system refuses plain http to a name with a dot in
# it, so an app built for http://tilo.localhost:24520 could not load its own site. A build for
# such a site names exactly that name in its Info.plist ($1) as the one the system lets
# through: no name under it, and no other. An address cannot be named there, so a build for
# one (http://127.0.0.1:24520) says that plain http is let through to what is local, the
# narrowest the system has for an address. A build for an https site says nothing of it and
# is held to https everywhere.
transport() { # Info.plist
  local named="${site#http://}"
  [[ "$site" == http://* ]] || return 0
  named="${named%:*}"
  plist -c "Add :NSAppTransportSecurity dict" "$1"
  if [[ "$named" =~ ^[0-9.]+$ ]]; then
    plist -c "Add :NSAppTransportSecurity:NSAllowsLocalNetworking bool true" "$1"
  else
    plist -c "Add :NSAppTransportSecurity:NSExceptionDomains dict" \
      -c "Add :NSAppTransportSecurity:NSExceptionDomains:$named dict" \
      -c "Add :NSAppTransportSecurity:NSExceptionDomains:$named:NSExceptionAllowsInsecureHTTPLoads bool true" "$1"
  fi
}

# Name the sites whose passkeys the app may use: in the rights its signature claims ($1), and
# in its Info.plist ($3), where the app's own check reads them. Only as far as its
# provisioning profile ($2, decoded; empty for none) lets it: a signature that claims what its
# profile does not grant is not installed on an iPhone and not started on a Mac. A build that
# may name none says so, because its page can then ask for no passkey.
associate() { # rights, decoded profile, Info.plist
  local rights="$1" profile="$2" info="$3" named index=0
  [ "${#passkey_sites[@]}" -gt 0 ] || return 0
  if [ -z "$profile" ]; then
    say "no passkeys in this build: it carries no profile (README.md, \"Passkeys\")"
    return 0
  fi
  if ! plist -c 'Print :Entitlements:com.apple.developer.associated-domains' "$profile" >/dev/null 2>&1; then
    say "no passkeys in this build: its profile grants no associated domains (README.md, \"Passkeys\")"
    return 0
  fi
  plist -c "Add :com.apple.developer.associated-domains array" "$rights"
  plist -c "Add :TiloPasskeySites array" "$info"
  for named in "${passkey_sites[@]}"; do
    plist -c "Add :com.apple.developer.associated-domains:$index string webcredentials:$named" "$rights"
    plist -c "Add :TiloPasskeySites:$index string ${named%%\?*}" "$info"
    index=$((index + 1))
    case "$named" in
      *\?mode=*) say "${named%%\?*} is named in ${named##*=} mode: for devices set up for it, never for a store" ;;
    esac
  done
  say "passkeys: the build names ${passkey_sites[*]}"
}

draw() { sips -s format png -z "$2" "$2" "$mark" --out "$1" >/dev/null; }

build_mac() {
  local bin app="$made/mac/$name.app" size
  ! running "$app" || die "$app is open: quit it first, or it would be replaced under whoever uses it"
  bin="$(products mac)"
  local contents="$app/Contents" icons="$made/mac/AppIcon.iconset" rights="$made/mac/app.entitlements"
  # Opened while it was being built: the same.
  ! running "$app" || die "$app is open: quit it first, or it would be replaced under whoever uses it"
  rm -rf "$made/mac"
  mkdir -p "$contents/MacOS" "$contents/Resources" "$icons"
  cp "$bin/TiloMac" "$contents/MacOS/$program"
  fill "$bundle/Mac-Info.plist" "$contents/Info.plist"
  stamp "$contents/Info.plist" app
  cp -R "$bundle/en.lproj" "$contents/Resources/"
  for size in 16 32 128 256 512; do
    draw "$icons/icon_${size}x${size}.png" "$size"
    draw "$icons/icon_${size}x${size}@2x.png" "$((size * 2))"
  done
  iconutil -c icns "$icons" -o "$contents/Resources/AppIcon.icns"
  rm -rf "$icons"

  local sign=(codesign --force --options runtime --timestamp=none --sign "$identity")
  fill "$bundle/Mac.entitlements" "$rights"
  if [ -n "$team" ]; then
    local appex="$contents/PlugIns/TiloShare.appex"
    mkdir -p "$appex/Contents/MacOS" "$appex/Contents/Resources"
    cp "$bin/TiloShare" "$appex/Contents/MacOS/TiloShare"
    fill "$bundle/MacShare-Info.plist" "$appex/Contents/Info.plist"
    stamp "$appex/Contents/Info.plist"
    cp -R "$bundle/en.lproj" "$appex/Contents/Resources/"
    fill "$bundle/MacShare.entitlements" "$made/mac/share.entitlements"
    # Inner code first, then the bundle that seals it; never --deep.
    "${sign[@]}" --entitlements "$made/mac/share.entitlements" "$appex"
  else
    # Signed by nobody, the app and an extension share no place: it is built without one.
    plist -c "Delete :com.apple.security.application-groups" "$rights"
    say "signed by nobody: no share extension, and the next build cannot read this one's keys"
  fi
  # A Mac app runs without a provisioning profile. It carries one only to use passkeys: being
  # associated with a site is a right a profile grants, to the app the signature says it is.
  local profile=""
  if [ -n "${TILO_APPLE_MAC_PROFILE:-}" ]; then
    [ -n "$team" ] || die "a profile is for a signed app: name who signs in TILO_APPLE_SIGNING_IDENTITY"
    [ -f "$TILO_APPLE_MAC_PROFILE" ] || die "TILO_APPLE_MAC_PROFILE names no file"
    profile="$made/mac/profile.plist"
    security cms -D -i "$TILO_APPLE_MAC_PROFILE" > "$profile" 2>/dev/null \
      || die "$TILO_APPLE_MAC_PROFILE is no provisioning profile"
    [ "$(plist -c 'Print :TeamIdentifier:0' "$profile")" = "$team" ] \
      || die "the profile is another team's than the signing identity's"
    local covers
    covers="$(plist -c 'Print :Entitlements:com.apple.application-identifier' "$profile" 2>/dev/null)" \
      || die "$TILO_APPLE_MAC_PROFILE is no Mac app's profile"
    # shellcheck disable=SC2254  # the profile's identifier is a pattern ("TEAM.*")
    case "$team.$id" in $covers) ;; *) die "the profile covers $covers, not $id" ;; esac
    cp "$TILO_APPLE_MAC_PROFILE" "$contents/embedded.provisionprofile"
    plist -c "Add :com.apple.application-identifier string $team.$id" \
      -c "Add :com.apple.developer.team-identifier string $team" "$rights"
  fi
  associate "$rights" "$profile" "$contents/Info.plist"
  "${sign[@]}" --entitlements "$rights" "$app"
  codesign --verify --deep --strict "$app"
}

# What the Mac app can tell of itself with no window. Twice: the first run may store the key
# the second has to find again.
check_mac() {
  local run="$made/mac/$name.app/Contents/MacOS/$program"
  "$run" --check "$@" >/dev/null || true
  "$run" --check "$@"
}

# One bundle of the iPhone app: what it says of itself, its sentences, the profile that lets
# it run on the phone, and its signature. The program is already in it.
phone_part() { # directory, bundle id, Info.plist, entitlements, profile, "app" for the app itself
  local dir="$1" rights="$made/iphone/$2.entitlements" profile="$5" decoded="$made/iphone/$2.profile.plist" sdk covers
  security cms -D -i "$profile" > "$decoded" 2>/dev/null || die "$profile is no provisioning profile"
  [ "$(plist -c 'Print :TeamIdentifier:0' "$decoded")" = "$team" ] \
    || die "$profile is another team's than the signing identity's"
  sdk="$(xcrun --sdk iphoneos --show-sdk-version)"
  covers="$(plist -c 'Print :Entitlements:application-identifier' "$decoded")"
  # shellcheck disable=SC2254  # the profile's identifier is a pattern ("TEAM.*")
  local hint=""
  [ "${6:-}" = app ] || hint=": name the profile of the share extension in TILO_APPLE_SHARE_PROFILE"
  case "$team.$2" in $covers) ;; *) die "the profile covers $covers, not $2$hint" ;; esac
  fill "$3" "$dir/Info.plist"
  stamp "$dir/Info.plist" "${6:-}"
  plist -c "Add :DTPlatformName string iphoneos" -c "Add :DTPlatformVersion string $sdk" \
    -c "Add :DTSDKName string iphoneos$sdk" "$dir/Info.plist"
  cp -R "$bundle/en.lproj" "$dir/"
  cp "$profile" "$dir/embedded.mobileprovision"
  fill "$4" "$rights"
  # A development profile lets a debugger attach, and the signature has to say the same.
  if [ "$(plist -c 'Print :Entitlements:get-task-allow' "$decoded" 2>/dev/null)" = true ]; then
    plist -c "Add :get-task-allow bool true" "$rights"
  fi
  # The app itself, not its extension, shows the page.
  [ "${6:-}" != app ] || associate "$rights" "$decoded" "$dir/Info.plist"
  codesign --force --timestamp=none --sign "$identity" --entitlements "$rights" "$dir"
}

build_iphone() {
  [ -n "$team" ] || die "an iPhone app is signed by someone: name them in TILO_APPLE_SIGNING_IDENTITY"
  [ -f "${TILO_APPLE_PROFILE:-}" ] || die "name the iPhone app's provisioning profile in TILO_APPLE_PROFILE"
  local bin app="$made/iphone/$name.app" scale share_profile="${TILO_APPLE_SHARE_PROFILE:-$TILO_APPLE_PROFILE}"
  [ -f "$share_profile" ] || die "TILO_APPLE_SHARE_PROFILE names no file"
  bin="$(products iphone)"
  local appex="$app/PlugIns/TiloShare.appex"
  rm -rf "$made/iphone"
  mkdir -p "$appex"

  # The extension first, then the app that seals it. Each carries the profile that covers it.
  cp "$bin/TiloShare" "$appex/TiloShare"
  phone_part "$appex" "$id.share" "$bundle/PhoneShare-Info.plist" "$bundle/PhoneShare.entitlements" "$share_profile"
  cp "$bin/TiloPhone" "$app/$program"
  cp -R "$bundle/Settings.bundle" "$app/"
  # The two files Phone-Info.plist names as the app's icon.
  for scale in 2 3; do
    draw "$made/iphone/icon.png" "$((60 * scale))"
    # An icon on a phone carries no transparency.
    sips -s format jpeg -s formatOptions best "$made/iphone/icon.png" --out "$made/iphone/icon.jpg" >/dev/null
    sips -s format png "$made/iphone/icon.jpg" --out "$app/AppIcon60x60@${scale}x.png" >/dev/null
  done
  rm -f "$made/iphone/icon.png" "$made/iphone/icon.jpg"
  phone_part "$app" "$id" "$bundle/Phone-Info.plist" "$bundle/Phone.entitlements" "$TILO_APPLE_PROFILE" app
  codesign --verify --deep --strict "$app"
}

# Onto the one paired iPhone that is reachable, or the one TILO_APPLE_DEVICE names. The app is
# installed and not opened.
install_iphone() {
  local device="${TILO_APPLE_DEVICE:-}" list="$made/iphone/devices.json"
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
  xcrun devicectl device install app --device "$device" "$made/iphone/$name.app"
}

# The iPhone app in a simulator, asked what it can tell. No simulator has a graphics adapter,
# so none can show Tilo: this says whether the app starts on a phone's system, whether the
# script that joins page and system works in a phone's web view, whether the page's key is
# kept, and whether the site answers. The simulator runs without its window and is shut down
# again if this started it.
check_simulator() {
  local bin app="$made/simulator/$name.app" device started="" failed="" run
  # The team its entitlements name, so the app finds the keychain group they grant.
  local team=SIMULATOR0
  rm -rf "$made/simulator"
  mkdir -p "$app"
  fill "$bundle/Phone.entitlements" "$made/simulator/rights.plist"
  bin="$(products simulator)"
  cp "$bin/TiloPhone" "$app/$program"
  fill "$bundle/Phone-Info.plist" "$app/Info.plist"
  stamp "$app/Info.plist" app
  plist -c "Set :CFBundleSupportedPlatforms:0 iPhoneSimulator" -c "Delete :CFBundleIcons" "$app/Info.plist"
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
        xcrun simctl launch --console --terminate-running-process "$device" "$id" \
          --check --quit "$@" > "$made/simulator/$run.txt" 2>&1 || true
      done
  } || failed=1
  xcrun simctl uninstall "$device" "$id" >/dev/null 2>&1 || true
  [ -z "$started" ] || xcrun simctl shutdown "$device" >/dev/null 2>&1 || true
  [ -z "$failed" ] || die "the app could not be put into the simulator"
  grep '^CHECK ' "$made/simulator/second.txt" | sed 's/^CHECK //' | grep -v '^end$' \
    || die "the app said nothing in the simulator"
  ! grep '^CHECK FAIL' "$made/simulator/second.txt" | grep -qv 'WebGPU'
}

# A test reads this file for its functions (`source build.sh`): it builds nothing then.
[ "${BASH_SOURCE[0]}" = "$0" ] || return 0

command="${1:-}"
[ $# -eq 0 ] || shift
# A build says which build it is before anything is compiled for it.
case "$command" in
  mac | iphone | simulator) built_from ;;
esac
case "$command" in
  test)
    (cd "$here" && xcrun swift test --scratch-path "$out/swiftpm-mac")
    ;;
  mac)
    build_mac
    say "built $made/mac/$name.app"
    case "${1:-}" in
      --check)
        shift
        check_mac "$@"
        ;;
      "") ;;
      *) usage ;;
    esac
    ;;
  iphone)
    build_iphone
    say "built $made/iphone/$name.app"
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
