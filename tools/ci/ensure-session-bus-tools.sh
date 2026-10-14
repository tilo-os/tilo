#!/usr/bin/env bash
# Supply the session-bus tools tests start themselves (a private dbus-daemon,
# and xdg-dbus-proxy for the launcher's bus filters), inside an ephemeral CI
# container whose golden-pinned builder predates them.
set -euo pipefail

missing=()
command -v dbus-daemon >/dev/null || missing+=(dbus-daemon)
command -v xdg-dbus-proxy >/dev/null || missing+=(xdg-dbus-proxy)
(( ${#missing[@]} )) || exit 0
if [[ ${GITHUB_ACTIONS:-} != true ]] || ! { test -f /.dockerenv || test -f /run/.containerenv; }; then
    echo 'session-bus tool installation is restricted to CI containers' >&2
    exit 1
fi

# Keep every installed package pinned: never update Mesa, fonts or libraries
# in golden-image jobs. A dependency requiring an upgrade fails the job.
installed=$(rpm -qa --qf '%{NAME}\n' | sort -u | paste -sd, -)
dnf install -y --setopt=install_weak_deps=False --setopt=obsoletes=False \
    --setopt=best=False --setopt=clean_requirements_on_remove=False \
    "--exclude=$installed" "${missing[@]}"
for tool in "${missing[@]}"; do
    command -v "$tool" >/dev/null || {
        echo "$tool installation did not supply the command" >&2
        exit 1
    }
done
