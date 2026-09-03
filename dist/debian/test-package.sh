#!/usr/bin/env bash
set -euo pipefail

[[ $# -ge 1 ]] || { echo "usage: $0 qos-session.deb [qos-session-devel.deb]" >&2; exit 2; }
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
packages=()
expect_devel=0
for package in "$@"; do
    packages+=("$(realpath "$package")")
    [[ $(basename "$package") != qos-session-devel_* ]] || expect_devel=1
done
apt-get update
apt-get install -y --no-install-recommends "${packages[@]}" file
"$repo_root/dist/verify-payload.sh" /
dpkg-query -W -f='${Status}\n' qos-session | grep -q 'install ok installed'
if (( expect_devel )); then
    "$repo_root/dist/verify-devel-payload.sh" /
    dpkg-query -W -f='${Status}\n' qos-session-devel | grep -q 'install ok installed'
fi
