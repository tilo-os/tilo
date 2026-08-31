#!/usr/bin/env bash
# Assemble the distro-neutral filesystem payload from native release binaries.
set -euo pipefail

usage() {
    echo "usage: $0 BIN_DIR STAGE_DIR" >&2
    exit 2
}

[[ $# == 2 ]] || usage
bin_dir=$1
stage=$2
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

[[ ! -e $stage ]] || { echo "stage path already exists: $stage" >&2; exit 1; }
for bin in shell qos-launcher qos-unlock-helper; do
    [[ -x $bin_dir/$bin ]] || { echo "missing executable: $bin_dir/$bin" >&2; exit 1; }
done

marker=qos-dev-session-build-marker-v1
if LC_ALL=C grep -q -a -F -e "$marker" "$bin_dir/shell"; then
    echo "refusing to package a shell carrying the developer-session marker" >&2
    exit 1
fi

mkdir -p "$stage"/{bin,libexec,pam,wayland-sessions,portal}
install -pm 0755 "$bin_dir/shell"             "$stage/bin/qos-shell"
install -pm 0755 "$bin_dir/qos-launcher"      "$stage/bin/qos-launcher"
install -pm 0755 "$bin_dir/qos-unlock-helper" "$stage/libexec/qos-unlock-helper"
install -pm 0755 "$repo_root/dist/session/qos-session"     "$stage/libexec/qos-session"
install -pm 0755 "$repo_root/dist/session/qos-session-dev" "$stage/libexec/qos-session-dev"
install -pm 0644 "$repo_root/dist/session/qos.desktop"     "$stage/wayland-sessions/qos.desktop"
install -pm 0644 "$repo_root/dist/session/qos-dev.desktop" "$stage/wayland-sessions/qos-dev.desktop"
install -pm 0644 "$repo_root/os/etc/pam.d/qos-unlock"      "$stage/pam/qos-unlock"

portal_src=$repo_root/os/usr/share/xdg-desktop-portal/portals/qos.portal
install -pm 0644 "$portal_src" "$stage/portal/qos.portal"
