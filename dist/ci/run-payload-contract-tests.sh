#!/usr/bin/env bash
# Exercise format-independent installed-layout verifiers without compiling qOS.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
work=$(mktemp -d)
cleanup() { rm -rf -- "$work"; }
trap cleanup EXIT
true_bin=$(type -P true)
case $(uname -m) in
    x86_64) fixture_arch=amd64; payload_arch=x86_64 ;;
    aarch64) fixture_arch=arm64; payload_arch=aarch64 ;;
    *) echo "unsupported native fixture architecture" >&2; exit 2 ;;
esac

# Minimal distro images need not install Git merely to make the reusable OS
# payload reproducible. The outer runner resolves both source values and passes
# them through the packaging boundary.
package_job="$repo_root/dist/ci/run-package-job.sh"
grep -Fq -- '--env SOURCE_DATE_EPOCH="$source_epoch"' "$package_job"
grep -Fq -- '--env GITHUB_SHA="$source_revision"' "$package_job"
if sed -n '/^fedora-44)/,/^    ;;/p' "$package_job" | grep -Eq '\bgit[[:space:]]'; then
    echo "Fedora payload assembly still requires Git inside the target image" >&2
    exit 1
fi

"$repo_root/dist/session/tests/host-session-lifecycle.sh"
"$repo_root/dist/session/tests/private-dbus-handoff.sh"
"$repo_root/dist/session/tests/private-user-manager-handoff.sh"
"$repo_root/dist/rpm/test-signing-contract.sh"

# A production import entry must name an installed native executable. This
# fails before the import payload is added, and must remain in every builder.
for builder in dist/build-binaries.sh dist/rpm/build-rpm.sh dist/arch/PKGBUILD; do
    grep -Eq 'cargo build .*-p imager.*--bin imager' "$repo_root/$builder" || {
        echo "$builder does not build the delivered folder import UI" >&2
        exit 1
    }
done

# Host packages ship only the compositor from the shell crate. Keep every FHS
# builder explicit so adding another developer binary to that crate cannot
# multiply release build time and disk use again.
for builder in dist/build-binaries.sh dist/rpm/build-rpm.sh dist/arch/PKGBUILD; do
    grep -Eq 'cargo build .*(-p shell.*--bin shell|--bin shell.*-p shell)' \
        "$repo_root/$builder" || {
        echo "$builder builds every shell binary instead of only the packaged compositor" >&2
        exit 1
    }
done

# A local Atomic trial must install only the runtime RPM unless the developer
# entry is explicitly requested; a broad qos-session*.rpm glob regresses that
# contract by silently layering qos-session-devel too.
overlay_helper="$repo_root/dist/rpm/usroverlay-install.sh"
grep -Fq -- "-name 'qos-session-[0-9]*.rpm'" "$overlay_helper"
grep -Fq -- 'include_devel=1' "$overlay_helper"
if grep -Fq -- "-name 'qos-session*.rpm'" "$overlay_helper"; then
    echo "usroverlay helper still selects the optional developer RPM by default" >&2
    exit 1
fi

fhs=$work/fhs
mkdir -p "$fhs"/{usr/bin,usr/libexec,usr/lib/systemd/user,etc/pam.d,usr/share/wayland-sessions,usr/share/applications,usr/share/xdg-desktop-portal/portals,usr/share/licenses/qos-session}
for target in "$fhs/usr/bin/qos-shell" "$fhs/usr/bin/qos-launcher" "$fhs/usr/bin/qos-import-folders" \
              "$fhs/usr/libexec/qos-unlock-helper"; do
    install -m755 "$true_bin" "$target"
done
install -m644 "$repo_root/dist/session/qos-import-folders.desktop" "$fhs/usr/share/applications/qos-import-folders.desktop"
install -m755 "$repo_root/dist/session/qos-session" "$fhs/usr/libexec/qos-session"
install -m755 "$repo_root/dist/session/qos-session-dev" "$fhs/usr/libexec/qos-session-dev"
install -m644 "$repo_root/dist/session/qos.desktop" "$fhs/usr/share/wayland-sessions/qos.desktop"
install -m644 "$repo_root/dist/session/qos-dev.desktop" "$fhs/usr/share/wayland-sessions/qos-dev.desktop"
install -m644 "$repo_root/os/etc/pam.d/qos-unlock" "$fhs/etc/pam.d/qos-unlock"
install -m644 "$repo_root/os/usr/share/xdg-desktop-portal/portals/qos.portal" \
    "$fhs/usr/share/xdg-desktop-portal/portals/qos.portal"
install -m644 "$repo_root/os/usr/share/xdg-desktop-portal/nirios-portals.conf" \
    "$fhs/usr/share/xdg-desktop-portal/nirios-portals.conf"
install -m644 "$repo_root/os/usr/lib/systemd/user/qos-graphical-session.target" \
    "$fhs/usr/lib/systemd/user/qos-graphical-session.target"
install -m644 "$repo_root/LICENSE" "$fhs/usr/share/licenses/qos-session/LICENSE"
QOS_EXPECT_ARCH="$fixture_arch" "$repo_root/dist/verify-payload.sh" "$fhs"
"$repo_root/dist/verify-devel-payload.sh" "$fhs"
# A tool failure cannot masquerade as a complete runtime dependency closure.
mkdir "$work/fake-tools"
printf '#!/bin/sh\nexit 42\n' > "$work/fake-tools/ldd"
chmod 755 "$work/fake-tools/ldd"
if PATH="$work/fake-tools:$PATH" QOS_EXPECT_ARCH="$fixture_arch" "$repo_root/dist/verify-payload.sh" "$fhs" >/dev/null 2>&1; then
    echo "payload verifier accepted a failed ldd invocation" >&2; exit 1
fi
printf '#!/bin/sh\necho "libmissing.so => not found"\n' > "$work/fake-tools/ldd"
if PATH="$work/fake-tools:$PATH" QOS_EXPECT_ARCH="$fixture_arch" "$repo_root/dist/verify-payload.sh" "$fhs" >/dev/null 2>&1; then
    echo "payload verifier accepted a missing shared library" >&2; exit 1
fi
mv "$fhs/usr/bin/qos-import-folders" "$work/import-held"
if QOS_EXPECT_ARCH="$fixture_arch" "$repo_root/dist/verify-payload.sh" "$fhs" >/dev/null 2>&1; then
    echo "payload verifier accepted missing import executable" >&2; exit 1
fi
mv "$work/import-held" "$fhs/usr/bin/qos-import-folders"
sed -i 's#^Exec=.*#Exec=/missing --import-folders#' "$fhs/usr/share/applications/qos-import-folders.desktop"
if QOS_EXPECT_ARCH="$fixture_arch" "$repo_root/dist/verify-payload.sh" "$fhs" >/dev/null 2>&1; then
    echo "payload verifier accepted wrong import entry" >&2; exit 1
fi
install -m644 "$repo_root/dist/session/qos-import-folders.desktop" "$fhs/usr/share/applications/qos-import-folders.desktop"

sed -i 's#^TryExec=.*#TryExec=/wrong#' "$fhs/usr/share/wayland-sessions/qos-dev.desktop"
if "$repo_root/dist/verify-devel-payload.sh" "$fhs" >/dev/null 2>&1; then
    echo "developer payload verifier accepted a wrong TryExec" >&2
    exit 1
fi

nix=$work/nix-result
mkdir -p "$nix"/{bin,libexec,etc/pam.d,share/systemd/user,share/wayland-sessions,share/applications,share/xdg-desktop-portal/portals,share/licenses/qos-session}
for target in "$nix/bin/qos-shell" "$nix/bin/qos-launcher" "$nix/bin/qos-import-folders" \
              "$nix/libexec/qos-unlock-helper"; do
    install -m755 "$true_bin" "$target"
done
sed "s#/usr/bin/qos-import-folders#$nix/bin/qos-import-folders#g" "$repo_root/dist/session/qos-import-folders.desktop" > "$nix/share/applications/qos-import-folders.desktop"
install -m755 "$repo_root/dist/session/qos-session" "$nix/libexec/qos-session"
install -m644 "$repo_root/dist/session/qos.desktop" "$nix/share/wayland-sessions/qos.desktop"
install -m644 "$repo_root/os/etc/pam.d/qos-unlock" "$nix/etc/pam.d/qos-unlock"
install -m644 "$repo_root/os/usr/share/xdg-desktop-portal/portals/qos.portal" \
    "$nix/share/xdg-desktop-portal/portals/qos.portal"
install -m644 "$repo_root/os/usr/share/xdg-desktop-portal/nirios-portals.conf" \
    "$nix/share/xdg-desktop-portal/nirios-portals.conf"
install -m644 "$repo_root/os/usr/lib/systemd/user/qos-graphical-session.target" \
    "$nix/share/systemd/user/qos-graphical-session.target"
install -m644 "$repo_root/LICENSE" "$nix/share/licenses/qos-session/LICENSE"
sed -i \
    -e "s#QOS_SHELL=/usr/bin/qos-shell#QOS_SHELL=$nix/bin/qos-shell#" \
    -e 's#SYSTEMCTL=/usr/bin/systemctl#SYSTEMCTL=/nix/store/fixture-systemd/bin/systemctl#' \
    -e 's#SYSTEMD_CAT=/usr/bin/systemd-cat#SYSTEMD_CAT=/nix/store/fixture-systemd/bin/systemd-cat#' \
    -e 's#DBUS_UPDATE=/usr/bin/dbus-update-activation-environment#DBUS_UPDATE=/nix/store/fixture-dbus/bin/dbus-update-activation-environment#' \
    "$nix/libexec/qos-session"
sed -i \
    -e "s#Exec=/usr/libexec/qos-session#Exec=$nix/libexec/qos-session#" \
    -e "s#TryExec=/usr/bin/qos-shell#TryExec=$nix/bin/qos-shell#" \
    "$nix/share/wayland-sessions/qos.desktop"
"$repo_root/dist/ci/verify-nix-result.sh" "$nix" "$fixture_arch"

# The OS hand-off is deterministic and contains only the three native binaries
# already install-tested by the distro package lane.
payload_bin=$work/payload-bin
mkdir -p "$payload_bin" "$work/payload-a" "$work/payload-b"
for binary in shell qos-launcher qos-unlock-helper; do
    install -m755 "$true_bin" "$payload_bin/$binary"
done
SOURCE_DATE_EPOCH=123 GITHUB_SHA="$(printf 'a%.0s' {1..40})" \
    "$repo_root/dist/desktop/package-os-payload.sh" \
    "$payload_bin" "$payload_arch" "$work/payload-a"
SOURCE_DATE_EPOCH=123 GITHUB_SHA="$(printf 'a%.0s' {1..40})" \
    "$repo_root/dist/desktop/package-os-payload.sh" \
    "$payload_bin" "$payload_arch" "$work/payload-b"
cmp "$work/payload-a/qos-desktop-os-$payload_arch.tar.gz" \
    "$work/payload-b/qos-desktop-os-$payload_arch.tar.gz"
mkdir "$work/payload-unpacked"
tar -xzf "$work/payload-a/qos-desktop-os-$payload_arch.tar.gz" \
    -C "$work/payload-unpacked"
(cd "$work/payload-unpacked" && sha256sum --check --strict SHA256SUMS)

echo "payload contract fixtures passed"
