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
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}"

packages=()
features=()
binary_crates=()
binaries=()
build_targets=()
selected_crates=()
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
cd "$REPO"
TILO_THIN_BIN_CRATES="$thin_crates" \
RUSTC_WORKSPACE_WRAPPER="$REPO/tools/ci/rustc-thin-app-wrapper.sh" \
cargo build --locked --profile "$PROFILE" --features "$feature_csv" \
    -p runtime-dylib "${packages[@]}" "${build_targets[@]}"

# A Rust dylib mismatch is not reliably rejected by the ELF linker: symbol
# names include crate hashes, so an app built against a second feature-unified
# dependency can retain undefined symbols and fail only when the loader starts
# it. Resolve every selected binary against this exact runtime now, before it
# can be signed or published. `--help` is intentionally not assumed to be a
# supported app argument; reaching main (any status except the loader's 127)
# is enough for this ABI check.
rust_target_libdir=$(rustc --print target-libdir)
for index in "${!binaries[@]}"; do
    binary=${binaries[$index]}
    crate=${selected_crates[$index]}
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
