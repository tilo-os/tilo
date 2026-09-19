#!/usr/bin/env bash
# Build selected qpkg binaries and the shared Rust/UI runtime in one Cargo
# graph. One graph is a correctness requirement: Rust dylib symbols include
# crate hashes, and independently resolving apps can otherwise produce thin
# binaries that expect different builds of libruntime_dylib.so.
set -euo pipefail

REPO="${REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
EXCLUDE_PACKAGES=" ${EXCLUDE_PACKAGES:-} "
INCLUDE_PACKAGES=" ${INCLUDE_PACKAGES:-} "
# Optional binary subset; INCLUDE_PACKAGES still defines the complete ABI graph.
build_selection_set="${BUILD_APP_PACKAGES+x}"
BUILD_APP_PACKAGES="${BUILD_APP_PACKAGES:-}"
APP_FEATURES="${APP_FEATURES:-}"
PROFILE="${PROFILE:-thin-app}"
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}"

packages=()
features=()
binary_crates=()
binaries=()
target_args=()
graph_packages=()
graph_binary_crates=()
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
    features+=("$package/thin")
    graph_packages+=("$package")
    graph_binary_crates+=("${binary//-/_}")
    if [ -n "$build_selection_set" ]; then
        case " $BUILD_APP_PACKAGES " in
            *" $package "*) ;;
            *) continue ;;
        esac
        target_args+=("--bin" "$binary")
    fi
    binary_crates+=("${binary//-/_}")
    binaries+=("$binary")
done

[ "${#packages[@]}" -gt 0 ] || { echo "no qpkg applications selected" >&2; exit 1; }
if [ -n "$build_selection_set" ]; then
    for requested in $BUILD_APP_PACKAGES; do
        case " ${graph_packages[*]} " in
            *" $requested "*) ;;
            *) echo "app binary package is outside the runtime graph: $requested" >&2; exit 1 ;;
        esac
    done
    [ "${#target_args[@]}" -gt 0 ] || { echo "no app binaries selected" >&2; exit 1; }
fi
feature_csv="$(IFS=,; echo "${features[*]}")"
if [ -n "$APP_FEATURES" ]; then
    feature_csv="$feature_csv,$APP_FEATURES"
fi
thin_crates="$(IFS=:; echo "${graph_binary_crates[*]}")"

cd "$REPO"
# Cargo resolves features for every -p root even when --bin filters the build
# targets. The thin feature makes runtime-dylib a dependency of each app, so
# its library is still built without --lib (which would build every app lib).
TILO_THIN_BIN_CRATES="$thin_crates" \
RUSTC_WORKSPACE_WRAPPER="$REPO/tools/ci/rustc-thin-app-wrapper.sh" \
cargo build --locked --profile "$PROFILE" --features "$feature_csv" \
    -p runtime-dylib "${packages[@]}" "${target_args[@]}"

# A Rust dylib mismatch is not reliably rejected by the ELF linker: symbol
# names include crate hashes, so an app built against a second feature-unified
# dependency can retain undefined symbols and fail only when the loader starts
# it. Resolve every selected binary against this exact runtime now, before it
# can be signed or published. `--help` is intentionally not assumed to be a
# supported app argument; reaching main (any status except the loader's 127)
# is enough for this ABI check.
rust_target_libdir=$(rustc --print target-libdir)
for index in "${!binary_crates[@]}"; do
    binary=${binaries[$index]}
    crate=${binary_crates[$index]}
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

printf 'built %s thin qpkg apps with %s\n' \
    "${#binary_crates[@]}" "$TARGET_DIR/$PROFILE/libruntime_dylib.so"
