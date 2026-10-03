#!/usr/bin/env bash
# Build an installable APK for an app in this checkout: a thin wrapper over
# `tilo-app build --platform android` (docs/android.md), kept for muscle memory.
#
#   crates/sdk/ui-core/android/build-apk.sh <app> [--abi arm64-v8a|x86_64]... [--release]
#
# ABI defaults to both. --release runs `tilo-app package --platform android`
# instead: a release-signed APK and bundle under the app's dist/. The label,
# application id, icon and permissions come from the app's tilo-app.toml.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)
die() { echo "build-apk.sh: $*" >&2; exit 2; }

app=""
abis=()
release=0
while (($#)); do
    case "$1" in
    --abi) abis+=("${2:?--abi needs a value}"); shift 2 ;;
    --release) release=1; shift ;;
    --label | --application-id) die "$1 is gone: set [package] name or id in tilo-app.toml" ;;
    -h | --help) sed -n '2,9s/^# \{0,1\}//p' "${BASH_SOURCE[0]}"; exit 0 ;;
    -*) die "unknown option $1" ;;
    *) [[ -z $app ]] || die "one app per run"; app=$1; shift ;;
    esac
done
[[ -n $app ]] || die "name an app, for example: build-apk.sh calculator --abi x86_64"
((${#abis[@]})) || abis=(arm64-v8a x86_64)
dir="$root/apps/$app"
[[ -d $dir ]] || die "no app at $dir"

for abi in "${abis[@]}"; do
    case "$abi" in
    arm64-v8a) arch=aarch64 ;;
    x86_64) arch=x86_64 ;;
    *) die "unknown ABI $abi (arm64-v8a or x86_64)" ;;
    esac
    if ((release)); then
        command=(package --platform android --arch "$arch" --force
            --output "$dir/dist/$app-android-$arch.tapp")
    else
        command=(build --platform android --arch "$arch")
    fi
    "${CARGO:-cargo}" run -q --manifest-path "$root/Cargo.toml" -p tilo-app-cli --bin tilo-app -- \
        "${command[@]}" "$dir"
done
