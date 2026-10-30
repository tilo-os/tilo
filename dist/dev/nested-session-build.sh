#!/usr/bin/env bash
# Build everything dist/dev/nested-session.sh runs, into one directory: the
# host programs (shell, launcher, installer, packager, runtime-bundle tool,
# model gateway, the ush supervisor, tilo-control, the native-room governor
# fixture), the `tilolang` CLI (`tilolang dev --session` installs into the
# session), the workspace host and room service a local site runs
# (nested-pair.sh), the thin apps, the tilolang runtime binary (apps/tilolang,
# which nested-session.sh bakes into the tilolang runtime bundle), the shared
# runtime library they link and
# its engines, the Rust standard library that runtime was linked against, the
# env-runtime image Terminal's environments start from, and the build
# environment's dbus-daemon (nested-session-bus) for hosts that have none.
#
# The thin apps and libruntime_dylib.so must come from one Cargo graph (a
# Rust dylib's symbols carry crate hashes), so one run of this script builds
# them together.
#
# Usage:
#   dist/dev/nested-session-build.sh [OUT]      build into OUT (replaced), by
#                                               default the target directory's
#                                               nested-session
#   dist/dev/nested-session-build.sh --apps     print the apps it builds
#
# The env-runtime is sealed from the tree t.toml's image-env-runtime-dev-tree-ARCH
# check builds from the tree's own RPM package set, as a run's dev images get
# theirs: TILO_ENV_RUNTIME_TREES names the directory holding
# env-runtime-tree-ARCH.tar. Without those trees the session takes the runtime
# published for this tree's definition (os/dev/resolve-env-runtime.sh), as a
# Tilo image of the tree ships it; with none published either, the session has
# no env-runtime and Terminal has no environment to start a shell in.
#
# With Browser among the apps, the Chromium engine it renders with is built
# too (scripts/browser/build-host.py, CMake and the pinned CEF SDK) and staged
# in chromium-engine/, which nested-session.sh signs as the engine runtime.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
# Directory names under apps/. The session installs these or a subset.
# TILO_NESTED_APPS builds fewer (a list of the same names, possibly empty):
# `tilolang run` needs none of them, only the runtime and the tilolang binary.
apps=${TILO_NESTED_APPS-"agent notes code-editor word-processor chat settings sheets tasks file-explorer browser terminal web-search mail pdf-viewer"}
if [[ ${1:-} == --apps ]]; then
    printf '%s\n' "$apps"
    exit 0
fi
(($# <= 1)) || { echo "usage: nested-session-build.sh [OUT] | --apps" >&2; exit 2; }
out=${1:+$(realpath -m "$1")}
cd "$repo_root"
arch=$(uname -m)
env_runtime_trees=
if [[ -n ${TILO_ENV_RUNTIME_TREES:-} ]]; then
    env_runtime_trees=$(realpath -m "$TILO_ENV_RUNTIME_TREES")
    [[ -f $env_runtime_trees/env-runtime-tree-$arch.tar && -f $env_runtime_trees/env-runtime-rpms-$arch.txt ]] \
        || { echo "no env-runtime tree for $arch in $env_runtime_trees" >&2; exit 1; }
else
    echo "TILO_ENV_RUNTIME_TREES is not set: the session gets the published env-runtime, if there is one" >&2
fi
target=$(realpath -m "${CARGO_TARGET_DIR:-target}")
out=${out:-$target/nested-session}
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
# trust inputs from the session's state directory (the installer installs the
# session's own builds, signed with its key, under the local-build size
# ceiling: package_store::size_limit). Never ship it.
profile_args() {
    printf '%s\n' --profile "$1" \
        --config "profile.$1.inherits=\"release\"" --config "profile.$1.lto=false" \
        --config "profile.$1.codegen-units=16" --config "profile.$1.incremental=true" \
        --config "profile.$1.debug=false"
}
# Each line of a build's output carries its name, as the builds interleave.
label() { sed -u "s/^/$1: /"; }
host_profile=nested
# tilo-runtime-mcp is the agent's end of the session's agent door
# (`tilo-runtime-mcp --connect`), so a check that drives the session as an
# admitted agent runs against the staged build without building its own.
host_bins=(shell tilo-runtime-mcp tilo-launcher tilo-install tilo-app-package tilo-runtime-bundle
           tilo-model-gateway tilo-ush tilo-control tilolang-host tilolang nested-session-bus)
build_host() {
    local args
    mapfile -t args < <(profile_args "$host_profile")
    # The shell, the launcher and the installer read the session's own
    # inputs (system interfaces, runtimes, trust) through the crates they
    # share, whose `dev-overrides` features unify only when the three build
    # together under the plain feature name: a `package/feature` form leaves
    # the shell reading the host's declarations through a remote builder.
    cargo build --locked --timings "${args[@]}" \
        -p shell --bin shell --bin tilo-runtime-mcp --example packaged_native_governor \
        -p launcher --bin tilo-launcher \
        -p install --bin tilo-install \
        --features dev-overrides || return 1
    cargo build --locked --timings "${args[@]}" \
        -p tilo-app-package-cli --bin tilo-app-package \
        -p runtime-bundle --bin tilo-runtime-bundle \
        -p ush --bin tilo-ush \
        -p tilo-control --bin tilo-control \
        -p nested-session-bus --bin nested-session-bus || return 1
    # The gateway, on a build of its own: its binary needs `server`, which no
    # longer comes from building beside the shell, and the session's gateway
    # runs with the features the workspace builds it with.
    cargo build --locked --timings "${args[@]}" \
        -p tilo-model-gateway --bin tilo-model-gateway \
        --features server,host-backend,local-models,runtime-client
}
# The tilolang host, in a build of its own: built with the shell, the features the shell
# turns on in the crates both use (media, the toolkit's services) would unify into it, and its
# binary would link libraries its sandbox, the tilolang runtime on the app runtime alone, does
# not hold.
build_tilolang_host() {
    local args
    mapfile -t args < <(profile_args "$host_profile")
    cargo build --locked --timings "${args[@]}" -p tilolang-host --bin tilolang-host
}
# The `tilolang` CLI authors run against the session (`tilolang dev --session`), in
# a build of its own for the same reason: its toolkit features stay out of the
# host programs and the tilolang host.
build_tilolang_cli() {
    local args
    mapfile -t args < <(profile_args "$host_profile")
    cargo build --locked --timings "${args[@]}" -p tilolang-cli --bin tilolang
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
# In the profile the apps ship in. The session installs its apps, signed with
# its own key, under the local-build size ceiling, so it can build them in the
# thin-dev profile (for build speed) once a session built so is proven;
# dist/dev/tilolang-runtime-build.sh must then build in the same profile, so
# its tilolang runtime links the library this session runs.
thin_profile=thin-app
selected=()
for app in $apps; do selected+=("$app"); done
packages=tilolang-app
if ((${#selected[@]})); then
    packages="$packages $(python3 tools/apps/app_release.py --apps "${selected[*]}" | jq -r '.packages | join(" ")')"
fi
runtime_plan=$(python3 tools/apps/app_release.py --collection default)
build_thin() {
    INCLUDE_PACKAGES=$(jq -r '.packages | join(" ")' <<<"$runtime_plan") \
        BUILD_PACKAGES=$packages \
        APP_FEATURES=$(jq -r '.features | join(",")' <<<"$runtime_plan") \
        TARGET_DIR=$target PROFILE=$thin_profile \
        EXTRA_CARGO_ARGS="${EXTRA_CARGO_ARGS:+$EXTRA_CARGO_ARGS }--timings" \
        os/scripts/build-thin-apps.sh
}

# The env-runtime Terminal's environments start from, sealed with a throwaway
# key into the layout os/scripts/install-env-runtime.sh installs from, as a
# run's dev images get theirs (os/scripts/ensure-dev-env-runtime.sh): the
# session ships it in a system store of its own, with that key as the store's
# image publisher, and the ush supervisor finds it there as on a Tilo device.
# Sealing takes minutes; it needs only the host build's packer, so it runs
# beside the site build rather than after everything.
env_runtime=$target/nested-session-env-runtime
seal_env_runtime() {
    rm -rf "$env_runtime"
    if [[ -z $env_runtime_trees ]]; then
        published_env_runtime
        return
    fi
    local began=$SECONDS
    bash tools/ci/publish/env-runtime.sh dev "$arch" "$env_runtime_trees" "$target/$host_profile" "$env_runtime"
    printf 'sealed the env-runtime in %s s\n' "$((SECONDS - began))"
}
# Without trees to seal, the runtime published for this tree's definition
# (os/dev/resolve-env-runtime.sh: the registry's image of that fingerprint,
# its signature checked) is the one a Tilo image of this tree ships: its
# /env-runtime directory is what install-env-runtime.sh installs from.
published_env_runtime() {
    local ref container
    if ! ref=$(bash os/dev/resolve-env-runtime.sh); then
        echo "no env-runtime is published for os/Containerfile.env-runtime and no trees were given: Terminal opens no shell" >&2
        return 0
    fi
    podman pull --quiet "$ref" >/dev/null || return 1
    container=$(podman create --quiet "$ref" true) || return 1
    podman cp "$container:/env-runtime" "$env_runtime"
    local copied=$?
    podman rm --force "$container" >/dev/null
    return "$copied"
}

# The Browser's Chromium engine (scripts/browser/build-host.py: the pinned CEF
# SDK and the native host), which the session signs as the
# `tilo-runtime/<RUNTIME_VERSION>` bundle Browser requires
# (ui::webview::chromium::runtime). Built again only when the CEF pin or the
# host's sources change.
chromium_engine=$target/nested-session-chromium
build_chromium_engine() {
    local key began=$SECONDS
    key=$( { cat scripts/browser/cef-lock.json scripts/browser/build-host.py
             git ls-files -s apps/browser/chromium crates/browser/tilo-chromium-blocker; } \
           | sha256sum | cut -c1-64)
    if [[ -f $chromium_engine/.key && $(<"$chromium_engine/.key") == "$key" \
          && -x $chromium_engine/runtime/chromium/tilo-chromium-host ]]; then
        echo "the Chromium engine is up to date"
        return 0
    fi
    rm -rf "$chromium_engine"
    mkdir -p "$chromium_engine"
    python3 scripts/browser/build-host.py --runtime "$chromium_engine/runtime" \
        --jobs "${CARGO_BUILD_JOBS:-$(nproc)}" || return 1
    printf '%s\n' "$key" > "$chromium_engine/.key"
    printf 'built the Chromium engine in %s s\n' "$((SECONDS - began))"
}
build_host_then_site() {
    build_host 2>&1 | label host || return 1
    local seal
    seal_env_runtime 2>&1 | label seal & seal=$!
    build_site 2>&1 | label site || { wait "$seal"; return 1; }
    build_tilolang_host 2>&1 | label tilolang-host || { wait "$seal"; return 1; }
    build_tilolang_cli 2>&1 | label tilolang || { wait "$seal"; return 1; }
    wait "$seal"
}

pids=()
build_host_then_site & pids+=($!)
(build_thin 2>&1 | label thin) & pids+=($!)
if [[ " $apps " == *" browser "* ]]; then
    (build_chromium_engine 2>&1 | label chromium) & pids+=($!)
fi
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
# Cargo writes the timings where it ran; a build forwarded to a builder leaves none here.
[[ ! -d $timings ]] || cp -rp "$timings" "$out/timings"
cp -p "$target/$thin_profile/libruntime_dylib.so" "$target/$thin_profile/runtime-engines" "$out/thin-app/"
while IFS= read -r file; do
    cp -p "$target/$thin_profile/$file" "$out/thin-app/"
done < <(cat "$target/$thin_profile/runtime-engines"
         echo tilolang-app
         ((${#selected[@]})) && python3 tools/apps/app_release.py --apps "${selected[*]}" | jq -r '.catalog[].binary')
[[ ! -d $env_runtime ]] || cp -rp "$env_runtime" "$out/env-runtime"
[[ ! -d $chromium_engine/runtime/chromium ]] || cp -rp "$chromium_engine/runtime/chromium" "$out/chromium-engine"
# The runtime bundle carries the exact libstd the runtime links, so the
# session needs no local Rust toolchain.
needed_std=$(readelf -dW "$out/thin-app/libruntime_dylib.so" \
    | sed -n 's/.*Shared library: \[\(libstd-[^]]*\)\].*/\1/p')
[[ -n $needed_std ]] || { echo "libruntime_dylib.so links no libstd" >&2; exit 1; }
cp -p "$(rustc --print target-libdir)/$needed_std" "$out/thin-app/"
# The session's private D-Bus: Tilo OS hosts run their user bus on
# dbus-broker and carry no dbus-daemon, which the nested session starts one
# of per shell (dist/dev/session-bus.sh). nested-session-bus carries the
# build environment's (the build image's, the same Fedora release's, linking
# only libraries every Tilo host has) inside a cargo-built executable, so it
# comes back with the other programs from a build on another machine. A build
# environment without one builds it empty, and it refuses to run.
"$out/bin/nested-session-bus" --version >/dev/null \
    || { echo "nested-session-bus carries no working dbus-daemon: build where dbus-daemon is installed" >&2; exit 1; }
printf 'staged the nested session in %s\n' "$out"
