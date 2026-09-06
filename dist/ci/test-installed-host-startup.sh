#!/usr/bin/env bash
# Prove the installed compositor reaches and paints the host-desktop first-login
# screen as an ordinary user. This deliberately uses the display-free backend:
# package CI must exercise the production startup path without owning a CI
# runner's seat or relying on a nested display server.
set -euo pipefail

[[ $# == 0 ]] || { echo "usage: $0" >&2; exit 2; }
[[ $(id -u) == 0 ]] || { echo "installed host startup test must run as root" >&2; exit 2; }
test -x /usr/bin/qos-shell
command -v useradd >/dev/null
command -v runuser >/dev/null

work=$(mktemp -d)
smoke_user=qosci$$
cleanup() {
    userdel "$smoke_user" >/dev/null 2>&1 || true
    rm -rf -- "$work"
}
trap cleanup EXIT
fail() {
    echo "installed qOS host startup check failed: $1" >&2
    echo '--- compositor log ---' >&2
    cat "$work/startup.log" >&2 || true
    echo '--- frame summary ---' >&2
    cat "$work/state/frame-summary.json" >&2 || true
    exit 1
}

# A real passwd entry catches HOME/uid assumptions which running the package as
# root would hide. Keep every user-owned path under the throwaway tree so this
# cannot consume or create a developer's actual qOS identity.
useradd --home-dir "$work/home" --create-home --shell /bin/bash "$smoke_user"
smoke_uid=$(id -u "$smoke_user")
smoke_gid=$(id -g "$smoke_user")
chmod 0711 "$work"
install -d -m 0700 -o "$smoke_uid" -g "$smoke_gid" \
    "$work/runtime" "$work/config" "$work/data" "$work/state" "$work/cache"

set +e
runuser -u "$smoke_user" -- env -i \
    HOME="$work/home" \
    USER="$smoke_user" \
    LOGNAME="$smoke_user" \
    PATH=/usr/local/bin:/usr/bin:/bin \
    XDG_RUNTIME_DIR="$work/runtime" \
    XDG_CONFIG_HOME="$work/config" \
    XDG_DATA_HOME="$work/data" \
    XDG_STATE_HOME="$work/state" \
    XDG_CACHE_HOME="$work/cache" \
    WGPU_BACKEND=vulkan \
    QOS_RUN_SECONDS=3 \
    QOS_FRAME_WARMUP_MS=0 \
    QOS_FRAME_SUMMARY="$work/state/frame-summary.json" \
    /usr/bin/qos-shell --host-desktop-session --firstboot --backend headless \
    >"$work/startup.log" 2>&1
status=$?
set -e

if (( status != 0 )); then
    echo "installed qOS host session exited with status $status" >&2
    cat "$work/startup.log" >&2
    exit "$status"
fi
grep -Fq \
    'nirios compositor: showing host-desktop identity setup from the first frame' \
    "$work/startup.log" || fail 'first-login selection was not logged'
test -s "$work/state/frame-summary.json" || fail 'frame summary was not written'
grep -Eq '"painted"[[:space:]]*:[[:space:]]*[1-9][0-9]*' \
    "$work/state/frame-summary.json" || fail 'the compositor did not paint a frame'
test -d "$work/data/qos" || fail 'the host identity root was not prepared'
test ! -e "$work/data/qos/user-identity/primary.json" \
    || fail 'startup minted an identity without user consent'

echo 'installed host first-login startup: PASS'
