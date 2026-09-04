#!/usr/bin/env bash
# Build every qpkg application and the shared Rust/UI runtime in one Cargo
# graph. One graph is a correctness requirement: Rust dylib symbols include
# crate hashes, and independently resolving apps can otherwise produce thin
# binaries that expect different builds of libruntime_dylib.so.
set -euo pipefail

REPO="${REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
EXCLUDE_PACKAGES=" ${EXCLUDE_PACKAGES:-} "
INCLUDE_PACKAGES=" ${INCLUDE_PACKAGES:-} "
APP_FEATURES="${APP_FEATURES:-}"
PROFILE="${PROFILE:-thin-app}"

packages=()
features=()
binary_crates=()
for manifest in "$REPO"/apps/*/qos-app.toml; do
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
done

[ "${#packages[@]}" -gt 0 ] || { echo "no qpkg applications selected" >&2; exit 1; }
feature_csv="$(IFS=,; echo "${features[*]}")"
if [ -n "$APP_FEATURES" ]; then
    feature_csv="$feature_csv,$APP_FEATURES"
fi
thin_crates="$(IFS=:; echo "${binary_crates[*]}")"

cd "$REPO"
QOS_THIN_BIN_CRATES="$thin_crates" \
RUSTC_WORKSPACE_WRAPPER="$REPO/tools/rustc-thin-app-wrapper.sh" \
cargo build --locked --profile "$PROFILE" --features "$feature_csv" \
    -p runtime-dylib "${packages[@]}"

printf 'built %s thin qpkg apps with %s\n' \
    "${#binary_crates[@]}" "$REPO/target/$PROFILE/libruntime_dylib.so"
