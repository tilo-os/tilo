#!/usr/bin/env bash
#
# pack-all-apps.sh — build every qOS app that ships a qos-app.toml,
# then pack it into a .qpkg and (optionally) install it.
#
# Layout
# ------
# The script walks apps/*/qos-app.toml. For each match it:
#   1. Reads the `package.id` and `entrypoint.bin` values from the
#      manifest so it knows what binary to copy and where.
#   2. Builds the cargo package whose crate name matches the app
#      directory (e.g. apps/settings/ → `cargo build -p settings`).
#   3. Assembles a payload dir under `target/qpkgs/<name>-payload/`
#      that mirrors the entrypoint path (`bin/<name>` by default).
#   4. Runs `qos-pkg pack` and writes the .qpkg into
#      `target/qpkgs/<name>.qpkg`.
#   5. If --install was passed, `qos-install install`s each one
#      with `--allow-self-signed`.
#
# Usage
# -----
#   scripts/pack-all-apps.sh            # build + pack all
#   scripts/pack-all-apps.sh --install  # + install into the local store
#   scripts/pack-all-apps.sh --apps notes,terminal
#
# Exit status: 0 if EVERY targeted app was packed successfully;
# non-zero if any failed. Each failure is logged; the run does not
# abort on the first error so we still see the full fleet status.
#
# Notes
# -----
# * cargo runs go through `distrobox enter my-distrobox` because
#   the host lacks a few build deps (see CLAUDE.md's `distrobox`
#   section).
# * `apps/browser` intentionally lacks a qos-app.toml — Servo's
#   build is broken on main. This script only walks manifests that
#   exist, so browser is silently skipped.
# * The signing identity is generated fresh on every run; --install
#   accepts it via --allow-self-signed. Wire in a real key at CI
#   time by passing `QOS_PKG_IDENTITY=path/to/identity.json` and
#   the script forwards it to `qos-pkg pack --identity`.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

INSTALL=0
FILTER=""
DISTROBOX="${QOS_DISTROBOX:-my-distrobox}"
IDENTITY="${QOS_PKG_IDENTITY:-}"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --install) INSTALL=1; shift ;;
        --apps)    FILTER="$2"; shift 2 ;;
        --distrobox) DISTROBOX="$2"; shift 2 ;;
        -h|--help)
            grep '^#' "$0" | sed 's/^# *//'
            exit 0
            ;;
        *) echo "unknown flag: $1" >&2; exit 2 ;;
    esac
done

OUT="$REPO_ROOT/target/qpkgs"
mkdir -p "$OUT"

# ------------------------------------------------------------------
# 1. Enumerate the apps that ship a qos-app.toml.
# ------------------------------------------------------------------
APPS=()
for manifest in apps/*/qos-app.toml; do
    [[ -f "$manifest" ]] || continue
    name="$(basename "$(dirname "$manifest")")"
    if [[ -n "$FILTER" ]]; then
        case ",$FILTER," in
            *",$name,"*) ;;
            *) continue ;;
        esac
    fi
    APPS+=("$name")
done
if [[ ${#APPS[@]} -eq 0 ]]; then
    echo "no apps matched (filter=${FILTER:-<none>})" >&2
    exit 1
fi

echo "==> apps to pack: ${APPS[*]}"

# ------------------------------------------------------------------
# 2. Build packaging tools once, plus every targeted app crate.
#    Building them all in one `cargo build` command reuses the
#    dependency graph — much faster than one build per app.
# ------------------------------------------------------------------
PKG_FLAGS=(-p pkg-cli -p install -p launcher)
for app in "${APPS[@]}"; do
    # Cargo package name === directory basename.
    PKG_FLAGS+=(-p "$app")
done

echo "==> cargo build --release ${PKG_FLAGS[*]}"
distrobox enter "$DISTROBOX" -- bash -c "cd '$REPO_ROOT' && cargo build --release ${PKG_FLAGS[*]}"

# ------------------------------------------------------------------
# 3. Pack + (optionally) install per app.
# ------------------------------------------------------------------
QOS_PKG="$REPO_ROOT/target/release/qos-pkg"
QOS_INSTALL="$REPO_ROOT/target/release/qos-install"

FAILED=()
PACKED=()
for app in "${APPS[@]}"; do
    manifest="apps/$app/qos-app.toml"

    # Discover the entrypoint bin name from the manifest. Falls
    # back to `bin/$app` — every current manifest follows that
    # convention.
    entry="$(awk -F'"' '/^bin[[:space:]]*=/ { print $2; exit }' "$manifest" 2>/dev/null || echo "")"
    entry="${entry:-bin/$app}"

    payload="$OUT/${app}-payload"
    rm -rf "$payload"
    mkdir -p "$payload/$(dirname "$entry")"

    # The cargo bin name matches the entry basename for every app
    # on main; if that assumption breaks the copy will fail and
    # this app is logged as red.
    binname="$(basename "$entry")"
    if [[ ! -x "target/release/$binname" ]]; then
        echo "  RED $app — missing target/release/$binname (build failed?)"
        FAILED+=("$app:build")
        continue
    fi
    cp "target/release/$binname" "$payload/$entry"

    output="$OUT/${app}.qpkg"
    IDENT_ARG=()
    [[ -n "$IDENTITY" ]] && IDENT_ARG=(--identity "$IDENTITY")

    if ! "$QOS_PKG" pack \
        --manifest "$manifest" \
        --payload  "$payload" \
        --output   "$output" \
        "${IDENT_ARG[@]}" >/dev/null; then
        echo "  RED $app — qos-pkg pack failed"
        FAILED+=("$app:pack")
        continue
    fi

    PACKED+=("$app")
    echo "  GREEN $app — $(du -h "$output" | cut -f1) at ${output#$REPO_ROOT/}"

    if [[ $INSTALL -eq 1 ]]; then
        if ! "$QOS_INSTALL" install "$output" --allow-self-signed >/dev/null 2>&1; then
            echo "        install failed"
            FAILED+=("$app:install")
        fi
    fi
done

# ------------------------------------------------------------------
# 4. Summary.
# ------------------------------------------------------------------
echo ""
echo "==> summary"
echo "   packed: ${#PACKED[@]}/${#APPS[@]}"
if [[ ${#FAILED[@]} -gt 0 ]]; then
    echo "   failed: ${FAILED[*]}"
    exit 1
fi
