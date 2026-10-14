#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# check-shipped-runtimes.sh -- fail unless every runtime a shipped app pins is
# shipped with it.
#
# An app's manifest names the runtime builds it runs on: `[package].runtime`
# and the `[package].optional-runtimes` it mounts when present (the Browser's
# Chromium engine). The launcher resolves each by exact version, so an image
# whose store lists an app but whose /usr/lib/tilo-runtime lacks that build
# boots into an app that says "not installed". This reads the manifests of the
# packages actually installed in STORE -- not a list kept beside them -- and
# requires each named build to be a bundle that verifies, so the two cannot
# drift apart without failing the build.
#
# Env:
#   STORE         the installed app store          (default /usr/share/tilo-apps)
#   RUNTIME_ROOT  where runtime bundles are shipped (default /usr/lib/tilo-runtime)
#   BUNDLE_BIN    tilo-runtime-bundle               (default tilo-runtime-bundle)
# ---------------------------------------------------------------------------
set -euo pipefail

STORE="${STORE:-/usr/share/tilo-apps}"
RUNTIME_ROOT="${RUNTIME_ROOT:-/usr/lib/tilo-runtime}"
BUNDLE_BIN="${BUNDLE_BIN:-tilo-runtime-bundle}"

fail=0
checked=0
for manifest in "$STORE"/pkgs/*.pkg/manifest.toml; do
    [ -e "$manifest" ] || continue
    # `runtime = "..."` and every string of the optional-runtimes array,
    # which may span lines.
    refs="$(awk '
        /^runtime[ \t]*=/ { print; next }
        /^optional-runtimes[ \t]*=/ { inlist = 1 }
        inlist { print }
        inlist && /\]/ { inlist = 0 }
    ' "$manifest" | grep -o '"tilo-runtime/[^"]*"' | tr -d '"' | sort -u)"
    for ref in $refs; do
        version="${ref#tilo-runtime/}"
        checked=$((checked + 1))
        dir="$RUNTIME_ROOT/$version"
        if [ ! -f "$dir/bundle.toml" ]; then
            printf 'check-shipped-runtimes: %s pins %s, which is not shipped at %s\n' \
                "$manifest" "$ref" "$dir" >&2
            fail=1
        elif ! "$BUNDLE_BIN" verify "$dir" >/dev/null 2>&1; then
            printf 'check-shipped-runtimes: %s pins %s, whose bundle at %s does not verify\n' \
                "$manifest" "$ref" "$dir" >&2
            fail=1
        fi
    done
done
[ "$fail" = 0 ] || exit 1
printf 'check-shipped-runtimes: %s runtime pin(s) shipped and verified\n' "$checked" >&2
