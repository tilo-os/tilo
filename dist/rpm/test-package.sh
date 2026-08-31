#!/usr/bin/env bash
set -euo pipefail
[[ $# -ge 1 ]] || { echo "usage: $0 qos-session.rpm [qos-session-devel.rpm]" >&2; exit 2; }
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
dnf install -y "$@" file
"$repo_root/dist/verify-payload.sh" /
rpm -q qos-session >/dev/null
