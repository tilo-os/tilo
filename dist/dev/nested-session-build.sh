#!/usr/bin/env bash
# Build everything dist/dev/nested-session.sh runs, into one directory: the
# host programs (shell, launcher, installer, packager, runtime-bundle tool,
# model gateway, tilo-control, the native-room governor fixture), the
# workspace host and room service a local site runs (nested-pair.sh), the thin
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

# The thin apps' graph shares no compiled unit with the host programs' (its
# profile, features and wrapper differ), and each build spends minutes on a few
# large crates (tilo-ui, the shell) with most cores idle: one after another, the
# job used a third of its cores. So the thin build runs beside the host and
# site builds, under one jobserver holding the job's cores: together they never
# run more compilers (or take more memory) than one build of that width would.
jobs=${CARGO_BUILD_JOBS:-$(nproc)}
mkdir -p "$target"
jobserver=$target/nested-session.jobserver
rm -f "$jobserver"
mkfifo "$jobserver"
exec 9<>"$jobserver"
# Each cargo keeps one implicit token; the fifo holds the rest.
if (( jobs > 2 )); then head -c $((jobs - 2)) /dev/zero | tr '\0' + >&9; fi
export MAKEFLAGS="-j$jobs --jobserver-auth=fifo:$jobserver"
# Per-unit timings of every build, staged beside the programs: what the
# session's rebuild spends its time on.
timings=$target/cargo-timings
rm -rf "$timings"

# --- Host programs -----------------------------------------------------------
# Release optimisation without the shipped profile's fat LTO and single
# codegen unit, whose link alone takes tens of minutes on every shell change.
# No line tables: release strips debug information from the binaries anyway.
# `dev-overrides` lets the shell mint a throwaway identity and serve the
# control socket, and lets the launcher and installer take the runtime and
# trust inputs from the session's state directory. Never ship it.
profile_args() {
    printf '%s\n' --profile "$1" \
        --config "profile.$1.inherits=\"release\"" --config "profile.$1.lto=false" \
        --config "profile.$1.codegen-units=16" --config "profile.$1.incremental=true" \
        --config "profile.$1.debug=false"
}
# Each line of a build's output carries its name, as the builds interleave.
label() { sed -u "s/^/$1: /"; }
host_profile=nested
host_bins=(shell tilo-launcher tilo-install tilo-app-package tilo-runtime-bundle
           tilo-model-gateway tilo-control)
build_host() {
    local args
    mapfile -t args < <(profile_args "$host_profile")
    cargo build --locked --timings "${args[@]}" \
        -p shell --bin shell --example packaged_native_governor --features shell/dev-overrides \
        -p launcher --bin tilo-launcher --features launcher/dev-overrides \
        -p install --bin tilo-install \
        -p tilo-app-package-cli --bin tilo-app-package \
        -p runtime-bundle --bin tilo-runtime-bundle \
        -p tilo-model-gateway --bin tilo-model-gateway \
        -p tilo-control --bin tilo-control
}
# The workspace host and the room service in front of it, as the room
# service's image builds them (tilo-relay with its agents): the local site
# dist/dev/nested-pair.sh runs, so two nested sessions meet in its home
# workspace the way two computers meet in a community's. A second build, so
# the relay's features never unify into the shell's dependencies; after the
# host build and in its profile, so the units both resolve the same way (most
# of the toolkit and the agent crates) are compiled once.
site_bins=(tilo-relay tilo-room-service)
build_site() {
    local args
    mapfile -t args < <(profile_args "$host_profile")
    cargo build --locked --timings "${args[@]}" \
        -p tilo-relay --bin tilo-relay --features tilo-relay/agents \
        -p tilo-room-service --bin tilo-room-service
}

# --- Thin apps and the shared runtime -----------------------------------------
# Resolved against the default collection's runtime, so the runtime is the one
# every app of the collection links, whichever of them the session installs.
selected=()
for app in $apps; do selected+=("$app"); done
packages=$(python3 tools/apps/app_release.py --apps "${selected[*]}" | jq -r '.packages | join(" ")')
runtime_plan=$(python3 tools/apps/app_release.py --collection default)
build_thin() {
    INCLUDE_PACKAGES=$(jq -r '.packages | join(" ")' <<<"$runtime_plan") \
        BUILD_PACKAGES=$packages \
        APP_FEATURES=$(jq -r '.features | join(",")' <<<"$runtime_plan") \
        TARGET_DIR=$target \
        EXTRA_CARGO_ARGS="${EXTRA_CARGO_ARGS:+$EXTRA_CARGO_ARGS }--timings" \
        os/scripts/build-thin-apps.sh
}

pids=()
(build_host 2>&1 | label host && build_site 2>&1 | label site) & pids+=($!)
(build_thin 2>&1 | label thin) & pids+=($!)
failed=0
for pid in "${pids[@]}"; do wait "$pid" || failed=1; done
exec 9<&-
rm -f "$jobserver"
unset MAKEFLAGS
(( failed == 0 )) || { echo "a nested-session build failed (see above)" >&2; exit 1; }

# --- Stage ----------------------------------------------------------------------
rm -rf "$out"
mkdir -p "$out/bin/examples" "$out/thin-app"
for name in "${host_bins[@]}" "${site_bins[@]}"; do cp -p "$target/$host_profile/$name" "$out/bin/"; done
cp -p "$target/$host_profile/examples/packaged_native_governor" "$out/bin/examples/"
cp -rp "$timings" "$out/timings"
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
