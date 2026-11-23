#!/usr/bin/env bash
# Draw every screen of Tilo Assistant to pictures, with nothing on screen: the app's own views
# with the content of the window's drawing (Sources/AssistantDrawing), light and dark.
#
#   dist/apple/assistant-shots.sh OUT          the Mac's screens into OUT/mac, the iPhone's into OUT/phone
#   dist/apple/assistant-shots.sh OUT mac      the Mac's alone: drawn by this Mac, in a window never put on a screen
#   dist/apple/assistant-shots.sh OUT phone    the iPhone's alone: drawn in a simulator that shows no window
#   dist/apple/assistant-shots.sh OUT told     what the system is handed outside the window, in the same
#                                              simulator: the notifications with their buttons and the live
#                                              activities, as OUT/phone/told.txt and two pictures of its screen
#   dist/apple/assistant-shots.sh OUT said     what every screen of the iPhone says of itself to someone who
#                                              cannot see it, in the order it is read: OUT/phone/said-*.txt
#   dist/apple/assistant-shots.sh OUT secret   the protected field with something typed into it, in front and as
#                                              the app leaves the front: OUT/phone/secret.txt and its pictures
#   dist/apple/assistant-shots.sh OUT typing   the main chat with the keyboard up, as the whole screen shows it
#   dist/apple/assistant-shots.sh OUT place    whether a chat shown for the first time rests at its end, and
#                                              where each of its messages says it stands to someone who
#                                              cannot see it, against where a held finger reaches it
#                                              (OUT/phone/place.txt; a line FAULT fails it)
#   dist/apple/assistant-shots.sh OUT keyboard the main chat with the keyboard up, put away and up again, light
#                                              and dark: what a drag on the conversation does to the keyboard,
#                                              what the window draws behind the keyboard's upper corners,
#                                              what Up, Down and Escape of an attached keyboard do in the
#                                              field and what is kept (OUT/phone/keyboard-*.txt; a line FAULT fails
#                                              it), and the whole screen with the keyboard up and with it away
#
# The simulator is one of its own, made when it is missing and shut down again after.
#
# Environment:
#   TILO_APPLE_SIGNING_IDENTITY  who signs the Mac's picture tool. A program started on this Mac is
#                                signed by someone; without a name the Mac's pictures are not drawn.
#   TILO_SHOTS_SIMULATOR         the simulator's name (default assistant-shots)
#   TILO_SHOTS_DEVICE            the kind of iPhone (default "iPhone 18 Pro")
#   TILO_SHOTS_TEXT              the text size the phone is set to, as simctl names it (default large)
#   TILO_BUILD_JOBS              how many compilers run at once (default: as many as the Mac has cores)
#   DEVELOPER_DIR                the Xcode to build with, when it is not the selected one
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
here="$repo/dist/apple"
made="$repo/target/apple.noindex/assistant-simulator"
fonts="$repo/target/apple.noindex/assistant-fonts"
id=so.tilo.assistant.shots

say() { printf 'assistant-shots: %s\n' "$*" >&2; }
die() { say "$*"; exit 1; }

[ $# -ge 1 ] || { sed -n '2,/^set /p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//' >&2; exit 2; }
mkdir -p "$1"
out="$(cd "$1" && pwd)"
what="${2:-all}"
simulator_name="${TILO_SHOTS_SIMULATOR:-assistant-shots}"
jobs=()
[ -z "${TILO_BUILD_JOBS:-}" ] || jobs=(-j "$TILO_BUILD_JOBS")

# However a run ends, its simulator does not stay up after it.
closing() {
  [ "$what" != mac ] || return 0
  local up
  up="$(xcrun simctl list devices 2>/dev/null | sed -n "s/^ *$simulator_name (\([0-9A-F-]*\)) (Booted).*/\1/p" | head -1)"
  [ -z "$up" ] || xcrun simctl shutdown "$up" >/dev/null 2>&1 || true
}
trap closing EXIT

# Tilo Sans, the face the window is drawn in. The repository keeps no font: its font tool
# prepares the catalogue once, outside the sources.
faces() {
  local catalogue="$repo/target/prepared-fonts" face
  if [ ! -f "$catalogue/TiloSans[wdth,wght].ttf" ]; then
    PATH="/opt/homebrew/bin:$PATH" python3 "$repo/tools/font-catalog.py" prepare \
      "$repo/crates/ui/ui-core/assets/fonts" "$catalogue" --scope product >&2 || die "Tilo Sans could not be prepared"
  fi
  mkdir -p "$fonts"
  for face in "TiloSans[wdth,wght].ttf" "TiloSans-Italic[wdth,wght].ttf"; do cp "$catalogue/$face" "$fonts/"; done
}

mac() {
  local scratch="$repo/target/apple.noindex/swiftpm-mac"
  # Nothing that nobody signed is started on this Mac: the system holds such a program at its start.
  [ -n "${TILO_APPLE_SIGNING_IDENTITY:-}" ] || die "name who signs the picture tool in TILO_APPLE_SIGNING_IDENTITY"
  (cd "$here" && xcrun swift build ${jobs[@]+"${jobs[@]}"} --scratch-path "$scratch" --product AssistantShots >&2)
  codesign --force --timestamp=none --sign "$TILO_APPLE_SIGNING_IDENTITY" "$scratch/debug/AssistantShots" >&2
  "$scratch/debug/AssistantShots" --out "$out/mac" --fonts "$fonts"
}

# The picture tool as an app of its own for a simulator, with the assistant's widget extension
# inside it, so the live activity is drawn by the very extension the assistant's app carries.
bundle() {
  local sdk bin app="$made/Assistant Shots.app" appex
  sdk="$(xcrun --sdk iphonesimulator --show-sdk-path)"
  local flags=(-c release ${jobs[@]+"${jobs[@]}"} --triple "$(uname -m)-apple-ios26.4-simulator" --sdk "$sdk" --scratch-path "$made/swiftpm")
  # A build that fails ends the script: what an earlier build left is not the tool it was asked for.
  (cd "$here" && xcrun swift build "${flags[@]}" >&2) || die "the picture tool did not build for the simulator"
  bin="$(cd "$here" && xcrun swift build "${flags[@]}" --show-bin-path)"
  appex="$app/PlugIns/AssistantWidgets.appex"
  rm -rf "$app"
  mkdir -p "$appex"
  cp "$bin/AssistantShots" "$app/AssistantShots"
  cp -R "$here/AssistantBundle/en.lproj" "$app/"
  cp -R "$fonts" "$app/Fonts"
  cat > "$app/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>Assistant Shots</string>
  <key>CFBundleExecutable</key><string>AssistantShots</string>
  <key>CFBundleIdentifier</key><string>$id</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>Assistant Shots</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.0.0</string>
  <key>CFBundleSupportedPlatforms</key><array><string>iPhoneSimulator</string></array>
  <key>CFBundleVersion</key><string>0.0.0</string>
  <key>LSRequiresIPhoneOS</key><true/>
  <key>MinimumOSVersion</key><string>26.4</string>
  <key>NSSupportsLiveActivities</key><true/>
  <key>UIApplicationSceneManifest</key><dict>
    <key>UIApplicationSupportsMultipleScenes</key><false/>
  </dict>
  <key>UIDeviceFamily</key><array><integer>1</integer></array>
  <key>UILaunchScreen</key><dict/>
</dict></plist>
PLIST
  cp "$bin/AssistantWidgets" "$appex/AssistantWidgets"
  cp -R "$here/AssistantBundle/en.lproj" "$appex/"
  cp -R "$fonts" "$appex/Fonts"
  sed -e "s/__NAME__/Assistant Shots/g" -e "s/__ID__/$id/g" "$here/AssistantBundle/Widgets-Info.plist" > "$appex/Info.plist"
  /usr/libexec/PlistBuddy -c "Set :CFBundleSupportedPlatforms:0 iPhoneSimulator" "$appex/Info.plist"
  codesign --force --timestamp=none --sign - "$appex"
  codesign --force --timestamp=none --sign - "$app"
  printf '%s\n' "$app"
}

# The simulator of its own, booted with no window.
simulator() {
  local device
  device="$(xcrun simctl list devices -j | python3 -I -c '
import json, sys
found = [d["udid"] for devices in json.load(sys.stdin)["devices"].values() for d in devices
         if d["name"] == sys.argv[1] and d.get("isAvailable")]
print(found[0] if found else "")
' "$simulator_name")"
  [ -n "$device" ] || device="$(xcrun simctl create "$simulator_name" "${TILO_SHOTS_DEVICE:-iPhone 18 Pro}")"
  xcrun simctl boot "$device" 2>/dev/null || true
  xcrun simctl bootstatus "$device" -b >/dev/null
  xcrun simctl status_bar "$device" override --time "9:41" >/dev/null 2>&1 || true
  xcrun simctl ui "$device" content_size "${TILO_SHOTS_TEXT:-large}" >/dev/null 2>&1 || true
  printf '%s\n' "$device"
}

phone() {
  local app device container
  app="$(bundle)" || exit 1
  device="$(simulator)"
  mkdir -p "$out/phone"
  xcrun simctl install "$device" "$app"
  # The tool draws every screen into its documents and ends.
  xcrun simctl launch --console --terminate-running-process "$device" "$id" > "$out/phone/drawn.txt" 2>&1 || true
  grep '^SHOTS ' "$out/phone/drawn.txt" || die "the tool drew nothing: see $out/phone/drawn.txt"
  container="$(xcrun simctl get_app_container "$device" "$id" data)"
  cp "$container"/Documents/*.png "$out/phone/"
  grep -q '^SHOTS done' "$out/phone/drawn.txt" || die "the tool did not finish"
}

told() {
  local app device waited=0
  app="$(bundle)" || exit 1
  device="$(simulator)"
  mkdir -p "$out/phone"
  xcrun simctl install "$device" "$app"
  rm -f "$(xcrun simctl get_app_container "$device" "$id" data)"/Documents/*.txt 2>/dev/null || true
  xcrun simctl launch --terminate-running-process "$device" "$id" --tell >/dev/null
  # The tool keeps what it says in its documents, and stays, so its screen can be taken.
  local kept
  kept="$(xcrun simctl get_app_container "$device" "$id" data)/Documents/told.txt"
  until grep -q '^TELL done' "$kept" 2>/dev/null; do
    waited=$((waited + 1))
    [ "$waited" -lt 90 ] || { say "the tool did not finish telling"; break; }
    sleep 1
  done
  rm -f "$out/phone/told-all.txt"
  cp "$kept" "$out/phone/told.txt" 2>/dev/null || die "the tool said nothing"
  cat "$out/phone/told.txt"
  xcrun simctl io "$device" screenshot "$out/phone/told-in-the-app.png" >/dev/null 2>&1
  # Another app in front: the live activities stand in the island.
  xcrun simctl launch "$device" com.apple.Preferences >/dev/null 2>&1 || true
  sleep 4
  xcrun simctl io "$device" screenshot "$out/phone/told-behind-another-app.png" >/dev/null 2>&1
  xcrun simctl terminate "$device" "$id" >/dev/null 2>&1 || true
  xcrun simctl terminate "$device" com.apple.Preferences >/dev/null 2>&1 || true
}

# The picture tool put into the simulator and started in one of its ways, left running.
started() { # the way
  local app device
  app="$(bundle)" || exit 1
  device="$(simulator)"
  mkdir -p "$out/phone"
  xcrun simctl install "$device" "$app"
  # What an earlier run kept is not this run's word.
  rm -f "$(xcrun simctl get_app_container "$device" "$id" data)"/Documents/*.txt 2>/dev/null || true
  xcrun simctl launch --terminate-running-process "$device" "$id" "$@" >/dev/null
  printf '%s\n' "$device"
}

# The tool, already in the simulator, started once more in a way.
again() { # device, the way
  local device="$1"
  shift
  rm -f "$(xcrun simctl get_app_container "$device" "$id" data)"/Documents/*.txt 2>/dev/null || true
  xcrun simctl launch --terminate-running-process "$device" "$id" "$@" >/dev/null
}

# Wait for a line in a file the tool keeps in its documents, and name the file.
waited() { # device, file, line
  local kept seconds=0
  kept="$(xcrun simctl get_app_container "$1" "$id" data)/Documents/$2"
  until grep -q "$3" "$kept" 2>/dev/null; do
    seconds=$((seconds + 1))
    [ "$seconds" -lt 60 ] || return 1
    sleep 1
  done
  printf '%s\n' "$kept"
}

said() {
  local app device container
  app="$(bundle)" || exit 1
  device="$(simulator)"
  mkdir -p "$out/phone"
  xcrun simctl install "$device" "$app"
  xcrun simctl launch --console --terminate-running-process "$device" "$id" --said > "$out/phone/said.txt" 2>&1 || true
  grep -q '^SAID done' "$out/phone/said.txt" || die "the tool did not finish reading: see $out/phone/said.txt"
  container="$(xcrun simctl get_app_container "$device" "$id" data)"
  cp "$container"/Documents/said-*.txt "$out/phone/"
}

secret() {
  local device kept container
  device="$(started --secret)"
  kept="$(waited "$device" secret.txt '^SECRET typed')" || die "nothing was typed into the protected field"
  xcrun simctl io "$device" screenshot "$out/phone/secret-screen-in-front.png" >/dev/null 2>&1
  # Another app in front: the system keeps a picture of the window the tool leaves behind.
  xcrun simctl launch "$device" com.apple.Preferences >/dev/null 2>&1 || true
  waited "$device" secret.txt '^SECRET done' >/dev/null || say "the tool did not say what it showed behind another app"
  xcrun simctl io "$device" screenshot "$out/phone/secret-screen-behind.png" >/dev/null 2>&1
  container="$(xcrun simctl get_app_container "$device" "$id" data)"
  cp "$kept" "$out/phone/secret.txt"
  cp "$container"/Documents/secret-*.png "$out/phone/" 2>/dev/null || true
  cat "$out/phone/secret.txt"
  xcrun simctl terminate "$device" "$id" >/dev/null 2>&1 || true
  xcrun simctl terminate "$device" com.apple.Preferences >/dev/null 2>&1 || true
}

typing() {
  local device
  device="$(started --typing)"
  sleep 7
  xcrun simctl io "$device" screenshot "$out/phone/typing.png" >/dev/null 2>&1
  xcrun simctl terminate "$device" "$id" >/dev/null 2>&1 || true
}

# Whether a chat rests at its end, and where each of its messages says it stands to someone who cannot
# see it against where a held finger reaches it, once for each way a chat comes to be shown for the
# first time: with the list of conversations under it (`chat`; `start`: as the app itself starts;
# `late`: its pieces arriving a second after; `active`: made once the app is in front), opened from
# that list (`list`), without the list (`alone`), the same on a chat longer than the screen (`-long`),
# and in the dark look (`-dark`).
# TILO_SHOTS_FIRST names the ways to show. A line FAULT is the tool's own refusal.
place() {
  local device="" container first firsts="${TILO_SHOTS_FIRST:-chat start late active list alone chat-long late-long alone-long start-dark}" wanted=0
  mkdir -p "$out/phone"
  : > "$out/phone/place.txt"
  for first in $firsts; do
    wanted=$((wanted + 1))
    if [ -z "$device" ]; then
      device="$(started --place "--first=$first")" || die "the tool was not started"
    else
      again "$device" --place "--first=$first"
    fi
    waited "$device" place.txt '^PLACE done' >/dev/null || say "the tool did not finish ($first)"
    container="$(xcrun simctl get_app_container "$device" "$id" data)"
    cat "$container/Documents/place.txt" >> "$out/phone/place.txt" 2>/dev/null || die "the tool said nothing of where a message stands ($first)"
    cp "$container"/Documents/place-*.png "$out/phone/" 2>/dev/null || true
    xcrun simctl terminate "$device" "$id" >/dev/null 2>&1 || true
  done
  cat "$out/phone/place.txt"
  [ "$(grep -c '^PLACE done' "$out/phone/place.txt")" = "$wanted" ] || die "where a message stands: the tool did not finish"
  ! grep -q '^PLACE FAULT' "$out/phone/place.txt" || die "where a message stands: a fault, named in the lines above"
}

# The keyboard up and put away again, in both looks. A line FAULT is the tool's own refusal.
keyboard() {
  local device kept look way
  for look in light dark; do
    device="$(simulator)"
    xcrun simctl ui "$device" appearance "$look" >/dev/null 2>&1 || true
    # The keyboard shows its keys, not the tip it shows a new person once.
    xcrun simctl spawn "$device" defaults write com.apple.keyboard.preferences DidShowContinuousPathIntroduction -bool true >/dev/null 2>&1 || true
    way=--keyboard
    [ "$look" != dark ] || way=--keyboard-dark
    device="$(started "$way")"
    kept="$(waited "$device" keyboard.txt '^KEYBOARD ready up')" || die "the tool did not bring the keyboard up"
    xcrun simctl io "$device" screenshot "$out/phone/keyboard-up-$look.png" >/dev/null 2>&1
    waited "$device" keyboard.txt '^KEYBOARD ready down' >/dev/null || die "the tool did not put the keyboard away"
    xcrun simctl io "$device" screenshot "$out/phone/keyboard-down-$look.png" >/dev/null 2>&1
    waited "$device" keyboard.txt '^KEYBOARD done' >/dev/null || say "the tool did not finish"
    cp "$kept" "$out/phone/keyboard-$look.txt"
    cat "$out/phone/keyboard-$look.txt"
    xcrun simctl terminate "$device" "$id" >/dev/null 2>&1 || true
  done
  xcrun simctl ui "$(simulator)" appearance light >/dev/null 2>&1 || true
  ! grep -q '^KEYBOARD FAULT' "$out/phone/keyboard-light.txt" "$out/phone/keyboard-dark.txt" || die "the keyboard: a fault, named in the lines above"
}

faces
case "$what" in
  all) mac; phone; told ;;
  mac) mac ;;
  phone) phone ;;
  told) told ;;
  said) said ;;
  secret) secret ;;
  typing) typing ;;
  place) place ;;
  keyboard) keyboard ;;
  *) die "mac, phone, told, said, secret, typing, place or keyboard" ;;
esac
if [ "$what" != mac ]; then
  xcrun simctl shutdown "$(simulator)" >/dev/null 2>&1 || true
fi
say "pictures are in $out"
