#!/usr/bin/env bash
# Resolve tapp applications and the shared Rust/UI runtime in one Cargo
# graph. One graph is a correctness requirement: Rust dylib symbols include
# crate hashes, and independently resolving apps can otherwise produce thin
# binaries that expect different builds of libruntime_dylib.so.
set -euo pipefail

REPO="${REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
EXCLUDE_PACKAGES=" ${EXCLUDE_PACKAGES:-} "
INCLUDE_PACKAGES=" ${INCLUDE_PACKAGES:-} "
BUILD_PACKAGES=" ${BUILD_PACKAGES:-} "
APP_FEATURES="${APP_FEATURES:-}"
PROFILE="${PROFILE:-thin-app}"
BUILD_ROLE="${BUILD_ROLE:-apps}"
BUILD_PHASE="${BUILD_PHASE:-all}"
case "$BUILD_PHASE" in all|build|verify) ;; *) echo "invalid thin build phase" >&2; exit 1 ;; esac
case "$BUILD_ROLE" in apps|home-capture) ;; *) echo "invalid thin build role" >&2; exit 1 ;; esac
if [ "$BUILD_ROLE" = home-capture ]; then
    case "$BUILD_PACKAGES" in
        " imager "|" imager file-explorer word-processor "|" imager file-explorer word-processor browser ") ;;
        *) echo "home capture selects the Imager worker and optional ordinary Files/Writer/Browser" >&2; exit 1 ;;
    esac
fi
TARGET_DIR="${TARGET_DIR:-${CARGO_TARGET_DIR:-$REPO/target}}"

# TILO_THIN_KEEP_SYMBOLS=1 keeps the symbol tables of the apps, the runtime and its engines, for
# reading what makes an app large (the app-binaries check); every package is built the same way
# otherwise. Shipped packages are stripped.
strip=symbols
[ "${TILO_THIN_KEEP_SYMBOLS:-}" = 1 ] && strip=none
packages=()
features=()
binary_crates=()
binaries=()
build_targets=()
selected_crates=()
app_profiles=()
# A runtime bundle's binary (apps/*/tilo-runtime.toml: the tilolang runtime,
# which every tilolang app's entrypoint executes) links the same runtime as
# the apps, so it is built in their graph whichever apps are selected.
for manifest in "$REPO"/apps/*/tilo-app.toml "$REPO"/apps/*/tilo-runtime.toml; do
    [ -f "$manifest" ] || continue
    dir="$(dirname "$manifest")"
    package="$(sed -n 's/^name *= *"\([^"]*\)".*/\1/p' "$dir/Cargo.toml" | head -1)"
    binary="$(sed -n 's/^bin  *= *"bin\/\([^"]*\)".*/\1/p' "$manifest" | head -1)"
    [ -n "$package" ] && [ -n "$binary" ] \
        || { echo "could not read package/binary from $dir" >&2; exit 1; }
    case "$EXCLUDE_PACKAGES" in
        *" $package "*) continue ;;
    esac
    if [ "$INCLUDE_PACKAGES" != "  " ] && [ "${manifest##*/}" = tilo-app.toml ]; then
        case "$INCLUDE_PACKAGES" in
            *" $package "*) ;;
            *) continue ;;
        esac
    fi
    packages+=("-p" "$package")
    # Package overrides reach both the executable and app-owned libraries.
    # They are visible to Cargo and compiler caches before the wrapper runs;
    # matching only the binary crate name misses differently named libraries.
    app_profiles+=(--config "profile.$PROFILE.package.\"$package\".opt-level=\"z\""
                   --config "profile.$PROFILE.package.\"$package\".codegen-units=1"
                   --config "profile.$PROFILE.package.\"$package\".strip=\"$strip\"")
    features+=("$package/thin")
    if [ "$BUILD_ROLE" = home-capture ] && [ "$package" = imager ]; then
        binary=tilo-home-export
        features+=(imager/capture-worker)
    fi
    binary_crates+=("${binary//-/_}")
    if [ "$BUILD_PACKAGES" = "  " ] || [[ "$BUILD_PACKAGES" == *" $package "* ]]; then
        binaries+=("$binary")
        selected_crates+=("${binary//-/_}")
        build_targets+=(--bin "$binary")
    fi
done

[ "${#packages[@]}" -gt 0 ] || { echo "no tapp applications selected" >&2; exit 1; }

# Runtime engines (crates/runtime/engines/*): Rust dylibs chained on
# the runtime, each mapped only by the app that uses it, and shipped beside
# libruntime_dylib.so in the runtime bundle. The bundle must not depend on
# which apps this run selects, so every engine is built every time, through
# its `<package>-check` binary: Cargo builds a library only for a target that
# needs it, and `--lib` would build every app's library too.
engines=()
for manifest in "$REPO"/crates/runtime/engines/*/Cargo.toml; do
    [ -f "$manifest" ] || continue
    engine="$(sed -n 's/^name *= *"\([^"]*\)".*/\1/p' "$manifest" | head -1)"
    [ -n "$engine" ] || { echo "could not read the package name of $manifest" >&2; exit 1; }
    grep -Fq "name = \"$engine-check\"" "$manifest" \
        || { echo "runtime engine $engine has no $engine-check binary" >&2; exit 1; }
    engines+=("$engine")
    packages+=("-p" "$engine")
    features+=("$engine/thin")
    binary_crates+=("${engine//-/_}_check")
    build_targets+=(--bin "$engine-check")
done
[ "${#binaries[@]}" -gt 0 ] || { echo "no tapp binaries selected" >&2; exit 1; }
for requested in $BUILD_PACKAGES; do
    [[ " ${packages[*]} " == *" -p $requested "* ]] || {
        echo "requested app is outside the runtime graph: $requested" >&2; exit 1;
    }
done
feature_csv="$(IFS=,; echo "${features[*]}")"
# The collection's features name apps by package. An app this run leaves out
# (EXCLUDE_PACKAGES, or outside INCLUDE_PACKAGES: the image without Browser)
# is not selected, and Cargo refuses the whole build for a feature of a
# package it does not select, so its features are left out with it.
IFS=, read -r -a app_feature_pairs <<< "$APP_FEATURES"
for pair in ${app_feature_pairs[@]+"${app_feature_pairs[@]}"}; do
    case " ${packages[*]} " in
        *" -p ${pair%%/*} "*) feature_csv="$feature_csv,$pair" ;;
    esac
done
# The protected-grant probe must resolve the very SDK shipped for every app
# selection; building it separately would permit a different unified graph.
binary_crates+=(tilo_runtime_account_capability)
build_targets+=(--bin tilo-runtime-account-capability)
thin_crates="$(IFS=:; echo "${binary_crates[*]}")"

# Keep every package and feature in resolution, but select only requested binary
# targets. Cargo still builds their runtime dependency and gives it the same
# unified dependency features as a full collection build.
#
# Release jobs raise CARGO_PROFILE_RELEASE_* for faster fat links. thin-app
# inherits release, so those overrides would otherwise change every dependency
# compiled into the thin graph (and the wrapper can only reset the final tapp
# crate). Drop them for this invocation so image and CI thin apps share one
# size-oriented profile.
unset CARGO_PROFILE_RELEASE_CODEGEN_UNITS CARGO_PROFILE_RELEASE_LTO \
      CARGO_PROFILE_RELEASE_OPT_LEVEL CARGO_PROFILE_RELEASE_PANIC \
      CARGO_PROFILE_RELEASE_STRIP CARGO_PROFILE_RELEASE_DEBUG
#
# The size wrapper and the crates it applies to are Cargo configuration on the
# command line rather than environment, so they reach rustc through any Cargo
# front end, including one that builds on another machine and forwards only
# its own allowlist of variables.
cd "$REPO"
read -r -a extra_cargo_args <<< "${EXTRA_CARGO_ARGS:-}"
# A compiler cache must see the real compiler after the workspace wrapper adds
# its flags. Cargo runs RUSTC_WRAPPER first and hands it the workspace wrapper
# as the compiler, so a cache there keys a workspace crate by the arguments
# before the size flags: it served a crate compiled with the old flags after
# the wrapper changed them (t's rustc-wrapper caches library crates so). Every
# outer wrapper moves behind the workspace wrapper; sccache keeps its basename
# (thin-app/sccache) so cc-rs uses the same native compiler cache, any other
# goes through thin-app/rustc-wrapper. Outside this order, sccache 0.17 rejects
# an absolute compiler as a second input.
if [[ -n ${RUSTC_WRAPPER:-} && ${RUSTC_WRAPPER} != "$REPO"/tools/ci/thin-app/* ]]; then
    cache_path=$RUSTC_WRAPPER
    cache_config=$(python3 -c 'import json, sys; print(json.dumps(sys.argv[1]))' "$cache_path")
    export TILO_THIN_SCCACHE="$cache_path"
    if [[ $cache_path == */sccache || $cache_path == sccache ]]; then
        export RUSTC_WRAPPER="$REPO/tools/ci/thin-app/sccache"
    else
        export RUSTC_WRAPPER="$REPO/tools/ci/thin-app/rustc-wrapper"
    fi
    extra_cargo_args+=(--config "build.rustc-wrapper=\"$RUSTC_WRAPPER\""
                      --config "env.TILO_THIN_SCCACHE.value=$cache_config"
                      --config "env.TILO_THIN_SCCACHE.force=true")
fi
if [ "$BUILD_PHASE" != verify ] && [ -n "${SDK_COMPILER_UNITS:-}" ]; then
    extra_cargo_args+=(--message-format=json-render-diagnostics)
    # Keep the actual unit identities on the producing machine; no runtime bytes change.
    exec 3>&1
    exec > >(tee "$SDK_COMPILER_UNITS" >&3)
    sdk_compiler_stream_pid=$!
fi
keep_symbols=()
[ "$strip" = none ] && keep_symbols=(--config "profile.$PROFILE.strip=\"none\"")
if [ "$BUILD_PHASE" != verify ]; then
"${CARGO:-cargo}" build --locked --profile "$PROFILE" --features "$feature_csv" "${extra_cargo_args[@]}" \
    --config "build.rustc-workspace-wrapper=\"$REPO/tools/ci/rustc-thin-app-wrapper.sh\"" \
    --config "env.TILO_THIN_BIN_CRATES=\"$thin_crates\"" \
    "${app_profiles[@]}" ${keep_symbols[@]+"${keep_symbols[@]}"} \
    -p runtime-dylib "${packages[@]}" "${build_targets[@]}"
fi
if [ "$BUILD_PHASE" = build ]; then
    if [ -n "${SDK_COMPILER_UNITS:-}" ]; then
        exec 1>&3 3>&-
        wait "$sdk_compiler_stream_pid"
    fi
    printf 'thin graph compiled; native verification still required (%s)\n' "$BUILD_ROLE"
    exit 0
fi

# A Rust dylib mismatch is not reliably rejected by the ELF linker: symbol
# names include crate hashes, so an app built against a second feature-unified
# dependency can retain undefined symbols and fail only when the loader starts
# it. Resolve every selected binary against this exact runtime now, before it
# can be signed or published. `--help` is intentionally not assumed to be a
# supported app argument; reaching main (any status except the loader's 127)
# is enough for this ABI check.
rust_target_libdir=$(rustc --print target-libdir)
# An object's dynamic section, read whole before it is searched: with
# pipefail, `readelf | grep -q` fails whenever grep stops at its match while
# readelf is still writing (SIGPIPE), which reported a correctly built object
# as wrongly built.
dynamic_section() {
    readelf -dW "$1"
}
# The shared runtime's relocations run in every app process; the
# wrapper packs them. Without RELR it was linked without the wrapper.
grep -Fq '(RELR)' <<<"$(dynamic_section "$TARGET_DIR/$PROFILE/libruntime_dylib.so")" || {
    echo "libruntime_dylib.so was not linked through rustc-thin-app-wrapper.sh (no RELR)" >&2
    exit 1
}
# The runtime and its engines bind their own data symbols at link time
# (-Bsymbolic). A copy relocation would give the object holding it a second
# copy of such a static that the library's own code never sees; Rust reaches
# another object's data through the GOT, so one means a changed toolchain.
no_copy_relocations() {
    if grep -Eq 'R_[A-Z0-9_]+_COPY' <<<"$(readelf -rW "$1")"; then
        echo "$1 has copy relocations; the shared runtime's -Bsymbolic link needs none" >&2
        exit 1
    fi
}
for index in "${!binaries[@]}"; do
    binary=${binaries[$index]}
    crate=${selected_crates[$index]}
    # An app whose `thin` feature was not applied still links and runs, but
    # carries its own static copy of the runtime (tens of MiB). Cargo silently
    # routes `<package>/thin` to a dependency when the package has one keyed by
    # its own name, so name the cause here rather than at the tapp size limit.
    binary_dynamic=$(dynamic_section "$TARGET_DIR/$PROFILE/$binary")
    grep -Fq '[libruntime_dylib.so]' <<<"$binary_dynamic" || {
        echo "thin app does not link libruntime_dylib.so: $binary" >&2
        echo "its <package>/thin feature was not applied; a dependency keyed by the package's own name claims it" >&2
        exit 1
    }
    # Only rustc-thin-app-wrapper.sh asks for packed relative relocations, so
    # their absence means the tapp crate compiled at the profile's speed
    # settings instead, megabytes larger and caught otherwise only at the tapp
    # size limit.
    grep -Fq '(RELR)' <<<"$binary_dynamic" || {
        echo "thin app was not compiled through rustc-thin-app-wrapper.sh: $binary" >&2
        exit 1
    }
    no_copy_relocations "$TARGET_DIR/$PROFILE/$binary"
    loader_arguments=(--help)
    [ "$binary" != tilo-home-export ] || loader_arguments=(--protocol)
    set +e
    # Every symbol bound at exec, whatever the app was linked with: the
    # runtime keeps only what something exported reaches, so a missing one
    # fails here rather than at first use.
    LD_BIND_NOW=1 LD_LIBRARY_PATH="$TARGET_DIR/$PROFILE:$rust_target_libdir" \
        "$TARGET_DIR/$PROFILE/$binary" "${loader_arguments[@]}" \
        >/dev/null 2>"$TARGET_DIR/$PROFILE/.${crate}.loader-check.err"
    status=$?
    set -e
    if (( status == 127 )) || grep -Eq 'symbol lookup error|undefined symbol|error while loading shared libraries' \
        "$TARGET_DIR/$PROFILE/.${crate}.loader-check.err"; then
        cat "$TARGET_DIR/$PROFILE/.${crate}.loader-check.err" >&2
        echo "thin app/runtime ABI mismatch: $binary" >&2
        exit 1
    fi
    rm -f "$TARGET_DIR/$PROFILE/.${crate}.loader-check.err"
done

# Each engine must link this runtime through the wrapper and run against it.
# The list written last is what packaging folds into the runtime bundle
# (dist/apps/package-apps.sh, os/Containerfile, os/dev/assemble-app-runtime.py):
# one library file name a line, beside libruntime_dylib.so.
engine_list="$TARGET_DIR/$PROFILE/runtime-engines"
rm -f "$engine_list"
: > "$engine_list.partial"
engine_count=0
for engine in ${engines[@]+"${engines[@]}"}; do
    library="lib${engine//-/_}.so"
    [ -f "$TARGET_DIR/$PROFILE/$library" ] \
        || { echo "runtime engine $engine did not produce $library" >&2; exit 1; }
    library_dynamic=$(dynamic_section "$TARGET_DIR/$PROFILE/$library")
    grep -Fq '[libruntime_dylib.so]' <<<"$library_dynamic" || {
        echo "runtime engine $library does not link libruntime_dylib.so; it would carry its own copy of the shared crates" >&2
        exit 1
    }
    grep -Fq '(RELR)' <<<"$library_dynamic" || {
        echo "runtime engine $library was not linked through rustc-thin-app-wrapper.sh (no RELR)" >&2
        exit 1
    }
    no_copy_relocations "$TARGET_DIR/$PROFILE/$library"
    LD_LIBRARY_PATH="$TARGET_DIR/$PROFILE:$rust_target_libdir" "$TARGET_DIR/$PROFILE/$engine-check" || {
        echo "runtime engine $library does not run against this runtime ($engine-check failed)" >&2
        exit 1
    }
    printf '%s\n' "$library" >> "$engine_list.partial"
    engine_count=$((engine_count + 1))
done
mv "$engine_list.partial" "$engine_list"

printf 'built %s thin tapp apps with %s and %s runtime engines\n' \
    "${#binaries[@]}" "$TARGET_DIR/$PROFILE/libruntime_dylib.so" "$engine_count"

# The producer may capture the unit stream immediately after this script exits.
# Join its writer rather than exposing a still-being-written inventory.
if [ "$BUILD_PHASE" != verify ] && [ -n "${SDK_COMPILER_UNITS:-}" ]; then
    exec 1>&3 3>&-
    wait "$sdk_compiler_stream_pid"
fi
