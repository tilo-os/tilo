#!/usr/bin/env bash
# Fail before allocating image builders when the pinned apps predate an app fix.
set -euo pipefail
collection=${1:?collection source commit required}
target=${2:?OS source commit required}
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
for commit in "$collection" "$target"; do
  [[ $commit =~ ^[0-9a-f]{40}$ ]] && git -C "$repo" cat-file -e "$commit^{commit}" || {
    echo "::error::cannot verify app collection source $commit" >&2
    exit 1
  }
done
# Use the publication workflow's input set, including shared UI/runtime code.
mapfile -t inputs < <(awk '
  /^    paths:$/ { on = 1; next }
  on && /^      - / { gsub(/^      - "|"$/, ""); print ":(glob)" $0; next }
  on { exit }
' "$repo/.forgejo/workflows/release-apps.yml")
(( ${#inputs[@]} > 10 )) || {
  echo '::error::cannot read the Release / apps input policy' >&2
  exit 1
}
if ! git -C "$repo" diff --quiet "$collection" "$target" -- "${inputs[@]}"; then
  echo "::error::pinned apps from $collection differ from OS $target; publish a compatible Release / apps collection before proving a release. For branch desktop tests, use Dev / VM acceptance with component swaps (PR #2463)." >&2
  git -C "$repo" diff --name-only "$collection" "$target" -- "${inputs[@]}" >&2
  exit 1
fi
echo "::notice::app collection $collection has the app inputs of OS $target"
