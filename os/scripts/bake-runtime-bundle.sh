#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# Bake a signed qOS runtime bundle for the current architecture.
#
# What this does:
#   1. Ensures `qos-runtime-bundle` is built (uses cargo, honours SCCACHE).
#   2. Generates a fresh Ed25519 identity if $IDENTITY_JSON isn't already
#      populated.
#   3. Signs a bundle at $OUT_DIR/qos-runtime/$RUNTIME_VERSION/ from $FROM.
#   4. Writes the publisher pubkey to $OUT_DIR/runtime-bundle-publisher.hex
#      and (if $TRUST_LIST is set) appends it to that file.
#
# Envs (overridable):
#   RUNTIME_VERSION     bundle version tag written into bundle.toml + the
#                       system path (/usr/lib/qos-runtime/<v>/) [default: 1]
#   FROM                host prefix to copy source paths from [default: /]
#   ARCH                target architecture [default: $(uname -m)]
#   OUT_DIR             where the baked bundle + pubkey land
#                       [default: os/output/runtime-bundle]
#   IDENTITY_JSON       path to reuse a signing key. Generated fresh if
#                       missing.
#   TRUST_LIST          if set, append the pubkey to this file after signing.
#   BUNDLE_BIN          path to a prebuilt qos-runtime-bundle binary. Cargo
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
#
# Called from os/build.sh via `--bake-runtime` before the container build.
# Callable stand-alone during dev — the output tree can then be COPYied
# into a bootc image or served as a per-user install source.
# ---------------------------------------------------------------------------
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_DIR="$(cd "$OS_DIR/.." && pwd)"

RUNTIME_VERSION="${RUNTIME_VERSION:-1}"
FROM="${FROM:-/}"
ARCH="${ARCH:-$(uname -m)}"
OUT_DIR="${OUT_DIR:-$OS_DIR/output/runtime-bundle}"
IDENTITY_JSON="${IDENTITY_JSON:-}"
TRUST_LIST="${TRUST_LIST:-}"
BUNDLE_BIN="${BUNDLE_BIN:-}"
EXTRA_LIBS="${EXTRA_LIBS:-}"

msg() { printf '[bake-runtime-bundle] %s\n' "$*" >&2; }
die() { printf '[bake-runtime-bundle] ERROR: %s\n' "$*" >&2; exit 1; }

# 1. Locate the qos-runtime-bundle binary.
if [ -z "$BUNDLE_BIN" ]; then
    msg "building qos-runtime-bundle (release)"
    (cd "$REPO_DIR" && cargo build --release -p runtime-bundle)
    BUNDLE_BIN="$REPO_DIR/target/release/qos-runtime-bundle"
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
BUNDLE_DIR="$OUT_DIR/qos-runtime/$RUNTIME_VERSION"
msg "baking bundle at $BUNDLE_DIR (arch=$ARCH, from=$FROM)"

# Shared objects to fold in beside the loader/libc set. Checked before the
# bake so a missing toolkit is reported here rather than at an app's exec.
INCLUDE_ARGS=()
for lib in $EXTRA_LIBS; do
    [ -f "$lib" ] || die "EXTRA_LIBS entry does not exist: $lib"
    INCLUDE_ARGS+=(--include "$lib")
    msg "including shared object: $lib"
done

rm -rf "$BUNDLE_DIR"
"$BUNDLE_BIN" build \
    --version "$RUNTIME_VERSION" \
    --arch "$ARCH" \
    --from "$FROM" \
    --identity "$IDENTITY_JSON" \
    "${INCLUDE_ARGS[@]+"${INCLUDE_ARGS[@]}"}" \
    --out "$BUNDLE_DIR"

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
