#!/usr/bin/env bash
# Lint with clippy, for both Android targets, the libraries `tilolang build
# --platform android` builds: the tilolang runtime's, and the Tilo app's
# (tilo-phone), which runs the device daemon in its own process and draws the
# toolkit.
#
#   CARGO=<any cargo drop-in> tools/ci/clippy-android.sh
#
# The targets' linkers come from the NDK (tools/ci/install-android-ndk.sh).
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo=${CARGO:-cargo}
sysroot=${ANDROID_NDK_HOME:?the NDK names its home}/toolchains/llvm/prebuilt/linux-x86_64/sysroot
for target in aarch64-linux-android x86_64-linux-android; do
  # QuickJS's bindings are generated with libclang, which needs the NDK's headers.
  export "BINDGEN_EXTRA_CLANG_ARGS_${target//-/_}=--sysroot=$sysroot"
  # missing_const_for_thread_local fires on the Android targets for
  # thread-locals that already use `const { … }`, a false positive.
  bash tools/ci/measure.sh "clippy $target" -- \
    bash scripts/ci-run.sh "Clippy (Android apps, $target)" -- \
    "$cargo" clippy --locked -p tilo-phone -p tilolang-process --features tilolang-process/app-launch \
    --lib --target "$target" \
    -- -D warnings -A clippy::missing_const_for_thread_local
done
