#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# Pack every app that ships a `tilo-app.toml` into a signed `.tapp` and install
# it into the image-wide app store, so a fresh boot has a populated launcher
# without the user running anything.
#
# The point of doing it this way, rather than by copying binaries into
# /usr/bin: a preinstalled app goes through the *same* `tilo-app-package pack` +
# `tilo-install install` path a user's own package goes through, and ends up
# described by the same registry rows. Being preinstalled changes where the
# payload lives and nothing else — no extra grants, no trust bypass, no
# first-party branch anywhere in the launcher. The one difference the store
# has at all is that it is read-only, being inside the image.
#
# Envs:
#   REPO           repo root, holding apps/*/tilo-app.toml   [default: /build]
#   BIN_DIR        directory of built release binaries      [default: /out]
#   TAPP_DIR       install these already-verified release tapps instead of
#                  packing BIN_DIR (optional)
#   COHORT_TAPP_DIR retain each installed signed archive by application id
#                  for verified image-cohort selection (optional)
#   EXPECTED_PUBKEY require every prebuilt tapp to use this publisher (64 hex)
#   STORE          app store root; becomes TILO_APPS_DATA_DIR
#                                          [default: /usr/share/tilo-apps]
#   STAGE          scratch for payload trees + .tapp files  [default: mktemp]
#   TILO_IMAGE_BUILD=1 pins the scratch path and registry install time to
#                  SOURCE_DATE_EPOCH for independently reproduced OS images
#   IDENTITY_JSON  signing key; generated if absent    [default: $STAGE/id.json]
#   PUBKEY_OUT     write the signing pubkey here (optional)
#   RUNTIME_ID     the runtime build the apps in BIN_DIR were linked against,
#                  `tilo-runtime/<release>+native.<digest>`; stamped over each
#                  manifest's authored release (required unless TAPP_DIR)
#   PKG_BIN / INSTALL_BIN / BUNDLE_BIN / SHIM_BIN
#                  tool paths                        [default: $BIN_DIR/<name>]
#   REQUIRE_MIN    fail unless at least this many packages install [default: 1]
#   MAX_TAPP_BYTES refuse a larger archive; defaults to the canonical
#                  application-format ceiling (the byte count in
#                  crates/packaging/tilo-app-package/max-app-bytes, which the
#                  Rust package checks compile in) and may only be made stricter
#   BUDGET_DIR     the per-app size budgets: budget.toml plus raises/*.toml
#                  [default: tools/perf/tapp-size-budget beside this script]
#   TAPP_ARCH      the architecture the apps were built for, as budget.toml
#                  names it                                  [default: uname -m]
#   TAPP_BUDGET    `enforce` refuses an app over its budget; `report` names it
#                  and packs it (a build whose binaries are not the release
#                  profile the budgets measure)            [default: enforce]
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
    if [ "${TILO_IMAGE_BUILD:-0}" = 1 ]; then
        STAGE=/tmp/tilo-image-app-pack-stage
        [ ! -e "$STAGE" ] || { printf 'image app stage already exists\n' >&2; exit 1; }
        [ -n "${SOURCE_DATE_EPOCH:-}" ] || { printf 'image app build needs SOURCE_DATE_EPOCH\n' >&2; exit 1; }
    else
        STAGE=$(mktemp -d)
    fi
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
PKG_BIN="${PKG_BIN:-$BIN_DIR/tilo-app-package}"
INSTALL_BIN="${INSTALL_BIN:-$BIN_DIR/tilo-install}"
BUNDLE_BIN="${BUNDLE_BIN:-$BIN_DIR/tilo-runtime-bundle}"
SHIM_BIN="${SHIM_BIN:-$BIN_DIR/tilo-cli-shim}"
TAPP_DIR="${TAPP_DIR:-}"
RUNTIME_ID="${RUNTIME_ID:-}"
EXPECTED_PUBKEY="${EXPECTED_PUBKEY:-}"
REQUIRE_MIN="${REQUIRE_MIN:-1}"
msg() { printf '[pack-apps] %s\n' "$*" >&2; }
die() { printf '[pack-apps] ERROR: %s\n' "$*" >&2; exit 1; }

SIZE_POLICY="$(dirname "${BASH_SOURCE[0]}")/../../crates/packaging/tilo-app-package/max-app-bytes"
POLICY_MAX_TAPP_BYTES="$(cat "$SIZE_POLICY" 2>/dev/null)" \
    || die "cannot read the application size policy at $SIZE_POLICY"
[[ "$POLICY_MAX_TAPP_BYTES" =~ ^[1-9][0-9]*$ ]] \
    || die "$SIZE_POLICY must hold a positive byte count"
MAX_TAPP_BYTES="${MAX_TAPP_BYTES:-$POLICY_MAX_TAPP_BYTES}"

[[ "$MAX_TAPP_BYTES" =~ ^[0-9]+$ ]] \
    || die "MAX_TAPP_BYTES must be an integer"
(( MAX_TAPP_BYTES <= POLICY_MAX_TAPP_BYTES )) \
    || die "MAX_TAPP_BYTES cannot exceed the $POLICY_MAX_TAPP_BYTES-byte application policy"

# --- per-app size budgets ---------------------------------------------------
# Each app's budget is far below the format ceiling above. Checking only the
# ceiling let an app grow fourfold and surface as "over 8 MB" when a whole
# layer failed; here the package that grew is named, with its size and its
# budget, the moment it is packed. tools/perf/tapp_size_ratchet.py reads the
# same files (budget.toml's [apps."<id>"] tables, plus every raise file's).
BUDGET_DIR="${BUDGET_DIR:-$(dirname "${BASH_SOURCE[0]}")/../../tools/perf/tapp-size-budget}"
TAPP_ARCH="${TAPP_ARCH:-$(uname -m)}"
TAPP_BUDGET="${TAPP_BUDGET:-enforce}"
case "$TAPP_BUDGET" in
    enforce|report) ;;
    *) die "TAPP_BUDGET must be enforce or report" ;;
esac
[ -f "$BUDGET_DIR/budget.toml" ] || die "no per-app size budgets at $BUDGET_DIR/budget.toml"

# The budget of package $1 on architecture $2 in bytes, raises included;
# nothing when the app has none (it is not in the default collection).
app_budget() {
    local files=("$BUDGET_DIR/budget.toml") raise
    for raise in "$BUDGET_DIR"/raises/*.toml; do
        [ -f "$raise" ] && files+=("$raise")
    done
    awk -v section="[apps.\"$1\"]" -v arch="$2" '
        FNR == 1 { base = (FILENAME ~ /budget\.toml$/); inside = 0 }
        {
            line = $0
            sub(/#.*/, "", line)
        }
        line ~ /^[[:space:]]*\[/ {
            gsub(/[[:space:]]/, "", line)
            inside = (line == section)
            next
        }
        inside && line ~ ("^[[:space:]]*" arch "[[:space:]]*=[[:space:]]*[0-9]+[[:space:]]*$") {
            value = line
            sub(/^[^=]*=[[:space:]]*/, "", value)
            if (base) { budget = value + 0; found = 1 } else { raised += value }
        }
        END { if (found) printf "%d\n", budget + raised }
    ' "${files[@]}"
}

# Hold package $2 ($3 bytes, named $1 in messages) to its budget. Prints the
# refusal and fails when it is over; a missing budget is reported, not refused.
check_budget() {
    local label=$1 id=$2 bytes=$3 budget
    budget="$(app_budget "$id" "$TAPP_ARCH")" || die "cannot read $BUDGET_DIR"
    if [ -z "$budget" ]; then
        msg "$label: $bytes bytes; no $TAPP_ARCH budget in $BUDGET_DIR/budget.toml"
        return 0
    fi
    if [ "$bytes" -le "$budget" ]; then
        return 0
    fi
    local over="$label: package is $bytes bytes, over its $TAPP_ARCH budget of $budget bytes (+$((bytes - budget))) in $BUDGET_DIR/budget.toml"
    if [ "$TAPP_BUDGET" = report ]; then
        msg "over budget (reported, not refused): $over"
        return 0
    fi
    printf '%s\n' "$over"
    return 1
}

for t in "$PKG_BIN" "$INSTALL_BIN" "$BUNDLE_BIN"; do
    [ -x "$t" ] || die "$t not found or not executable"
done

mkdir -p "$STAGE" "$STORE"
initial_rows="$(grep -c '^\[\[entry\]\]' "$STORE/registry.toml" 2>/dev/null || true)"

# --- source-package signing identity ---------------------------------------
# The caller's IDENTITY_JSON when it names one, else minted here and discarded
# with the stage dir. The signature's job (binding manifest to payload during
# install) is done once the packages are unpacked into the store, and the key
# is in no trust list. It does name every package: the publisher key is in the
# signed root, whose hash is the package id and the payload tree's directory
# name. So a key minted per build gives identical binaries new paths and new
# cohort archives in every image; Build / OS dev passes one identity for all
# its builds instead (os/Containerfile).
prebuilt=0
if [ -n "$TAPP_DIR" ] && compgen -G "$TAPP_DIR/*.tapp" >/dev/null; then
    prebuilt=1
    PUBKEY=''
else
    if [ ! -f "$IDENTITY_JSON" ]; then
        msg "generating app-signing identity at $IDENTITY_JSON"
        "$BUNDLE_BIN" key generate --out "$IDENTITY_JSON"
    fi
    PUBKEY="$("$BUNDLE_BIN" key pubkey --identity "$IDENTITY_JSON")"
    [ -n "$PUBKEY" ] || die "could not read the signing pubkey"
    if [ -n "$EXPECTED_PUBKEY" ] && [ "${PUBKEY,,}" != "${EXPECTED_PUBKEY,,}" ]; then
        die "source-pack identity does not match EXPECTED_PUBKEY"
    fi
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
    for tapp in "$TAPP_DIR"/*.tapp; do
        tapp_bytes=$(wc -c < "$tapp")
        if [ "$tapp_bytes" -gt "$MAX_TAPP_BYTES" ]; then
            failed+=("$(basename "$tapp"): $tapp_bytes bytes exceeds $MAX_TAPP_BYTES")
            continue
        fi
        if ! "$PKG_BIN" verify "$tapp" >"$STAGE/$(basename "$tapp").verify.log" 2>&1; then
            failed+=("$(basename "$tapp"): signature or content verification failed")
            continue
        fi
        package_details=$("$PKG_BIN" inspect "$tapp")
        budget_id=$(printf '%s\n' "$package_details" \
            | sed -n 's/^id  *= *"\([^"]*\)".*/\1/p' | head -1)
        if [ -n "$budget_id" ] && ! over=$(check_budget "$(basename "$tapp")" "$budget_id" "$tapp_bytes"); then
            failed+=("$over")
            continue
        fi
        package_pubkey=$(printf '%s\n' "$package_details" \
            | sed -n 's/^identity  *= *"ed25519:\([0-9a-fA-F]\{64\}\)".*/\1/p' \
            | head -1)
        [ -n "$package_pubkey" ] \
            || { failed+=("$(basename "$tapp"): publisher identity is missing"); continue; }
        if [ -z "$PUBKEY" ]; then
            PUBKEY=$package_pubkey
        elif [ "$PUBKEY" != "$package_pubkey" ]; then
            failed+=("$(basename "$tapp"): collection mixes publisher identities")
            continue
        fi
        if [ -n "${COHORT_TAPP_DIR:-}" ]; then
            package_id=$(printf '%s\n' "$package_details" \
                | sed -n 's/^id  *= *"\([^"]*\)".*/\1/p' | head -1)
            [[ "$package_id" =~ ^[A-Za-z0-9][A-Za-z0-9._+-]*$ ]] \
                || { failed+=("$(basename "$tapp"): unsafe or missing application id"); continue; }
        fi
        if ! "$INSTALL_BIN" install "$tapp" --allow-self-signed \
                >"$STAGE/$(basename "$tapp").install.log" 2>&1; then
            failed+=("$(basename "$tapp"): install failed")
            continue
        fi
        if [ -n "${COHORT_TAPP_DIR:-}" ]; then
            mkdir -p "$COHORT_TAPP_DIR"
            cp "$tapp" "$COHORT_TAPP_DIR/$package_id.tapp"
        fi
        shipped+=("$(basename "$tapp")")
    done
    [ -z "$PUBKEY_OUT" ] || { mkdir -p "$(dirname "$PUBKEY_OUT")"; printf '%s\n' "$PUBKEY" > "$PUBKEY_OUT"; }
    if [ -n "$EXPECTED_PUBKEY" ] && [ "${PUBKEY,,}" != "${EXPECTED_PUBKEY,,}" ]; then
        die "prebuilt collection publisher does not match EXPECTED_PUBKEY"
    fi
else

[[ "$RUNTIME_ID" == */*+native.* ]] \
    || die "RUNTIME_ID must name the runtime build the apps link (tilo-runtime/<release>+native.<digest>)"

for manifest in "$REPO"/apps/*/tilo-app.toml; do
    if [ -n "${APP_DIR:-}" ] && [ "$(basename "$(dirname "$manifest")")" != "$APP_DIR" ]; then continue; fi
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

    # `tilo-app-package pack` refuses a manifest whose publisher.identity disagrees
    # with the signing key, so stamp this build's pubkey into a copy. The
    # committed manifest is never modified.
    staged_manifest="$STAGE/manifest-$id.toml"
    sed "s|^\(identity  *= *\)\"ed25519:[0-9a-fA-F]*\"|\1\"ed25519:$PUBKEY\"|" \
        "$manifest" > "$staged_manifest"
    grep -q "ed25519:$PUBKEY" "$staged_manifest" \
        || { failed+=("$app ($id): could not stamp publisher identity"); continue; }
    # The binary links this exact runtime build, so the package names it,
    # not just the release the manifest was written for.
    "$(dirname "${BASH_SOURCE[0]}")/stamp-runtime.sh" "$staged_manifest" "$RUNTIME_ID" \
        || { failed+=("$app ($id): could not stamp runtime $RUNTIME_ID"); continue; }

    tapp="$STAGE/$id.tapp"
    if ! "$PKG_BIN" pack \
            --manifest "$staged_manifest" \
            --payload "$payload" \
            --output "$tapp" \
            --identity "$IDENTITY_JSON" >"$STAGE/$id.pack.log" 2>&1; then
        failed+=("$app ($id): pack failed — $(tail -3 "$STAGE/$id.pack.log" | tr '\n' ' ')")
        continue
    fi

    tapp_bytes="$(stat -c %s "$tapp")"
    # The app's own budget first: it names what grew long before the ceiling.
    if ! over=$(check_budget "$app ($id)" "$id" "$tapp_bytes"); then
        failed+=("$over")
        continue
    fi
    if [ "$tapp_bytes" -gt "$MAX_TAPP_BYTES" ]; then
        failed+=("$app ($id): package is $tapp_bytes bytes; limit is $MAX_TAPP_BYTES")
        continue
    fi

    # Verify against the key we just signed with, so a mis-signed package
    # fails here rather than silently at first launch.
    if ! "$PKG_BIN" verify --trusted-key "$PUBKEY" "$tapp" >"$STAGE/$id.verify.log" 2>&1; then
        failed+=("$app ($id): verify failed — $(tail -3 "$STAGE/$id.verify.log" | tr '\n' ' ')")
        continue
    fi

    # Self-signed is what this is: the image publisher signed it, and no
    # third-party attestor vouched for it. Saying so explicitly is honest,
    # and it is the same flag a user passes for their own local package.
    if ! "$INSTALL_BIN" install "$tapp" --allow-self-signed \
            >"$STAGE/$id.install.log" 2>&1; then
        failed+=("$app ($id): install failed — $(tail -3 "$STAGE/$id.install.log" | tr '\n' ' ')")
        continue
    fi

    if [ -n "${COHORT_TAPP_DIR:-}" ]; then
        mkdir -p "$COHORT_TAPP_DIR"
        cp "$tapp" "$COHORT_TAPP_DIR/$id.tapp"
    fi
    if [ -n "${TAPP_OUT:-}" ]; then
        [ -n "${APP_DIR:-}" ] || die "TAPP_OUT requires a selected APP_DIR"
        cp "$tapp" "$TAPP_OUT"
    fi
    shipped+=("$id ($tapp_bytes bytes)")
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

# The trees are materialised and the image's store is read-only: it never
# installs again, so the installer's content store, staging and writer lock
# would only double the payload bytes in the image.
rm -rf -- "$STORE/content" "$STORE/tmp" "$STORE/.lock"

# The registry is what the launcher reads; an empty one means an empty
# palette, which is the failure this whole step exists to prevent.
[ -s "$STORE/registry.toml" ] || die "$STORE/registry.toml is missing or empty"
rows="$(grep -c '^\[\[entry\]\]' "$STORE/registry.toml" || true)"
expected_rows=$((initial_rows + ${#shipped[@]}))
[ "$rows" = "$expected_rows" ] \
    || die "registry has $rows row(s), expected $initial_rows existing + ${#shipped[@]} installed"
msg "registry.toml lists $rows package(s)"

# Every runtime build a shipped app pins -- the Browser's engine included --
# must ship with the image at that exact version, or the app boots into
# "not installed". The check reads the installed manifests, so no list of
# runtimes is kept beside the apps.
STORE="$STORE" RUNTIME_ROOT="${RUNTIME_ROOT:-/usr/lib/tilo-runtime}" BUNDLE_BIN="$BUNDLE_BIN" \
    "$(dirname "${BASH_SOURCE[0]}")/check-shipped-runtimes.sh" \
    || die "an app pins a runtime build the image does not ship"
