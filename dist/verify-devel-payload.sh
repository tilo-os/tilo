#!/usr/bin/env bash
# Assertions shared by every optional developer-session subpackage.
set -euo pipefail

[[ $# == 1 ]] || { echo "usage: $0 ROOT" >&2; exit 2; }
root=${1%/}
[[ -n $root ]] || root=/
path() { printf '%s%s' "${root%/}" "$1"; }
libexec_dir=${TILO_LIBEXEC_DIR:-/usr/libexec}
[[ $libexec_dir == /* && $libexec_dir != */ ]] || {
    echo "TILO_LIBEXEC_DIR must be an absolute directory" >&2
    exit 2
}

wrapper=$(path "$libexec_dir/tilo-session-dev")
entry=$(path /usr/share/wayland-sessions/tilo-app.desktop)
[[ -x $wrapper ]] || { echo "missing developer session wrapper" >&2; exit 1; }
[[ -f $entry ]] || { echo "missing developer session entry" >&2; exit 1; }
grep -Fqx "Exec=$libexec_dir/tilo-session-dev" "$entry"
grep -Fqx "TryExec=$libexec_dir/tilo-session-dev" "$entry"
grep -Fq 'SESSION_RUNNER=$(dirname -- "$SELF_REAL")/tilo-session-runner' "$wrapper"
grep -Fq 'exec "$SESSION_RUNNER" "$TARGET_REAL" "$@"' "$wrapper"
