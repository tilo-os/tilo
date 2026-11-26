#!/usr/bin/env bash
# Launch the Tilo app's APK on an Android emulator and check it keeps drawing: it starts, draws
# its first frame, answers a swipe with a new one, and is still running with nothing fatal in
# its log. The app once aborted in its first redraw ("out of TLS keys"), which no test off
# Android could see.
#
#   tools/ci/android-launch.sh APK
#
# Needs ANDROID_HOME with emulator, platform-tools, cmdline-tools and the system image below,
# and /dev/kvm. The emulator draws with SwiftShader, so its Vulkan is a CPU one: no GPU is
# needed. What it saw (screenshots, logcat, the emulator's log) is left in $OUT.
set -euo pipefail

apk=${1:?the APK to launch}
# API 35: the emulator's images before it offer a Vulkan device that cannot present to a window,
# and the app draws with Vulkan alone on Android.
image="system-images;android-35;google_apis;x86_64"
package=so.tilo.phone
activity=so.tilo.android.TiloActivity
port=5560
out=${OUT:-$TMPDIR/android-launch}
mkdir -p "$out"

# The emulator refuses to start with less than about 7.4 GiB free beside its disks, whatever
# their size, though it writes about 2 GiB: $AVD_DIR when the scratch disk is smaller.
avd_dir=${AVD_DIR:-$TMPDIR}/android-user
rm -rf "$avd_dir"
export ANDROID_USER_HOME="$avd_dir" ANDROID_AVD_HOME="$avd_dir/avd"
export ANDROID_EMULATOR_HOME="$ANDROID_USER_HOME"
mkdir -p "$ANDROID_AVD_HOME"
sdk=$ANDROID_HOME
adb() { timeout 120 "$sdk/platform-tools/adb" -s "emulator-$port" "$@"; }

fail() {
  echo "android-launch: $*" >&2
  adb logcat -d -v brief >"$out/logcat.txt" 2>/dev/null || true
  if grep -qE "/(tilo|DEBUG|libc) " "$out/logcat.txt"; then
    echo "-- the app's log:" >&2
    grep -E "/(tilo|DEBUG|libc) " "$out/logcat.txt" | tail -60 >&2
  else
    echo "-- the emulator's log:" >&2
    tail -30 "$out/emulator.log" >&2
  fi
  exit 1
}

echo no | "$sdk/cmdline-tools/latest/bin/avdmanager" -s create avd --force -n launch -k "$image" -d pixel_6 >/dev/null
"$sdk/emulator/emulator" -avd launch -port "$port" -no-window -no-audio -no-boot-anim \
  -gpu swiftshader_indirect -no-snapshot -no-metrics >"$out/emulator.log" 2>&1 &
emulator=$!
cleanup() {
  adb emu kill >/dev/null 2>&1 || true
  wait "$emulator" 2>/dev/null || true
  "$sdk/platform-tools/adb" kill-server >/dev/null 2>&1 || true
  rm -rf "$avd_dir"
}
trap cleanup EXIT

timeout 300 "$sdk/platform-tools/adb" -s "emulator-$port" wait-for-device ||
  fail "the emulator did not come up"
# The device's own shell waits for the boot to finish.
timeout 300 "$sdk/platform-tools/adb" -s "emulator-$port" shell \
  'until [ "$(getprop sys.boot_completed)" = 1 ]; do sleep 1; done' ||
  fail "the emulator did not finish booting"

adb install -r "$apk" >/dev/null || fail "the APK did not install"
adb logcat -c
adb shell am start -n "$package/$activity" >"$out/start.txt" || fail "the activity did not start"
# The first frame: the app logs the window adapter it renders with as it starts drawing, and
# the frame reaches the screen a moment later. A blank screen compresses to a few kilobytes; the
# app's first screen does not.
timeout 300 "$sdk/platform-tools/adb" -s "emulator-$port" logcat -v brief -e "window adapter" -m 1 >/dev/null ||
  fail "the app opened no window to draw in"
shown=
for _ in $(seq 60); do
  adb exec-out screencap -p >"$out/first.png"
  if [ "$(stat -c %s "$out/first.png")" -gt 30000 ]; then
    shown=1
    break
  fi
done
adb shell input swipe 540 1800 540 600 300
adb shell input tap 540 1200
# The app redraws in its own time; a screenshot a few frames on shows what it drew.
adb shell sleep 5
adb exec-out screencap -p >"$out/later.png"
adb logcat -d -v brief >"$out/logcat.txt"

if grep -qE "out of TLS keys|Fatal signal" "$out/logcat.txt"; then fail "the app aborted"; fi
[ -n "$(adb shell pidof "$package")" ] || fail "the app is no longer running"
[ -n "$shown" ] || fail "the app showed no frame"
[ "$(sha256sum <"$out/first.png")" != "$(sha256sum <"$out/later.png")" ] ||
  fail "the app drew nothing after its first frame"
echo "android-launch: $package drew past its first frame and is running"
