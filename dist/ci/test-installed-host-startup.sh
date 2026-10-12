#!/usr/bin/env bash
# Prove the installed compositor reaches and paints the host-desktop first-login
# screen as an ordinary user. This deliberately uses the display-free backend:
# package CI must exercise the production startup path without owning a CI
# runner's seat or relying on a nested display server.
set -euo pipefail

# TILO_SHELL_BIN points the check at a workspace build instead of the
# installed package, e.g. inside a throwaway container:
#   podman run --rm -v "$PWD:/src:ro" -e TILO_SHELL_BIN=/src/target/release/shell \
#       <image with the session's runtime libraries> /src/dist/ci/test-installed-host-startup.sh
shell_bin=${TILO_SHELL_BIN:-/usr/bin/tilo-shell}

[[ $# == 0 ]] || { echo "usage: $0" >&2; exit 2; }
[[ $(id -u) == 0 ]] || { echo "installed host startup test must run as root" >&2; exit 2; }
test -x "$shell_bin"
command -v useradd >/dev/null
command -v runuser >/dev/null

work=$(mktemp -d)
smoke_user=tiloci$$
cleanup() {
    userdel "$smoke_user" >/dev/null 2>&1 || true
    rm -rf -- "$work"
}
trap cleanup EXIT
fail() {
    echo "installed Tilo host startup check failed: $1" >&2
    echo '--- compositor log ---' >&2
    cat "$work/startup.log" >&2 || true
    echo '--- frame summary ---' >&2
    cat "$work/state/frame-summary.json" >&2 || true
    exit 1
}

# A real passwd entry catches HOME/uid assumptions which running the package as
# root would hide. Keep every user-owned path under the throwaway tree so this
# cannot consume or create a developer's actual Tilo identity.
useradd --home-dir "$work/home" --create-home --shell /bin/bash "$smoke_user"
smoke_uid=$(id -u "$smoke_user")
smoke_gid=$(id -g "$smoke_user")
# The tree stands in for / and /var above a real /var/home: world-readable,
# as identity storage opens every ancestor directory for reading.
chmod 0755 "$work"
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
    TILO_RUN_SECONDS=3 \
    TILO_FRAME_WARMUP_MS=0 \
    TILO_FRAME_SUMMARY="$work/state/frame-summary.json" \
    "$shell_bin" --host-desktop-session --firstboot --backend headless \
    >"$work/startup.log" 2>&1
status=$?
set -e

if (( status != 0 )); then
    echo "installed Tilo host session exited with status $status" >&2
    cat "$work/startup.log" >&2
    exit "$status"
fi
grep -Fq \
    'tilo compositor: showing host-desktop identity setup from the first frame' \
    "$work/startup.log" || fail 'first-login selection was not logged'
test -s "$work/state/frame-summary.json" || fail 'frame summary was not written'
grep -Eq '"painted"[[:space:]]*:[[:space:]]*[1-9][0-9]*' \
    "$work/state/frame-summary.json" || fail 'the compositor did not paint a frame'
test -d "$work/data/tilo" || fail 'the host identity root was not prepared'
test ! -e "$work/data/tilo/user-identity/primary.json" \
    || fail 'startup minted an identity without user consent'

# Rendering the ceremony is not proof that its central operation works. Run
# the production provisioning entrypoint as the same throwaway account, using
# a Fedora Atomic-style /home symlink, then verify and repeat it. This catches
# filesystem-policy failures after the personal record is written (the former
# /home -> /var/home ELOOP did exactly that) without bypassing user consent in
# a real login or depending on screenshot-state injection.
real_home="$work/var/home/$smoke_user"
mkdir -p "$work/var/home"
mv "$work/home" "$real_home"
ln -s "$work/var/home" "$work/home-link"
host_home="$work/home-link/$smoke_user"
install -d -m 0700 -o "$smoke_uid" -g "$smoke_gid" \
    "$host_home/.config" "$host_home/.local/share"

# Every host-session invocation runs inside a login, which always has a
# runtime directory; the session resolves its Files sockets there before it
# touches the identity, and refuses to start without one.
run_identity() {
    runuser -u "$smoke_user" -- env -i \
        HOME="$host_home" \
        USER="$smoke_user" \
        LOGNAME="$smoke_user" \
        PATH=/usr/local/bin:/usr/bin:/bin \
        XDG_RUNTIME_DIR="$work/runtime" \
        XDG_CONFIG_HOME="$host_home/.config" \
        XDG_DATA_HOME="$host_home/.local/share" \
        TILO_IDENTITY_ROOT="$host_home/.local/share/tilo" \
        "$shell_bin" "$@"
}

run_identity --host-desktop-session --provision-identity \
    >"$work/provision.log" 2>&1 || {
        cat "$work/provision.log" >&2
        fail 'the installed binary could not create a host identity'
    }
run_identity --host-desktop-session --verify-collaboration-identity \
    >"$work/verify.log" 2>&1 || {
        cat "$work/verify.log" >&2
        fail 'the installed binary could not verify its new host identity'
    }
primary="$host_home/.config/tilo/identity/primary.json"
device_key="$host_home/.local/share/tilo/device-private/keypair.bin"
device_cert="$host_home/.local/share/tilo/device/cert.bin"
test -s "$primary" || fail 'host provisioning omitted the personal identity'
test -s "$device_key" || fail 'host provisioning omitted the device key'
test -s "$device_cert" || fail 'host provisioning omitted the device certificate'
before=$(sha256sum "$primary" "$device_key" "$device_cert")
run_identity --host-desktop-session --provision-identity \
    >"$work/reprovision.log" 2>&1 || fail 'idempotent host reprovision failed'
after=$(sha256sum "$primary" "$device_key" "$device_cert")
[[ $after == "$before" ]] || fail 'reprovision changed the established identity chain'

echo 'installed host first-login startup and identity lifecycle: PASS'
