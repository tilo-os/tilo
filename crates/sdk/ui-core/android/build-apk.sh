#!/usr/bin/env bash
# Build an installable APK for a Tilo app.
#
#   crates/sdk/ui-core/android/build-apk.sh <app-package> [--abi ABI]... [--release]
#       [--label LABEL] [--application-id ID]
#
# ABI is arm64-v8a or x86_64 (default: both). The app's Rust library is its
# `<app>-android` cdylib example (see `ui::android_main!`), built with `$CARGO`
# (default `cargo`). The Java side is TiloActivity alone, compiled against the
# pinned platform and packed with the pinned build-tools; no Gradle.
#
# Output: target/android/<app>/<profile>/<app>.apk, signed with a debug key
# generated once under target/android/ (or $TILO_ANDROID_KEYSTORE, with
# $TILO_ANDROID_KEYSTORE_PASS and $TILO_ANDROID_KEY_ALIAS, for a real key).
set -euo pipefail

BUILD_TOOLS_VERSION=36.1.0
PLATFORM=android-36
MIN_SDK=29
TARGET_SDK=36
JAVA_RELEASE=11
# Every staged file gets this mtime so the archive does not depend on when it
# was built (1980-01-01, the earliest time a zip entry can hold).
EPOCH=315532800

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$here/../../../.." && pwd)

die() {
    echo "build-apk: $*" >&2
    exit 1
}

usage() {
    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed -e '$d' -e 's/^# \{0,1\}//'
    exit "${1:-0}"
}

app=""
abis=()
release=0
label=""
application_id=""
while (($#)); do
    case "$1" in
    --abi) abis+=("${2:?--abi needs a value}"); shift 2 ;;
    --release) release=1; shift ;;
    --label) label=${2:?--label needs a value}; shift 2 ;;
    --application-id) application_id=${2:?--application-id needs a value}; shift 2 ;;
    -h | --help) usage ;;
    -*) die "unknown option $1" ;;
    *) [[ -z $app ]] || die "one app per run"; app=$1; shift ;;
    esac
done
[[ -n $app ]] || usage 2
((${#abis[@]})) || abis=(arm64-v8a x86_64)
[[ -n $label ]] || label="${app^}"
[[ -n $application_id ]] || application_id="so.tilo.${app//-/_}"

sdk=${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Android/Sdk}}
tools="$sdk/build-tools/$BUILD_TOOLS_VERSION"
android_jar="$sdk/platforms/$PLATFORM/android.jar"
[[ -x $tools/aapt2 ]] || die "missing build-tools $BUILD_TOOLS_VERSION: sdkmanager 'build-tools;$BUILD_TOOLS_VERSION'"
[[ -f $android_jar ]] || die "missing $PLATFORM: sdkmanager 'platforms;$PLATFORM'"
command -v javac >/dev/null || die "javac (JDK 17 or newer) is not on PATH"
command -v zip >/dev/null || die "zip is not on PATH"

cargo=${CARGO:-cargo}

# The NDK's llvm-strip, for dropping DWARF from debug libraries (the symbol
# table stays, so backtraces still name functions).
strip=""
for ndk in "${ANDROID_NDK_HOME:-}" "$sdk"/ndk/*; do
    candidate="$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip"
    [[ -x $candidate ]] && strip=$candidate
done

profile=debug
cargo_profile=()
if ((release)); then
    profile=release
    cargo_profile=(--release)
fi

example="$app-android"
lib_name="${example//-/_}"
out="$root/target/android/$app/$profile"
stage="$out/stage"
rm -rf "$stage"
mkdir -p "$stage/classes" "$stage/apk" "$out"

for abi in "${abis[@]}"; do
    case "$abi" in
    arm64-v8a) triple=aarch64-linux-android ;;
    x86_64) triple=x86_64-linux-android ;;
    *) die "unsupported ABI $abi (arm64-v8a or x86_64)" ;;
    esac
    (cd "$root" && "$cargo" build -p "$app" --example "$example" --features android \
        --target "$triple" --locked "${cargo_profile[@]}")
    built="$root/target/$triple/$profile/examples/lib$lib_name.so"
    [[ -f $built ]] || die "$built was not produced; does $app declare the $example example?"
    mkdir -p "$stage/apk/lib/$abi"
    if [[ -n $strip ]]; then
        "$strip" --strip-debug -o "$stage/apk/lib/$abi/lib$lib_name.so" "$built"
    else
        cp "$built" "$stage/apk/lib/$abi/"
    fi
done

mapfile -t java_sources < <(find "$here/java" -name '*.java' | sort)
javac --release "$JAVA_RELEASE" -classpath "$android_jar" -d "$stage/classes" \
    -Xlint:all,-options -Werror "${java_sources[@]}"
mapfile -t classes < <(find "$stage/classes" -name '*.class' | sort)
d8_mode=--debug
((release)) && d8_mode=--release
"$tools/d8" "$d8_mode" --min-api "$MIN_SDK" --lib "$android_jar" --output "$stage/apk" "${classes[@]}"

debuggable=true
((release)) && debuggable=false
version_code=$(git -C "$root" rev-list --count HEAD 2>/dev/null || echo 1)
version_name=$(git -C "$root" describe --always --dirty 2>/dev/null || echo dev)
sed -e "s|@PACKAGE@|$application_id|g" \
    -e "s|@LABEL@|$label|g" \
    -e "s|@LIB_NAME@|$lib_name|g" \
    -e "s|@DEBUGGABLE@|$debuggable|g" \
    -e "s|@VERSION_CODE@|$version_code|g" \
    -e "s|@VERSION_NAME@|$version_name|g" \
    "$here/AndroidManifest.xml.in" >"$stage/AndroidManifest.xml"

# The font catalog every native renderer loads; the app copies it out of the
# APK into its files directory on first start.
fonts="$root/crates/sdk/ui-core/assets/fonts"
mkdir -p "$stage/assets/tilo"
cp -r "$fonts" "$stage/assets/tilo/fonts"

find "$stage" -exec touch -h -d "@$EPOCH" {} +
"$tools/aapt2" link -o "$stage/linked.apk" -I "$android_jar" \
    --manifest "$stage/AndroidManifest.xml" \
    --min-sdk-version "$MIN_SDK" --target-sdk-version "$TARGET_SDK" \
    -A "$stage/assets"
(cd "$stage/apk" && find . -type f | sort | sed 's|^\./||' | zip -q -X -D "$stage/linked.apk" -@)
"$tools/zipalign" -f -P 16 4 "$stage/linked.apk" "$stage/aligned.apk"

keystore=${TILO_ANDROID_KEYSTORE:-$root/target/android/debug.keystore}
pass=${TILO_ANDROID_KEYSTORE_PASS:-android}
alias=${TILO_ANDROID_KEY_ALIAS:-androiddebugkey}
if [[ ! -f $keystore ]]; then
    [[ -z ${TILO_ANDROID_KEYSTORE:-} ]] || die "$keystore does not exist"
    keytool -genkeypair -keystore "$keystore" -storepass "$pass" -keypass "$pass" \
        -alias "$alias" -keyalg RSA -keysize 2048 -validity 10000 \
        -dname "CN=Tilo Debug, O=Tilo" >/dev/null
fi
"$tools/apksigner" sign --ks "$keystore" --ks-pass "pass:$pass" --key-pass "pass:$pass" \
    --ks-key-alias "$alias" --out "$out/$app.apk" "$stage/aligned.apk"
echo "$out/$app.apk"
