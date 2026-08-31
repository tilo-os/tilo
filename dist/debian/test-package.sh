#!/usr/bin/env bash
set -euo pipefail

[[ $# -ge 1 ]] || { echo "usage: $0 qos-session.deb [qos-session-devel.deb]" >&2; exit 2; }
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
packages=()
for package in "$@"; do packages+=("$(realpath "$package")"); done
apt-get update
apt-get install -y --no-install-recommends "${packages[@]}" file
"$repo_root/dist/verify-payload.sh" /
dpkg-query -W -f='${Status}\n' qos-session | grep -q 'install ok installed'
