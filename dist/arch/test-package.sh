#!/usr/bin/env bash
set -euo pipefail
[[ $# -ge 1 ]] || { echo "usage: $0 qos-session.pkg.tar.* [devel package]" >&2; exit 2; }
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
expect_devel=0
for package in "$@"; do
    [[ $(basename "$package") != qos-session-devel-* ]] || expect_devel=1
done
# The ARM CI userspace carries host-seeded sync databases because pacman 7's
# atomic database refresh disappears under qemu/PRoot. They are still current
# from the build-dependency phase, so upgrade/install without replacing them.
if [[ $(uname -m) == aarch64 ]]; then
    pacman -Su --noconfirm file
else
    pacman -Syu --noconfirm file
fi
pacman -U --noconfirm "$@"
QOS_LIBEXEC_DIR=/usr/lib/qos "$repo_root/dist/verify-payload.sh" /
pacman -Q qos-session >/dev/null
if (( expect_devel )); then
    QOS_LIBEXEC_DIR=/usr/lib/qos "$repo_root/dist/verify-devel-payload.sh" /
    pacman -Q qos-session-devel >/dev/null
fi
