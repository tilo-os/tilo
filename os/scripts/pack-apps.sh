#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# Pack every app that ships a `tilo-app.toml` into a signed `.qpkg` and install
# it into the image-wide app store, so a fresh boot has a populated launcher
# without the user running anything.
#
# The point of doing it this way, rather than by copying binaries into
# /usr/bin: a preinstalled app goes through the *same* `tilo-pkg pack` +
# `tilo-install install` path a user's own package goes through, and ends up
# described by the same registry rows. Being preinstalled changes where the
# payload lives and nothing else — no extra grants, no trust bypass, no
# first-party branch anywhere in the launcher. The one difference the store
# has at all is that it is read-only, being inside the image.
#
# Envs:
#   REPO           repo root, holding apps/*/tilo-app.toml   [default: /build]
#   BIN_DIR        directory of built release binaries      [default: /out]
#   QPKG_DIR       install these already-verified release qpkgs instead of
#                  packing BIN_DIR (optional)
#   EXPECTED_PUBKEY require every prebuilt qpkg to use this publisher (64 hex)
#   STORE          app store root; becomes TILO_APPS_DATA_DIR
#                                          [default: /usr/share/tilo-apps]
#   STAGE          scratch for payload trees + .qpkg files  [default: mktemp]
#   IDENTITY_JSON  signing key; generated if absent    [default: $STAGE/id.json]
#   PUBKEY_OUT     write the signing pubkey here (optional)
#   PKG_BIN / INSTALL_BIN / BUNDLE_BIN / SHIM_BIN
#                  tool paths                        [default: $BIN_DIR/<name>]
#   REQUIRE_MIN    fail unless at least this many packages install [default: 1]
#   MAX_QPKG_BYTES refuse a larger archive; defaults to the canonical 5 MiB
#                  application-format ceiling and may only be made stricter
#
# STORE is written at its *final* path on purpose: `tilo-install` records
# absolute payload paths in registry.toml, so staging it elsewhere and moving
# it afterwards would leave every row pointing at a directory that does not
# exist on the booted system.
#
# Exit status is non-zero if any app with a built binary fails to pack or
# install. An app whose binary is absent from BIN_DIR is reported and skipped
# — that is the "not built for this image" case, and the caller decides
# whether that is acceptable via REQUIRE_MIN.
# ---------------------------------------------------------------------------
set -euo pipefail

REPO="${REPO:-/build}"
BIN_DIR="${BIN_DIR:-/out}"
STORE="${STORE:-/usr/share/tilo-apps}"
owned_stage=0
if [ -z "${STAGE:-}" ]; then
    STAGE=$(mktemp -d)
    owned_stage=1
fi
# The default stage contains an ephemeral private publisher key. Remove it
# before this RUN commits a layer, including on failure. Explicit caller-owned
# stages remain the caller's responsibility and must never be deleted here.
cleanup_stage() {
    local status=$?
    if [ "$owned_stage" = 1 ]; then rm -rf -- "$STAGE"; fi
    exit "$status"
}
trap cleanup_stage EXIT
IDENTITY_JSON="${IDENTITY_JSON:-$STAGE/publisher.json}"
PUBKEY_OUT="${PUBKEY_OUT:-}"
PKG_BIN="${PKG_BIN:-$BIN_DIR/tilo-pkg}"
INSTALL_BIN="${INSTALL_BIN:-$BIN_DIR/tilo-install}"
BUNDLE_BIN="${BUNDLE_BIN:-$BIN_DIR/tilo-runtime-bundle}"
SHIM_BIN="${SHIM_BIN:-$BIN_DIR/tilo-cli-shim}"
QPKG_DIR="${QPKG_DIR:-}"
EXPECTED_PUBKEY="${EXPECTED_PUBKEY:-}"
REQUIRE_MIN="${REQUIRE_MIN:-1}"
POLICY_MAX_QPKG_BYTES=5242880
MAX_QPKG_BYTES="${MAX_QPKG_BYTES:-$POLICY_MAX_QPKG_BYTES}"

msg() { printf '[pack-apps] %s\n' "$*" >&2; }
die() { printf '[pack-apps] ERROR: %s\n' "$*" >&2; exit 1; }

[[ "$MAX_QPKG_BYTES" =~ ^[0-9]+$ ]] \
    || die "MAX_QPKG_BYTES must be an integer"
(( MAX_QPKG_BYTES <= POLICY_MAX_QPKG_BYTES )) \
    || die "MAX_QPKG_BYTES cannot exceed the 5 MiB application policy"

for t in "$PKG_BIN" "$INSTALL_BIN" "$BUNDLE_BIN"; do
    [ -x "$t" ] || die "$t not found or not executable"
done

mkdir -p "$STAGE" "$STORE"
initial_rows="$(grep -c '^\[\[entry\]\]' "$STORE/registry.toml" 2>/dev/null || true)"

# --- source-package signing identity ---------------------------------------
# Minted per build and discarded with the stage dir, exactly like the runtime
# bundle's. Nothing needs to sign as this identity again: the packages are
# already unpacked into the store by the time the image is sealed, so the
# signature's job (binding manifest to payload during install) is done.
prebuilt=0
if [ -n "$QPKG_DIR" ] && compgen -G "$QPKG_DIR/*.qpkg" >/dev/null; then
    prebuilt=1
    PUBKEY=''
else
    if [ ! -f "$IDENTITY_JSON" ]; then
        msg "generating app-signing identity at $IDENTITY_JSON"
        "$BUNDLE_BIN" key generate --out "$IDENTITY_JSON"
    fi
    PUBKEY="$("$BUNDLE_BIN" key pubkey --identity "$IDENTITY_JSON")"
    [ -n "$PUBKEY" ] || die "could not read the signing pubkey"
    [ -z "$PUBKEY_OUT" ] || { mkdir -p "$(dirname "$PUBKEY_OUT")"; printf '%s\n' "$PUBKEY" > "$PUBKEY_OUT"; }
    msg "signing as ed25519:$PUBKEY"
fi

# `tilo-install` writes a PATH shim per declared CLI command and needs the
# shim binary to copy. Without it a CLI package installs but its commands
# do not resolve, so point at it when it was built.
[ ! -x "$SHIM_BIN" ] || export TILO_CLI_SHIM_PATH="$SHIM_BIN"

# Each shim records the launcher to exec. Left to resolve on its own it
# would find the sibling of the `tilo-install` running here — a build-time
# path like /out/tilo-launcher that does not exist on the booted system, so
# every shimmed command would fail with "no such file". Name where the
# launcher will actually be.
export TILO_LAUNCHER_PATH="${TILO_LAUNCHER_PATH:-/usr/bin/tilo-launcher}"

export TILO_APPS_DATA_DIR="$STORE"

# --- pack + install every manifest -----------------------------------------
shipped=(); missing=(); failed=()

if [ "$prebuilt" = 1 ]; then
    for qpkg in "$QPKG_DIR"/*.qpkg; do
        qpkg_bytes=$(wc -c < "$qpkg")
        if [ "$qpkg_bytes" -gt "$MAX_QPKG_BYTES" ]; then
            failed+=("$(basename "$qpkg"): $qpkg_bytes bytes exceeds $MAX_QPKG_BYTES")
            continue
        fi
        if ! "$PKG_BIN" verify "$qpkg" >"$STAGE/$(basename "$qpkg").verify.log" 2>&1; then
            failed+=("$(basename "$qpkg"): signature or content verification failed")
            continue
        fi
        package_pubkey=$("$PKG_BIN" inspect "$qpkg" \
            | sed -n 's/^identity  *= *"ed25519:\([0-9a-fA-F]\{64\}\)".*/\1/p' \
            | head -1)
        [ -n "$package_pubkey" ] \
            || { failed+=("$(basename "$qpkg"): publisher identity is missing"); continue; }
        if [ -z "$PUBKEY" ]; then
            PUBKEY=$package_pubkey
        elif [ "$PUBKEY" != "$package_pubkey" ]; then
            failed+=("$(basename "$qpkg"): collection mixes publisher identities")
            continue
        fi
        if ! "$INSTALL_BIN" install "$qpkg" --allow-self-signed \
                >"$STAGE/$(basename "$qpkg").install.log" 2>&1; then
            failed+=("$(basename "$qpkg"): install failed")
            continue
        fi
        shipped+=("$(basename "$qpkg")")
    done
    [ -z "$PUBKEY_OUT" ] || { mkdir -p "$(dirname "$PUBKEY_OUT")"; printf '%s\n' "$PUBKEY" > "$PUBKEY_OUT"; }
    if [ -n "$EXPECTED_PUBKEY" ] && [ "${PUBKEY,,}" != "${EXPECTED_PUBKEY,,}" ]; then
        die "prebuilt collection publisher does not match EXPECTED_PUBKEY"
    fi
else

for manifest in "$REPO"/apps/*/tilo-app.toml; do
    [ -e "$manifest" ] || continue
    app="$(basename "$(dirname "$manifest")")"

    id="$(sed -n 's/^id  *= *"\([^"]*\)".*/\1/p' "$manifest" | head -1)"
    entry_bin="$(sed -n 's/^bin  *= *"\([^"]*\)".*/\1/p' "$manifest" | head -1)"
    [ -n "$id" ] || { failed+=("$app: no [package].id"); continue; }
    [ -n "$entry_bin" ] || { failed+=("$app: no [entrypoint].bin"); continue; }

    # The manifest names the path the binary takes *inside* the payload;
    # its basename is the cargo binary name in BIN_DIR.
    built="$BIN_DIR/$(basename "$entry_bin")"
    if [ ! -x "$built" ]; then
        missing+=("$app ($id): no binary at $built")
        continue
    fi

    payload="$STAGE/payload/$id"
    rm -rf "$payload"
    mkdir -p "$payload/$(dirname "$entry_bin")"
    cp "$built" "$payload/$entry_bin"
    chmod 755 "$payload/$entry_bin"

    # `tilo-pkg pack` refuses a manifest whose publisher.identity disagrees
    # with the signing key, so stamp this build's pubkey into a copy. The
    # committed manifest is never modified.
    staged_manifest="$STAGE/manifest-$id.toml"
    sed "s|^\(identity  *= *\)\"ed25519:[0-9a-fA-F]*\"|\1\"ed25519:$PUBKEY\"|" \
        "$manifest" > "$staged_manifest"
    grep -q "ed25519:$PUBKEY" "$staged_manifest" \
        || { failed+=("$app ($id): could not stamp publisher identity"); continue; }

    qpkg="$STAGE/$id.qpkg"
    if ! "$PKG_BIN" pack \
            --manifest "$staged_manifest" \
            --payload "$payload" \
            --output "$qpkg" \
            --identity "$IDENTITY_JSON" >"$STAGE/$id.pack.log" 2>&1; then
        failed+=("$app ($id): pack failed — $(tail -3 "$STAGE/$id.pack.log" | tr '\n' ' ')")
        continue
    fi

    qpkg_bytes="$(stat -c %s "$qpkg")"
    if [ "$qpkg_bytes" -gt "$MAX_QPKG_BYTES" ]; then
        failed+=("$app ($id): package is $qpkg_bytes bytes; limit is $MAX_QPKG_BYTES")
        continue
    fi

    # Verify against the key we just signed with, so a mis-signed package
    # fails here rather than silently at first launch.
    if ! "$PKG_BIN" verify --trusted-key "$PUBKEY" "$qpkg" >"$STAGE/$id.verify.log" 2>&1; then
        failed+=("$app ($id): verify failed — $(tail -3 "$STAGE/$id.verify.log" | tr '\n' ' ')")
        continue
    fi

    # Self-signed is what this is: the image publisher signed it, and no
    # third-party attestor vouched for it. Saying so explicitly is honest,
    # and it is the same flag a user passes for their own local package.
    if ! "$INSTALL_BIN" install "$qpkg" --allow-self-signed \
            >"$STAGE/$id.install.log" 2>&1; then
        failed+=("$app ($id): install failed — $(tail -3 "$STAGE/$id.install.log" | tr '\n' ' ')")
        continue
    fi

    shipped+=("$id ($qpkg_bytes bytes)")
done
fi

# --- report ----------------------------------------------------------------
msg "shipped ${#shipped[@]} size-gated package(s) into $STORE"
for m in ${missing[@]+"${missing[@]}"}; do msg "  skipped: $m"; done
for f in ${failed[@]+"${failed[@]}"};  do msg "  FAILED:  $f"; done

if [ "${#failed[@]}" -ne 0 ]; then
    die "${#failed[@]} package(s) failed to pack or install (see above)"
fi
if [ "${#shipped[@]}" -lt "$REQUIRE_MIN" ]; then
    die "only ${#shipped[@]} package(s) installed, expected at least $REQUIRE_MIN"
fi

# The registry is what the launcher reads; an empty one means an empty
# palette, which is the failure this whole step exists to prevent.
[ -s "$STORE/registry.toml" ] || die "$STORE/registry.toml is missing or empty"
rows="$(grep -c '^\[\[entry\]\]' "$STORE/registry.toml" || true)"
expected_rows=$((initial_rows + ${#shipped[@]}))
[ "$rows" = "$expected_rows" ] \
    || die "registry has $rows row(s), expected $initial_rows existing + ${#shipped[@]} installed"
msg "registry.toml lists $rows package(s)"
