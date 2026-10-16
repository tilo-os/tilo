#!/usr/bin/env bash
# Print ARCH's package targets from dist/ci/package-targets.txt as
# DISTRO|IMAGE|DEBIAN_REVISION lines, optionally only DISTRO's.
#
# usage: package-targets.sh amd64|arm64 [DISTRO]
set -euo pipefail
arch=${1:?usage: package-targets.sh amd64|arm64 [DISTRO]}
only=${2:-}
case $arch in amd64|arm64) ;; *) echo "unknown architecture: $arch" >&2; exit 2 ;; esac
found=0
while IFS='|' read -r target_arch distro image revision; do
  [[ -n $target_arch && $target_arch != \#* ]] || continue
  [[ $target_arch == "$arch" || $target_arch == '*' ]] || continue
  [[ -z $only || $distro == "$only" ]] || continue
  printf '%s|%s|%s\n' "$distro" "$image" "$revision"
  found=1
done < "$(dirname "${BASH_SOURCE[0]}")/package-targets.txt"
[[ $found == 1 ]] || { echo "no package target for $arch${only:+ $only}" >&2; exit 1; }
