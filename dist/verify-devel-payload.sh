#!/usr/bin/env bash
# Assertions shared by every optional developer-session subpackage.
set -euo pipefail

[[ $# == 1 ]] || { echo "usage: $0 ROOT" >&2; exit 2; }
root=${1%/}
[[ -n $root ]] || root=/
path() { printf '%s%s' "${root%/}" "$1"; }

wrapper=$(path /usr/libexec/qos-session-dev)
entry=$(path /usr/share/wayland-sessions/qos-dev.desktop)
[[ -x $wrapper ]] || { echo "missing developer session wrapper" >&2; exit 1; }
[[ -f $entry ]] || { echo "missing developer session entry" >&2; exit 1; }
grep -q '^Exec=/usr/libexec/qos-session-dev$' "$entry"
grep -q '^TryExec=/usr/bin/qos-shell$' "$entry"
grep -q -- '--firstboot' "$wrapper"
