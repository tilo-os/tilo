#!/usr/bin/env bash
# Lint with clippy the apps that ship an Android host, for both Android
# targets, with the `android` feature their host example builds with.
#
#   tools/ci/clippy-android.sh [-p <app>]...   # default: every Android app
#   CARGO=<any cargo drop-in> tools/ci/clippy-android.sh
#
# check.yml's `android` job runs it with the selected apps; the targets'
# linkers come from the NDK (tools/ci/install-android-ndk.sh).
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo=${CARGO:-cargo}
if [ "$#" -eq 0 ]; then
  # shellcheck disable=SC2046
  set -- $(python3 tools/ci/package_sets.py android)
fi
for target in aarch64-linux-android x86_64-linux-android; do
  # missing_const_for_thread_local fires on the Android targets for
  # thread-locals that already use `const { … }`, a false positive.
  bash tools/ci/measure.sh "clippy $target" -- \
    bash scripts/ci-run.sh "Clippy (Android apps, $target)" -- \
    "$cargo" clippy --locked "$@" --examples --features android --target "$target" \
    -- -D warnings -A clippy::missing_const_for_thread_local
done
