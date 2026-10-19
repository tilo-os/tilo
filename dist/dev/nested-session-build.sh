#!/usr/bin/env bash
# Build everything dist/dev/nested-session.sh runs, into one directory: the
# host programs (shell, launcher, installer, packager, runtime-bundle tool,
# model gateway, tilo-control, the native-room governor fixture), the thin
# apps, the shared runtime library they link and its engines, the Rust
# standard library that runtime was linked against, and a dbus-daemon for
# hosts that have none.
#
# t.toml's `nested-session` check runs it on a t node, and the session script
# downloads its output with `t outputs nested-session`. The thin apps and
# libruntime_dylib.so must come from one Cargo graph (a Rust dylib's symbols
# carry crate hashes), so they travel together as one check's outputs rather
# than through `t cargo build`, which copies back programs only.
#
# Usage:
#   dist/dev/nested-session-build.sh OUT        build into OUT (replaced)
#   dist/dev/nested-session-build.sh --apps     print the apps it builds
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
# Directory names under apps/. The session installs these or a subset.
apps="agent notes code-editor word-processor chat settings sheets file-explorer browser terminal web-search"
if [[ ${1:-} == --apps ]]; then
    printf '%s\n' "$apps"
    exit 0
fi
out=$(realpath -m "${1:?usage: nested-session-build.sh OUT | --apps}")
cd "$repo_root"
target=$(realpath -m "${CARGO_TARGET_DIR:-target}")
# Build scripts fetch their test assets into the build directory; on t the
# sources are read-only.
export TILO_TEST_ASSET_CACHE=${TILO_TEST_ASSET_CACHE:-$target/test-assets}

# --- Host programs -----------------------------------------------------------
# Release optimisation without the shipped profile's fat LTO and single
# codegen unit, whose link alone takes tens of minutes on every shell change.
# `dev-overrides` lets the shell mint a throwaway identity and serve the
# control socket, and lets the launcher and installer take the runtime and
# trust inputs from the session's state directory. Never ship it.
profile=nested
host_bins=(shell tilo-launcher tilo-install tilo-app-package tilo-runtime-bundle
           tilo-model-gateway tilo-control)
cargo build --locked --profile "$profile" \
    --config "profile.$profile.inherits=\"release\"" --config "profile.$profile.lto=false" \
    --config "profile.$profile.codegen-units=16" --config "profile.$profile.incremental=true" \
    -p shell --bin shell --example packaged_native_governor --features shell/dev-overrides \
    -p launcher --bin tilo-launcher --features launcher/dev-overrides \
    -p install --bin tilo-install \
    -p tilo-app-package-cli --bin tilo-app-package \
    -p runtime-bundle --bin tilo-runtime-bundle \
    -p tilo-model-gateway --bin tilo-model-gateway \
    -p tilo-control --bin tilo-control

# --- Thin apps and the shared runtime -----------------------------------------
# Resolved against the default collection's runtime, so the runtime is the one
# every app of the collection links, whichever of them the session installs.
selected=()
for app in $apps; do selected+=("$app"); done
packages=$(python3 tools/apps/app_release.py --apps "${selected[*]}" | jq -r '.packages | join(" ")')
runtime_plan=$(python3 tools/apps/app_release.py --collection default)
INCLUDE_PACKAGES=$(jq -r '.packages | join(" ")' <<<"$runtime_plan") \
    BUILD_PACKAGES=$packages \
    APP_FEATURES=$(jq -r '.features | join(",")' <<<"$runtime_plan") \
    TARGET_DIR=$target \
    os/scripts/build-thin-apps.sh

# --- Stage ----------------------------------------------------------------------
rm -rf "$out"
mkdir -p "$out/bin/examples" "$out/thin-app"
for name in "${host_bins[@]}"; do cp -p "$target/$profile/$name" "$out/bin/"; done
cp -p "$target/$profile/examples/packaged_native_governor" "$out/bin/examples/"
cp -p "$target/thin-app/libruntime_dylib.so" "$target/thin-app/runtime-engines" "$out/thin-app/"
while IFS= read -r file; do
    cp -p "$target/thin-app/$file" "$out/thin-app/"
done < <(cat "$target/thin-app/runtime-engines"
         python3 tools/apps/app_release.py --apps "${selected[*]}" | jq -r '.catalog[].binary')
# The runtime bundle carries the exact libstd the runtime links, so the
# session needs no local Rust toolchain.
needed_std=$(readelf -dW "$out/thin-app/libruntime_dylib.so" \
    | sed -n 's/.*Shared library: \[\(libstd-[^]]*\)\].*/\1/p')
[[ -n $needed_std ]] || { echo "libruntime_dylib.so links no libstd" >&2; exit 1; }
cp -p "$(rustc --print target-libdir)/$needed_std" "$out/thin-app/"
# The session's private D-Bus: Tilo OS hosts run their user bus on
# dbus-broker and carry no dbus-daemon, which the nested session starts one
# of per shell (dist/dev/session-bus.sh). The builder's is the same Fedora
# release's and links only libraries every Tilo host has.
if dbus_daemon=$(command -v dbus-daemon); then
    cp -pL "$dbus_daemon" "$out/bin/dbus-daemon"
fi
printf 'staged the nested session in %s\n' "$out"
