#!/usr/bin/env bash
# Assemble the distro-neutral filesystem payload from native release binaries.
set -euo pipefail

usage() {
    echo "usage: $0 BIN_DIR STAGE_DIR [--primary-files]" >&2
    exit 2
}

[[ $# == 2 || $# == 3 ]] || usage
primary_files=0
if [[ $# == 3 ]]; then
    [[ $3 == --primary-files ]] || usage
    primary_files=1
fi
bin_dir=$1
stage=$2
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

[[ ! -e $stage ]] || { echo "stage path already exists: $stage" >&2; exit 1; }
bins=(shell tilo-launcher tilo-unlock-helper tilo-flight-recorder tilo-session-report imager)
(( ! primary_files )) || bins+=(fs tilo-relay-admin tilo-enroll)
for bin in "${bins[@]}"; do
    [[ -x $bin_dir/$bin ]] || { echo "missing executable: $bin_dir/$bin" >&2; exit 1; }
done

marker=tilo-app-session-build-marker-v1
if LC_ALL=C grep -q -a -F -e "$marker" "$bin_dir/shell"; then
    echo "refusing to package a shell carrying the developer-session marker" >&2
    exit 1
fi

mkdir -p "$stage"/{bin,libexec,pam,wayland-sessions,applications,portal,pipewire,systemd-user,license}
install -pm 0755 "$bin_dir/shell"             "$stage/bin/tilo-shell"
if (( primary_files )); then
    install -pm 0755 "$bin_dir/fs" "$stage/bin/fs"
    install -pm 0755 "$bin_dir/tilo-relay-admin" "$stage/bin/tilo-relay-admin"
    install -pm 0755 "$bin_dir/tilo-enroll" "$stage/bin/tilo-enroll"
    install -pm 0644 "$repo_root/dist/session/tilo-files-workspace.service" "$stage/systemd-user/tilo-files-workspace.service"
fi
install -pm 0755 "$bin_dir/tilo-launcher"      "$stage/bin/tilo-launcher"
install -pm 0755 "$bin_dir/tilo-unlock-helper" "$stage/libexec/tilo-unlock-helper"
install -pm 0755 "$repo_root/dist/session/tilo-session"     "$stage/libexec/tilo-session"
install -pm 0755 "$repo_root/dist/session/tilo-session-dev" "$stage/libexec/tilo-session-dev"
install -pm 0755 "$repo_root/dist/session/tilo-session-runner" "$stage/libexec/tilo-session-runner"
install -pm 0755 "$bin_dir/tilo-flight-recorder" "$stage/libexec/tilo-flight-recorder"
install -pm 0755 "$bin_dir/tilo-session-report" "$stage/libexec/tilo-session-report"
install -pm 0644 "$repo_root/dist/session/tilo.desktop"     "$stage/wayland-sessions/tilo.desktop"
install -pm 0644 "$repo_root/dist/session/tilo-app.desktop" "$stage/wayland-sessions/tilo-app.desktop"
install -pm 0644 "$repo_root/dist/session/tilo-unlock.pam"   "$stage/pam/tilo-unlock"
install -pm 0644 "$repo_root/os/usr/lib/systemd/user/tilo-graphical-session.target" \
                 "$stage/systemd-user/tilo-graphical-session.target"
install -pm 0755 "$bin_dir/imager" "$stage/bin/tilo-import-folders"
install -pm 0644 "$repo_root/dist/session/tilo-import-folders.desktop" "$stage/applications/tilo-import-folders.desktop"

portal_src=$repo_root/os/usr/share/xdg-desktop-portal/portals/tilo.portal
install -pm 0644 "$portal_src" "$stage/portal/tilo.portal"
install -pm 0644 "$repo_root/os/usr/share/xdg-desktop-portal/tilo-portals.conf" \
                 "$stage/portal/tilo-portals.conf"
# The PipeWire client config Settings' "Reduce echo in calls" starts; the
# session offers the toggle only where this file is in PipeWire's search path.
install -pm 0644 "$repo_root/os/usr/share/pipewire/tilo-echo-cancel.conf" \
                 "$stage/pipewire/tilo-echo-cancel.conf"
install -pm 0644 "$repo_root/LICENSE" "$stage/license/LICENSE"

# Host only: the OS image retains its existing system fs.service.
if (( primary_files )); then
    printf '\n[Unit]\nWants=tilo-files-workspace.service\n' >> "$stage/systemd-user/tilo-graphical-session.target"
fi
