#!/usr/bin/env bash
# Materialize every immutable external input before a container build. Most
# originate on GitHub; the same cache also carries the reviewed, content-pinned
# virtio-win RPM because its Fedora repository publishes that package unsigned.
# Keeping downloads outside BuildKit makes interrupted transfers resumable and
# lets Forgejo's cache survive ephemeral runners.
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd -- "$script_dir/../.." && pwd)"
manifest="$repo/os/external-assets/github-assets.tsv"
cache_dir="${QOS_GITHUB_ASSET_CACHE:-$repo/os/external-assets/cache}"
mkdir -p "$cache_dir"

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
  if verify "$expected" "$target"; then
    [ ! -e "$target.part" ] || unlink -- "$target.part"
    printf '[qos] cached %s\n' "$name"
    continue
  fi

  part="$target.part"
  printf '[qos] fetching %s\n' "$name"
  verified=0
  for integrity_attempt in 1 2; do
    if ! curl --location --fail --show-error \
      --retry 12 --retry-all-errors --retry-delay 3 --connect-timeout 30 \
      --speed-limit 1024 --speed-time 120 --continue-at - \
      --output "$part" "$url"; then
      if [ "$integrity_attempt" = 1 ] && [ -f "$part" ]; then
        printf '[qos] resume failed for %s; retrying once from byte zero\n' "$name" >&2
        unlink -- "$part"
        continue
      fi
      exit 1
    fi
    if verify "$expected" "$part"; then
      verified=1
      break
    fi
    actual="$(sha256sum "$part" | cut -d ' ' -f 1)"
    printf 'checksum mismatch for %s: expected %s, got %s\n' \
      "$name" "$expected" "$actual" >&2
    unlink -- "$part"
    if [ "$integrity_attempt" = 1 ]; then
      printf '[qos] retrying %s once from byte zero\n' "$name" >&2
    fi
  done
  [ "$verified" = 1 ] || exit 1
  mv -f -- "$part" "$target"
done < "$manifest"

# A prefix restore can contain entries removed by a newer manifest. Drop those
# before exposing the directory as a container build context, then save the
# reconciled directory under the new exact manifest key.
while IFS= read -r cached; do
  name="${cached##*/}"
  case "$name" in *.part) continue ;; esac
  awk -F '\t' -v n="$name" '$1 !~ /^#/ && $2 == n { found=1 } END { exit !found }' \
    "$manifest" || {
      printf '[qos] pruning stale cached asset %s\n' "$name"
      unlink -- "$cached"
    }
done < <(find "$cache_dir" -maxdepth 1 -type f -print)

# A named BuildKit/Podman context is read by a container process rather than
# copied by the frontend. Normalize permissions and, on enforcing SELinux
# hosts, its label so rootless builds can mount the verified files read-only.
chmod 0755 "$cache_dir"
find "$cache_dir" -maxdepth 1 -type f ! -name '*.part' -exec chmod 0644 {} +
if command -v selinuxenabled >/dev/null 2>&1 && selinuxenabled; then
  chcon -R -t container_file_t -- "$cache_dir"
fi
