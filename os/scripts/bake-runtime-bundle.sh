#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# Bake a signed Tilo runtime bundle for the current architecture.
#
# What this does:
#   1. Ensures `tilo-runtime-bundle` is built (uses cargo, honours SCCACHE).
#   2. Generates a fresh Ed25519 identity if $IDENTITY_JSON isn't already
#      populated.
#   3. Signs a bundle at $OUT_DIR/tilo-runtime/<release>+native.<digest>/ from
#      $FROM, named by its ABI digest, and writes that version to
#      $VERSION_OUT.
#   4. Writes the publisher pubkey to $OUT_DIR/runtime-bundle-publisher.hex
#      and (if $TRUST_LIST is set) appends it to that file.
#
# Envs (overridable):
#   RUNTIME_RELEASE     the runtime release apps declare (`tilo-runtime/<release>`);
#                       the bundle's version is this release plus the digest
#                       of its payload [required]
#   VERSION_OUT         file the built bundle's version is written to
#                       [default: $OUT_DIR/runtime-version]
#   FROM                host prefix to copy source paths from [default: /]
#   ARCH                target architecture [default: $(uname -m)]
#   OUT_DIR             where the baked bundle + pubkey land
#                       [default: os/output/runtime-bundle]
#   IDENTITY_JSON       path to reuse a signing key. Generated fresh if
#                       missing.
#   TRUST_LIST          if set, append the pubkey to this file after signing.
#   BUNDLE_BIN          path to a prebuilt tilo-runtime-bundle binary. Cargo
#                       is used if unset.
#   EXTRA_LIBS          whitespace-separated absolute paths to extra shared
#                       objects to fold into the bundle's lib/ (empty by
#                       default). This is how the shared toolkit reaches a
#                       sandboxed app: `libruntime_dylib.so` (the `ui` +
#                       runtime SDK dylib, built with `cargo build --profile
#                       thin-app -p runtime-dylib`) and the matching Rust
#                       `libstd-<hash>.so` go here, and the launcher's
#                       `LD_LIBRARY_PATH=<mount>/lib` then resolves them by
#                       soname inside bwrap. Each is hashed into the manifest
#                       and covered by the signature like any other file, so
#                       a swapped toolkit fails `verify`. Every listed path
#                       must exist -- a typo is an error, not a silent skip,
#                       because a bundle missing the toolkit produces a thin
#                       app that dies at exec with an unresolved soname.
#   FONT_CATALOG_ROOT   verified product catalog directory to include as
#                       lib/fonts in this signed SDK (optional).
#
# Called from os/build.sh via `--bake-runtime` before the container build.
# Callable stand-alone during dev — the output tree can then be COPYied
# into a bootc image or served as a per-user install source.
# ---------------------------------------------------------------------------
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_DIR="$(cd "$OS_DIR/.." && pwd)"

RUNTIME_RELEASE="${RUNTIME_RELEASE:?RUNTIME_RELEASE is required}"
FROM="${FROM:-/}"
ARCH="${ARCH:-$(uname -m)}"
OUT_DIR="${OUT_DIR:-$OS_DIR/output/runtime-bundle}"
IDENTITY_JSON="${IDENTITY_JSON:-}"
TRUST_LIST="${TRUST_LIST:-}"
BUNDLE_BIN="${BUNDLE_BIN:-}"
EXTRA_LIBS="${EXTRA_LIBS:-}"
FONT_CATALOG_ROOT="${FONT_CATALOG_ROOT:-}"
VERSION_OUT="${VERSION_OUT:-$OUT_DIR/runtime-version}"

msg() { printf '[bake-runtime-bundle] %s\n' "$*" >&2; }
die() { printf '[bake-runtime-bundle] ERROR: %s\n' "$*" >&2; exit 1; }

# 1. Locate the tilo-runtime-bundle binary.
if [ -z "$BUNDLE_BIN" ]; then
    msg "building tilo-runtime-bundle (release)"
    (cd "$REPO_DIR" && cargo build --release -p runtime-bundle)
    BUNDLE_BIN="$REPO_DIR/target/release/tilo-runtime-bundle"
fi
if [ ! -x "$BUNDLE_BIN" ]; then
    echo "[bake-runtime-bundle] $BUNDLE_BIN not found or not executable" >&2
    exit 1
fi

mkdir -p "$OUT_DIR"

# 2. Signing identity — generate if not provided. The generated key is a
#    build-time secret; it's fine to regenerate per bake because the
#    pubkey is what gets pinned in the OS image's trust file, and every
#    bake writes a matching pair.
if [ -z "$IDENTITY_JSON" ]; then
    IDENTITY_JSON="$OUT_DIR/identity.json"
fi
if [ ! -f "$IDENTITY_JSON" ]; then
    msg "generating fresh signing identity at $IDENTITY_JSON"
    "$BUNDLE_BIN" key generate --out "$IDENTITY_JSON"
else
    msg "reusing existing signing identity at $IDENTITY_JSON"
fi

# 3. Sign + bake.
msg "baking runtime $RUNTIME_RELEASE under $OUT_DIR/tilo-runtime (arch=$ARCH, from=$FROM)"

# Shared objects to fold in beside the loader/libc set. Checked before the
# bake so a missing toolkit is reported here rather than at an app's exec.
INCLUDE_ARGS=()
if [ -n "$FONT_CATALOG_ROOT" ]; then
    python3 "$REPO_DIR/tools/font-catalog.py" verify "$FONT_CATALOG_ROOT" --scope product \
        || die "cannot sign an invalid product font catalog"
    [ "$(basename "$FONT_CATALOG_ROOT")" = fonts ] \
        || die "FONT_CATALOG_ROOT must be a fonts directory"
    INCLUDE_ARGS+=(--include "$FONT_CATALOG_ROOT")
fi
dependency_objects=()
dependency_dirs=()
for lib in $EXTRA_LIBS; do
    [ -f "$lib" ] || die "EXTRA_LIBS entry does not exist: $lib"
    INCLUDE_ARGS+=(--include "$lib")
    dependency_objects+=("$lib")
    dependency_dirs+=(--library-dir "$(dirname "$lib")")
    msg "including shared object: $lib"
done
if [ "${#dependency_objects[@]}" -gt 0 ]; then
    # Capture the exit status before consuming output: process substitution
    # would hide an ldd failure and let an incomplete runtime be signed.
    dependencies=$(python3 "$SCRIPT_DIR/collect-runtime-dependencies.py" \
        "${dependency_dirs[@]}" "${dependency_objects[@]}") \
        || die "cannot sign an incomplete native dependency closure"
    while IFS= read -r dependency; do
        INCLUDE_ARGS+=(--include "$dependency")
    done <<< "$dependencies"
fi

rm -rf "$OUT_DIR/tilo-runtime"
"$BUNDLE_BIN" build \
    --release "$RUNTIME_RELEASE" \
    --arch "$ARCH" \
    --from "$FROM" \
    --identity "$IDENTITY_JSON" \
    "${INCLUDE_ARGS[@]+"${INCLUDE_ARGS[@]}"}" \
    --out-root "$OUT_DIR/tilo-runtime" \
    --version-out "$VERSION_OUT"
BUNDLE_DIR="$OUT_DIR/tilo-runtime/$(cat "$VERSION_OUT")"
if [ -n "$FONT_CATALOG_ROOT" ]; then
    python3 "$REPO_DIR/tools/font-catalog.py" verify "$BUNDLE_DIR/lib/fonts" --scope product \
        || die "signed SDK is missing its verified product font catalog"
fi
# One copy per soname (tilo-runtime-bundle's BuildOptions::extra_libs): a
# library at the top of lib/, where LD_LIBRARY_PATH finds it, is not signed
# again in the loader set below it, and the loader only at its interpreter path.
for object in "$BUNDLE_DIR"/lib/*.so*; do
    [ -f "$object" ] || continue
    name=${object##*/}
    case $name in
        ld-linux*) die "the loader is signed at lib/$name as well as at its interpreter path" ;;
    esac
    for directory in lib64 lib usr/lib64 lib/x86_64-linux-gnu lib/aarch64-linux-gnu; do
        [ ! -e "$BUNDLE_DIR/lib/$directory/$name" ] \
            || die "$name is signed twice, at lib/$name and lib/$directory/$name"
    done
done
msg "baked runtime $(cat "$VERSION_OUT")"

# 4. Extract + record the pubkey.
PUBKEY_HEX="$("$BUNDLE_BIN" key pubkey --identity "$IDENTITY_JSON")"
PUBKEY_FILE="$OUT_DIR/runtime-bundle-publisher.hex"
printf '%s\n' "$PUBKEY_HEX" > "$PUBKEY_FILE"
msg "publisher: ed25519:$PUBKEY_HEX"
msg "pubkey written to $PUBKEY_FILE"

# 5. Optional trust-list update. If the caller passed $TRUST_LIST the
#    pubkey is appended (deduped) so a launcher configured to read
#    `runtime-bundle-trusted-publishers.txt` accepts this bake.
if [ -n "$TRUST_LIST" ]; then
    mkdir -p "$(dirname "$TRUST_LIST")"
    touch "$TRUST_LIST"
    if ! grep -qxF "$PUBKEY_HEX" "$TRUST_LIST"; then
        printf '%s\n' "$PUBKEY_HEX" >> "$TRUST_LIST"
        msg "added pubkey to $TRUST_LIST"
    else
        msg "pubkey already present in $TRUST_LIST"
    fi
fi

# 6. Verify the round-trip as a build-time sanity check — same code the
#    launcher runs at every mount.
"$BUNDLE_BIN" verify "$BUNDLE_DIR" --require-publisher "$PUBKEY_HEX"

msg "done"
