#!/usr/bin/env bash
# Supply the X11 bridge the shell's X11 clipboard and focus tests drive (a real
# xwayland-satellite and Xwayland behind the compositor's per-workspace
# bridge), inside an ephemeral CI container whose golden-pinned builder
# predates them. tools/ci/check-build-env.sh requires both of a builder.
set -euo pipefail

missing=()
command -v xwayland-satellite >/dev/null || missing+=(xwayland-satellite)
command -v Xwayland >/dev/null || missing+=(xorg-x11-server-Xwayland)
(( ${#missing[@]} )) || exit 0
if [[ ${GITHUB_ACTIONS:-} != true ]] || ! { test -f /.dockerenv || test -f /run/.containerenv; }; then
    echo 'X11 bridge installation is restricted to CI containers' >&2
    exit 1
fi

# Keep every installed package pinned: never update Mesa, fonts or libraries
# in golden-image jobs. A dependency requiring an upgrade fails the job.
installed=$(rpm -qa --qf '%{NAME}\n' | sort -u | paste -sd, -)
dnf install -y --setopt=install_weak_deps=False --setopt=obsoletes=False \
    --setopt=best=False --setopt=clean_requirements_on_remove=False \
    "--exclude=$installed" "${missing[@]}"
for tool in xwayland-satellite Xwayland; do
    command -v "$tool" >/dev/null || {
        echo "$tool installation did not supply the command" >&2
        exit 1
    }
done
