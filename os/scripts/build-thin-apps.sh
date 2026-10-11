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
TARGET_DIR="${TARGET_DIR:-${CARGO_TARGET_DIR:-$REPO/target}}"

packages=()
features=()
binary_crates=()
binaries=()
build_targets=()
selected_crates=()
app_profiles=()
for manifest in "$REPO"/apps/*/tilo-app.toml; do
    dir="$(dirname "$manifest")"
    package="$(sed -n 's/^name *= *"\([^"]*\)".*/\1/p' "$dir/Cargo.toml" | head -1)"
    binary="$(sed -n 's/^bin  *= *"bin\/\([^"]*\)".*/\1/p' "$manifest" | head -1)"
    [ -n "$package" ] && [ -n "$binary" ] \
        || { echo "could not read package/binary from $dir" >&2; exit 1; }
    case "$EXCLUDE_PACKAGES" in
        *" $package "*) continue ;;
    esac
    if [ "$INCLUDE_PACKAGES" != "  " ]; then
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
                   --config "profile.$PROFILE.package.\"$package\".strip=\"symbols\"")
    features+=("$package/thin")
    binary_crates+=("${binary//-/_}")
    if [ "$BUILD_PACKAGES" = "  " ] || [[ "$BUILD_PACKAGES" == *" $package "* ]]; then
        binaries+=("$binary")
        selected_crates+=("${binary//-/_}")
        build_targets+=(--bin "$binary")
    fi
done

[ "${#packages[@]}" -gt 0 ] || { echo "no tapp applications selected" >&2; exit 1; }
[ "${#binaries[@]}" -gt 0 ] || { echo "no tapp binaries selected" >&2; exit 1; }
for requested in $BUILD_PACKAGES; do
    [[ " ${packages[*]} " == *" -p $requested "* ]] || {
        echo "requested app is outside the runtime graph: $requested" >&2; exit 1;
    }
done
feature_csv="$(IFS=,; echo "${features[*]}")"
if [ -n "$APP_FEATURES" ]; then
    feature_csv="$feature_csv,$APP_FEATURES"
fi
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
# sccache must see the real compiler after the workspace wrapper adds its
# flags. Outside it, sccache 0.17 rejects an absolute compiler as a second
# input; a literal compiler is cacheable but bypasses the flag-mutating wrapper.
# Keep Cargo's compiler selection intact and route workspace calls through the
# size wrapper before caching. The dispatcher retains native compiler caching.
if [[ ${RUSTC_WRAPPER:-} == */sccache || ${RUSTC_WRAPPER:-} == sccache ]]; then
    cache_path=$RUSTC_WRAPPER
    cache_config=$(python3 -c 'import json, sys; print(json.dumps(sys.argv[1]))' "$cache_path")
    export TILO_THIN_SCCACHE="$cache_path"
    export RUSTC_WRAPPER="$REPO/tools/ci/thin-app/sccache"
    extra_cargo_args+=(--config "build.rustc-wrapper=\"$RUSTC_WRAPPER\""
                      --config "env.TILO_THIN_SCCACHE={value=$cache_config,force=true}")
fi
if [ -n "${SDK_COMPILER_UNITS:-}" ]; then
    extra_cargo_args+=(--message-format=json-render-diagnostics)
    # Keep the actual unit identities on the producing machine; no runtime bytes change.
    exec 3>&1
    exec > >(tee "$SDK_COMPILER_UNITS" >&3)
    sdk_compiler_stream_pid=$!
fi
"${CARGO:-cargo}" build --locked --profile "$PROFILE" --features "$feature_csv" "${extra_cargo_args[@]}" \
    --config "build.rustc-workspace-wrapper=\"$REPO/tools/ci/rustc-thin-app-wrapper.sh\"" \
    --config "env.TILO_THIN_BIN_CRATES=\"$thin_crates\"" \
    "${app_profiles[@]}" -p runtime-dylib "${packages[@]}" "${build_targets[@]}"

# A Rust dylib mismatch is not reliably rejected by the ELF linker: symbol
# names include crate hashes, so an app built against a second feature-unified
# dependency can retain undefined symbols and fail only when the loader starts
# it. Resolve every selected binary against this exact runtime now, before it
# can be signed or published. `--help` is intentionally not assumed to be a
# supported app argument; reaching main (any status except the loader's 127)
# is enough for this ABI check.
rust_target_libdir=$(rustc --print target-libdir)
# The shared runtime's relocations run in every app process (TIL-604); the
# wrapper packs them. Without RELR it was linked without the wrapper.
readelf -dW "$TARGET_DIR/$PROFILE/libruntime_dylib.so" | grep -Fq '(RELR)' || {
    echo "libruntime_dylib.so was not linked through rustc-thin-app-wrapper.sh (no RELR)" >&2
    exit 1
}
for index in "${!binaries[@]}"; do
    binary=${binaries[$index]}
    crate=${selected_crates[$index]}
    # An app whose `thin` feature was not applied still links and runs, but
    # carries its own static copy of the runtime (tens of MiB). Cargo silently
    # routes `<package>/thin` to a dependency when the package has one keyed by
    # its own name, so name the cause here rather than at the tapp size limit.
    readelf -dW "$TARGET_DIR/$PROFILE/$binary" | grep -Fq '[libruntime_dylib.so]' || {
        echo "thin app does not link libruntime_dylib.so: $binary" >&2
        echo "its <package>/thin feature was not applied; a dependency keyed by the package's own name claims it" >&2
        exit 1
    }
    # Only rustc-thin-app-wrapper.sh asks for packed relative relocations, so
    # their absence means the tapp crate compiled at the profile's speed
    # settings instead, megabytes larger and caught otherwise only at the tapp
    # size limit.
    readelf -dW "$TARGET_DIR/$PROFILE/$binary" | grep -Fq '(RELR)' || {
        echo "thin app was not compiled through rustc-thin-app-wrapper.sh: $binary" >&2
        exit 1
    }
    set +e
    LD_LIBRARY_PATH="$TARGET_DIR/$PROFILE:$rust_target_libdir" \
        "$TARGET_DIR/$PROFILE/$binary" --help \
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

printf 'built %s thin tapp apps with %s\n' \
    "${#binaries[@]}" "$TARGET_DIR/$PROFILE/libruntime_dylib.so"

# The producer may capture the unit stream immediately after this script exits.
# Join its writer rather than exposing a still-being-written inventory.
if [ -n "${SDK_COMPILER_UNITS:-}" ]; then
    exec 1>&3 3>&-
    wait "$sdk_compiler_stream_pid"
fi
