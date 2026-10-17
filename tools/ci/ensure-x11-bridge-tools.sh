#!/usr/bin/env bash
# Supply the X11 bridge the shell's X11 clipboard and focus tests drive (a real
# xwayland-satellite and Xwayland behind the compositor's per-workspace
# bridge), inside an ephemeral CI container whose golden-pinned builder
# predates them or carries Fedora's satellite instead of the patched build
# (tools/ci/install-xwayland-satellite.sh).
# tools/ci/check-build-env.sh requires both of a builder.
set -euo pipefail

manifest=os/external-assets/github-assets.tsv
patches=os/external-assets/xwayland-satellite
installer=tools/ci/install-xwayland-satellite.sh
wanted=$(bash "$installer" --stamp "$manifest" "$patches")
installed=$(cat /usr/share/licenses/tilo-xwayland-satellite/SOURCE 2>/dev/null || true)
if [[ $installed == "$wanted" ]] && command -v xwayland-satellite >/dev/null && command -v Xwayland >/dev/null; then
    exit 0
fi
if [[ ${GITHUB_ACTIONS:-} != true ]] || ! { test -f /.dockerenv || test -f /run/.containerenv; }; then
    echo 'X11 bridge installation is restricted to CI containers' >&2
    exit 1
fi

# Keep every installed package pinned: never update Mesa, fonts or libraries
# in golden-image jobs. A dependency requiring an upgrade fails the job.
missing=()
for package in xorg-x11-server-Xwayland xcb-util-cursor-devel open-sans-fonts; do
    rpm -q "$package" >/dev/null 2>&1 || missing+=("$package")
done
if (( ${#missing[@]} )); then
    installed_packages=$(rpm -qa --qf '%{NAME}\n' | sort -u | paste -sd, -)
    dnf install -y --setopt=install_weak_deps=False --setopt=obsoletes=False \
        --setopt=best=False --setopt=clean_requirements_on_remove=False \
        "--exclude=$installed_packages" "${missing[@]}"
fi
# Fedora's build would otherwise stay registered for the replaced binary.
if rpm -q xwayland-satellite >/dev/null 2>&1; then
    rpm -e --nodeps xwayland-satellite
fi
asset=$(bash "$installer" --asset-name)
./os/dev/cache-github-assets.sh "$asset"
bash "$installer" os/external-assets/cache /usr "$manifest" "$patches"
for tool in xwayland-satellite Xwayland; do
    command -v "$tool" >/dev/null || {
        echo "$tool installation did not supply the command" >&2
        exit 1
    }
done
