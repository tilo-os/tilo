#!/usr/bin/env bash
# Materialize every immutable external input before a container build. Most
# originate on GitHub; the same cache also carries the reviewed, content-pinned
# virtio-win RPM because its Fedora repository publishes that package unsigned.
# Keeping downloads outside BuildKit makes interrupted transfers resumable.
#
# In t, the os-assets check runs this once per manifest and its outputs fill
# the cache of every check that needs it. Such a check sets
# TILO_ASSET_OFFLINE=1: the requested assets are only verified, a missing one
# fails instead of being downloaded, and the cache may be read-only.
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd -- "$script_dir/../.." && pwd)"
manifest="$repo/os/external-assets/github-assets.tsv"
cache_dir="${TILO_GITHUB_ASSET_CACHE:-$repo/os/external-assets/cache}"
offline="${TILO_ASSET_OFFLINE:-}"
[ -n "$offline" ] || mkdir -p "$cache_dir"
# Every checkout has its own cache_dir (it is a build context inside the
# worktree), so without a shared store each new worktree downloaded every asset
# again. The store is per user and keyed by name; a file is only ever taken from
# it after its pinned checksum verifies. TILO_ASSET_STORE= (empty) turns it off.
store="${TILO_ASSET_STORE-${XDG_CACHE_HOME:-${HOME:-/nonexistent}/.cache}/tilo/external-assets}"
[ -z "$offline" ] || store=""
if [ -n "$store" ] && ! mkdir -p "$store" 2>/dev/null; then
  store=""
fi
# Hard link when the store shares the filesystem, else copy (reflink if possible).
share() {
  ln -f -- "$1" "$2" 2>/dev/null || cp --reflink=auto -f -- "$1" "$2"
}

wanted() {
  local candidate="$1" requested
  [ "$#" -gt 1 ] || return 0
  shift
  for requested in "$@"; do
    [ "$candidate" != "$requested" ] || return 0
  done
  return 1
}

for requested in "$@"; do
  awk -F '\t' -v n="$requested" \
    '$1 !~ /^#/ && $2 == n { found=1 } END { exit !found }' "$manifest" \
    || { printf 'requested asset is not in the manifest: %s\n' "$requested" >&2; exit 1; }
done

verify() {
  local expected="$1" path="$2" actual
  [ -f "$path" ] || return 1
  actual="$(sha256sum "$path" | cut -d ' ' -f 1)"
  [ "$actual" = "$expected" ]
}

while IFS=$'\t' read -r expected name url; do
  case "$expected" in ''|'#'*) continue ;; esac
  wanted "$name" "$@" || continue
  target="$cache_dir/$name"
  if [ -n "$offline" ]; then
    verify "$expected" "$target" || {
      printf '%s is not in %s with its pinned checksum (in t, the check needs os-assets)\n' \
        "$name" "$cache_dir" >&2
      exit 1
    }
    continue
  fi
  if verify "$expected" "$target"; then
    [ ! -e "$target.part" ] || unlink -- "$target.part"
    printf '[tilo] cached %s\n' "$name"
    [ -z "$store" ] || [ -e "$store/$name" ] || share "$target" "$store/$name" || true
    continue
  fi
  if [ -n "$store" ] && verify "$expected" "$store/$name" && share "$store/$name" "$target"; then
    printf '[tilo] cached %s (shared store %s)\n' "$name" "$store"
    continue
  fi

  part="$target.part"
  printf '[tilo] fetching %s\n' "$name"
  curl --location --fail --show-error \
    --retry 12 --retry-all-errors --retry-delay 3 --connect-timeout 30 \
    --speed-limit 1024 --speed-time 120 --continue-at - \
    --output "$part" "$url"
  if ! verify "$expected" "$part"; then
    printf 'checksum mismatch for %s: expected %s, got %s\n' \
      "$name" "$expected" "$(sha256sum "$part" | cut -d ' ' -f 1)" >&2
    unlink -- "$part"
    exit 1
  fi
  mv -f -- "$part" "$target"
  [ -z "$store" ] || share "$target" "$store/$name" || true
done < "$manifest"
[ -z "$offline" ] || exit 0

# A prefix restore can contain entries removed by a newer manifest. Drop those
# before exposing the directory as a container build context, then save the
# reconciled directory under the new exact manifest key.
while IFS= read -r cached; do
  name="${cached##*/}"
  case "$name" in *.part) continue ;; esac
  awk -F '\t' -v n="$name" '$1 !~ /^#/ && $2 == n { found=1 } END { exit !found }' \
    "$manifest" || {
      printf '[tilo] pruning stale cached asset %s\n' "$name"
      unlink -- "$cached"
    }
done < <(find "$cache_dir" -maxdepth 1 -type f -print)

# A named BuildKit/Podman context is read by a container process rather than
# copied by the frontend. Normalize permissions and, on enforcing SELinux
# hosts, its label so rootless builds can mount the verified files read-only.
chmod 0755 "$cache_dir"
find "$cache_dir" -maxdepth 1 -type f ! -name '*.part' -exec chmod 0644 {} +
if command -v selinuxenabled >/dev/null 2>&1 && selinuxenabled; then
  # A previous private container mount may leave a valid-looking type with
  # container-specific MCS categories (for example c123,c456). A later build
  # receives different categories and is then denied despite mode 0644. The
  # cache is an immutable, checksum-verified build context shared by jobs, so
  # give it the shared container level as well as the correct type.
  chcon -R -t container_file_t -l s0 -- "$cache_dir"
fi
