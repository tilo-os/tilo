#!/usr/bin/env bash
# Every remote workflow action is executable third-party code. Require an exact
# commit rather than a mutable tag or branch across the entire workflow set.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflows="$repo/.forgejo/workflows"
fail=0
manifest="$repo/os/external-assets/github-assets.tsv"
package_workflow="$workflows/package-desktop.yml"
containerfile="$repo/os/Containerfile"
github_cache_workflows=(
  "$workflows/build-builder-image.yml"
  "$workflows/build-image.yml"
  "$workflows/package-desktop.yml"
  "$workflows/release-imager.yml"
  "$workflows/snapshot-rpms.yml"
)

while IFS=: read -r file line match; do
  ref="${match#uses:}"
  ref="${ref#${ref%%[![:space:]]*}}"
  case "$ref" in
    ./*|docker://*) continue ;;
  esac
  revision="${ref##*@}"
  if [[ ! "$revision" =~ ^[0-9a-fA-F]{40}$ ]]; then
    printf 'mutable workflow action: %s:%s: %s\n' "$file" "$line" "$ref" >&2
    fail=1
  fi
done < <(grep -R -n -o -E 'uses:[[:space:]]*[^[:space:]#]+' "$workflows")

if [ "$fail" = 0 ]; then
  printf 'ok    every remote workflow action is pinned to a full commit SHA\n'
fi

declare -A asset_names=()
while IFS=$'\t' read -r checksum name url extra; do
  case "$checksum" in ''|'#'*) continue ;; esac
  if [[ ! "$checksum" =~ ^[0-9a-f]{64}$ ]] \
    || [[ ! "$name" =~ ^[A-Za-z0-9][A-Za-z0-9._+-]*$ ]] \
    || [[ ! "$url" =~ ^https:// ]] \
    || [[ -n "$extra" ]] \
    || [[ -n "${asset_names[$name]:-}" ]]; then
    printf 'invalid or duplicate external-asset manifest row: %s\n' "$name" >&2
    fail=1
    continue
  fi
  asset_names[$name]=1
  case "$url" in
    https://raw.githubusercontent.com/*)
      [[ "$url" =~ /[0-9a-f]{40}/ ]] || {
        printf 'mutable raw GitHub URL: %s\n' "$url" >&2
        fail=1
      }
      ;;
    https://github.com/*/archive/*)
      [[ "$url" =~ /archive/[0-9a-f]{40}\.tar\.gz$ ]] || {
        printf 'mutable GitHub source archive URL: %s\n' "$url" >&2
        fail=1
      }
      ;;
    https://github.com/*)
      [[ "$url" == */releases/download/* ]] && [[ "$url" != */releases/download/latest/* ]] || {
        printf 'unversioned GitHub release URL: %s\n' "$url" >&2
        fail=1
      }
      ;;
  esac
done < "$manifest"
if [ "$fail" = 0 ]; then
  printf 'ok    external GitHub assets have safe names, immutable revisions, and SHA-256 pins\n'
fi

for workflow in "${github_cache_workflows[@]}"; do
  if ! grep -Fq 'github-assets-all-v1-' "$workflow" \
    || ! grep -Fqx '            github-assets-all-v1-' "$workflow"; then
    printf 'workflow does not reuse prior verified GitHub cache generations: %s\n' \
      "${workflow#$repo/}" >&2
    fail=1
  fi
done
if ! grep -Fq "printf '[qos] pruning stale cached asset %s" \
  "$repo/os/dev/cache-github-assets.sh"; then
  printf 'GitHub cache cannot reconcile a prior manifest generation\n' >&2
  fail=1
fi
if [ "$fail" = 0 ]; then
  printf 'ok    GitHub inputs reuse prior manifest cache generations\n'
fi

for target in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
  name="sccache-v0.17.0-$target.tar.gz"
  if ! awk -F '\t' -v n="$name" \
    '$1 !~ /^#/ && $2 == n && $3 ~ /^https:\/\/github\.com\// { found=1 }
     END { exit !found }' "$manifest"; then
    printf 'missing checksum-pinned GitHub cache entry: %s\n' "$name" >&2
    fail=1
  fi
done
if ! grep -Fq 'key: desktop-external-inputs-${{ hashFiles('\''os/external-assets/github-assets.tsv'\'') }}' \
    "$package_workflow" \
  || ! grep -Fq 'github-assets-all-v1-${{ hashFiles('\''os/external-assets/github-assets.tsv'\'') }}' \
    "$package_workflow" \
  || ! grep -Fq 'os/dev/cache-github-assets.sh "sccache-v0.17.0-${target}.tar.gz"' \
    "$package_workflow" \
  || ! grep -Fq 'cached_archive="$repo_root/os/external-assets/cache/${name}.tar.gz"' \
    "$repo/dist/ci/install-sccache.sh"; then
  printf 'desktop package CI bypasses the verified GitHub input cache\n' >&2
  fail=1
else
  printf 'ok    desktop package CI restores checksum-pinned GitHub inputs\n'
fi

if grep -Fq 'ENV GOPROXY=https://proxy.golang.org GOSUMDB=sum.golang.org' "$containerfile" \
  && grep -Fq -- '--mount=type=cache,target=/go/pkg/mod' "$containerfile" \
  && grep -Fq -- '--mount=type=cache,target=/root/.cache/go-build' "$containerfile"; then
  printf 'ok    Go helper builds use verified persistent proxy caches without GitHub fallback\n'
else
  printf 'Go helper builds can bypass the verified module proxy or redownload unchanged modules\n' >&2
  fail=1
fi
exit "$fail"
