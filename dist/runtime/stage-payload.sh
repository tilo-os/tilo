#!/usr/bin/env bash
# Assemble the distro-neutral tilo-runtime payload from native release binaries.
#
# The runtime, the launcher it confines apps with and the Tilo launcher app
# share one private directory: the runtime finds tilo-launcher beside itself,
# and the app finds tilo-runtime on PATH. tilo-session owns /usr/bin/tilo-launcher,
# so nothing here collides with it.
set -euo pipefail

[[ $# == 2 ]] || { echo "usage: $0 BIN_DIR STAGE_DIR" >&2; exit 2; }
bin_dir=$1
stage=$2
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

[[ ! -e $stage ]] || { echo "stage path already exists: $stage" >&2; exit 1; }
for bin in tilo-runtime tilo-launcher store; do
    [[ -x $bin_dir/$bin ]] || { echo "missing executable: $bin_dir/$bin" >&2; exit 1; }
done

mkdir -p "$stage"/{libexec,systemd-user,applications,icons,apparmor,license}
for bin in tilo-runtime tilo-launcher store; do
    install -pm 0755 "$bin_dir/$bin" "$stage/libexec/$bin"
done
install -pm 0644 "$repo_root/dist/runtime/tilo-runtime.service" "$stage/systemd-user/"
install -pm 0644 "$repo_root/dist/runtime/so.tilo.store.desktop" "$stage/applications/"
install -pm 0644 "$repo_root/apps/imager/packaging/linux/tilo-imager.svg" \
    "$stage/icons/so.tilo.store.svg"
install -pm 0644 "$repo_root/dist/runtime/apparmor/tilo-runtime-launcher" "$stage/apparmor/"
install -pm 0644 "$repo_root/LICENSE" "$stage/license/LICENSE"
