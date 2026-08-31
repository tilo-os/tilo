#!/usr/bin/env bash
set -euo pipefail
[[ $# -ge 1 ]] || { echo "usage: $0 qos-session.pkg.tar.* [devel package]" >&2; exit 2; }
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
pacman -Syu --noconfirm file
pacman -U --noconfirm "$@"
"$repo_root/dist/verify-payload.sh" /
pacman -Q qos-session >/dev/null
