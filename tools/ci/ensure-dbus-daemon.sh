#!/usr/bin/env bash
# Supply dbus-daemon for tests that start a private session bus, inside an
# ephemeral CI container whose golden-pinned builder predates it.
set -euo pipefail

if command -v dbus-daemon >/dev/null; then exit 0; fi
if [[ ${GITHUB_ACTIONS:-} != true ]] || ! { test -f /.dockerenv || test -f /run/.containerenv; }; then
    echo 'dbus-daemon installation is restricted to CI containers' >&2
    exit 1
fi

# Keep every installed package pinned: never update Mesa, fonts or libraries
# in golden-image jobs. A dependency requiring an upgrade fails the job.
installed=$(rpm -qa --qf '%{NAME}\n' | sort -u | paste -sd, -)
dnf install -y --setopt=install_weak_deps=False --setopt=obsoletes=False \
    --setopt=best=False --setopt=clean_requirements_on_remove=False \
    "--exclude=$installed" dbus-daemon
command -v dbus-daemon >/dev/null || {
    echo 'dbus-daemon installation did not supply the daemon' >&2
    exit 1
}
