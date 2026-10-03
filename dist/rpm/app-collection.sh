#!/usr/bin/env bash
# Build the default desktop app collection into OUT, or keep the one already
# there when it was built from the same source: the commit, every uncommitted
# change to tracked files, and the app list. A rerun after a later packaging
# failure reuses it; a new commit or an edit rebuilds it. Outside a git
# checkout nothing identifies the source, so it is always rebuilt.
#
#   dist/rpm/app-collection.sh OUT
set -euo pipefail

[[ $# == 1 ]] || { echo "usage: $0 OUT" >&2; exit 2; }
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
out=$1
arch=$(uname -m)
collection=$out/tilo-apps-$arch.tar.gz
stamp=$out/built-from
package_apps=${TILO_PACKAGE_APPS:-$repo_root/dist/apps/package-apps.sh}

app_names=$(python3 "$repo_root/tools/apps/app_release.py" --collection desktop \
    | jq -r '.catalog[].name' | paste -sd' ' -)
[[ -n $app_names ]] || { echo "desktop app collection is empty" >&2; exit 1; }

source_id() {
    local head changes
    head=$(git -C "$repo_root" rev-parse HEAD 2>/dev/null) || return 0
    changes=$(git -C "$repo_root" diff HEAD --binary | sha256sum)
    printf 'commit %s\nchanges %s\napps %s\narch %s\n' \
        "$head" "${changes%% *}" "$app_names" "$arch"
}
built_from=$(source_id)

if [[ -n $built_from && -f $collection && -f $stamp && $(<"$stamp") == "$built_from" ]]; then
    echo "app collection already built from this source: $collection"
    exit 0
fi
rm -f -- "$stamp"
TILO_RELEASE_APPS="$app_names" TILO_APP_RELEASE_OUT="$out" "$package_apps"
[[ -f $collection ]] || { echo "app packaging produced no $collection" >&2; exit 1; }
# Recorded from before the build: an edit made while it ran rebuilds next time.
[[ -z $built_from ]] || printf '%s\n' "$built_from" >"$stamp"
