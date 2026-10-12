#!/usr/bin/env bash
# Package one app twice from this checkout and fail on any byte that differs.
#
#   tools/apps/check-reproducible-tapp.sh APP
#
# APP is a directory under apps/. Both builds go through dist/apps/package-apps.sh,
# the path Release / apps publishes through: the thin binary, the runtime
# bundle it links, the signed tapp, the registry's runtime package and the
# collection archive. The first takes Cargo's and the compiler cache's outputs
# as they are; the second removes the app's own build outputs and compiles
# them again with the compiler cache told to recompile (SCCACHE_RECACHE), so a
# compile that depends on anything but its inputs shows as a different binary.
# Both sign with one identity made for this check: Ed25519 signs
# deterministically, so a difference is in what was signed, never in the
# signature's randomness. tools/apps/compare_build_outputs.py names what differs.
#
# Every OS image and every update carries these packages: two builds of the
# same source must give the same bytes, or the update re-sends what did not
# change.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
app=${1:?usage: check-reproducible-tapp.sh APP}
[[ $app =~ ^[a-z0-9][a-z0-9-]*$ && -f $repo/apps/$app/tilo-app.toml ]] || {
    printf 'check-reproducible-tapp: no app %s under apps/\n' "$app" >&2
    exit 2
}
package=$(sed -n 's/^name *= *"\([^"]*\)".*/\1/p' "$repo/apps/$app/Cargo.toml" | head -1)
[[ -n $package ]] || { printf 'check-reproducible-tapp: apps/%s/Cargo.toml names no package\n' "$app" >&2; exit 2; }
work=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/tilo-reproducible-tapp.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
# Throwaway identities in the shape tilo-app-package and tilo-runtime-bundle
# read ({"secret_key_hex": …}), made without building either tool first.
(
    umask 077
    for name in app-publisher runtime-publisher; do
        python3 -c 'import json, secrets, sys; json.dump({"secret_key_hex": secrets.token_hex(32)}, sys.stdout)' \
            > "$work/$name.json"
    done
)

build() {
    env -u TILO_APP_RELEASE_SOURCE -u TILO_APP_RELEASE_SEQUENCE -u TILO_APP_DEV_SOURCE \
        -u TILO_APP_COMPILE_ONLY \
        TILO_RELEASE_APPS="$app" TILO_APP_RELEASE_OUT="$work/$1" \
        TAPP_PUBLISHER_IDENTITY_FILE="$work/app-publisher.json" \
        RUNTIME_BUNDLE_IDENTITY_FILE="$work/runtime-publisher.json" \
        SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-0}" \
        "$repo/dist/apps/package-apps.sh" >&2
}

started=$(date +%s)
build first
(cd "$repo" && cargo clean --profile thin-app -p "$package")
SCCACHE_RECACHE=1 build second
printf 'check-reproducible-tapp: built %s twice in %ss\n' "$app" "$(( $(date +%s) - started ))" >&2

# work/ holds each build's private scratch (payload copies, staged manifests).
python3 "$repo/tools/apps/compare_build_outputs.py" "$work/first" "$work/second" --ignore work
