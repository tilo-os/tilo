#!/usr/bin/env bash
# Exercise format-independent installed-layout verifiers without compiling Tilo.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
work=$(mktemp -d)
cleanup() { rm -rf -- "$work"; }
trap cleanup EXIT
true_bin=$(type -P true)

# Minimal distro images need not install Git merely to make the reusable OS
# payload reproducible. The outer runner resolves both source values and passes
# them through the packaging boundary.
package_job="$repo_root/dist/ci/run-package-job.sh"
grep -Fq -- '--env SOURCE_DATE_EPOCH="$source_epoch"' "$package_job"
grep -Fq -- '--env GITHUB_SHA="$source_revision"' "$package_job"
test "$(grep -Fc 'source tools/ci-sccache.sh prepare-shell' "$package_job")" -eq 3
if grep -Fq 'export RUSTC_WRAPPER=/usr/local/bin/sccache' "$package_job"; then
    echo "package lanes bypass the shared fail-open compiler-cache probe" >&2
    exit 1
fi
if sed -n '/^fedora-44)/,/^    ;;/p' "$package_job" | grep -Eq '\bgit[[:space:]]'; then
    echo "Fedora payload assembly still requires Git inside the target image" >&2
    exit 1
fi

"$repo_root/dist/session/tests/host-session-lifecycle.sh"
"$repo_root/dist/session/tests/private-dbus-handoff.sh"
"$repo_root/dist/session/tests/private-user-manager-handoff.sh"
"$repo_root/dist/rpm/test-signing-contract.sh"
"$repo_root/dist/rpm/test-app-store-stage.sh"
"$repo_root/dist/rpm/test-build-target-dir.sh"

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
# entry is explicitly requested; a broad tilo-session*.rpm glob regresses that
# contract by silently layering tilo-session-devel too.
overlay_helper="$repo_root/dist/rpm/usroverlay-install.sh"
grep -Fq -- "-name 'tilo-session-[0-9]*.rpm'" "$overlay_helper"
grep -Fq -- 'include_devel=1' "$overlay_helper"
if grep -Fq -- "-name 'tilo-session*.rpm'" "$overlay_helper"; then
    echo "usroverlay helper still selects the optional developer RPM by default" >&2
    exit 1
fi

fhs=$work/fhs
mkdir -p "$fhs"/{usr/bin,usr/libexec,usr/lib/systemd/user,etc/pam.d,usr/share/wayland-sessions,usr/share/xdg-desktop-portal/portals,usr/share/licenses/tilo-session}
for target in "$fhs/usr/bin/tilo-shell" "$fhs/usr/bin/tilo-launcher" \
              "$fhs/usr/libexec/tilo-unlock-helper"; do
    install -m755 "$true_bin" "$target"
done
install -m755 "$repo_root/dist/session/tilo-session" "$fhs/usr/libexec/tilo-session"
install -m755 "$repo_root/dist/session/tilo-session-runner" "$fhs/usr/libexec/tilo-session-runner"
install -m755 "$repo_root/dist/session/tilo-session-dev" "$fhs/usr/libexec/tilo-session-dev"
install -m644 "$repo_root/dist/session/tilo.desktop" "$fhs/usr/share/wayland-sessions/tilo.desktop"
install -m644 "$repo_root/dist/session/tilo-dev.desktop" "$fhs/usr/share/wayland-sessions/tilo-dev.desktop"
install -m644 "$repo_root/os/etc/pam.d/tilo-unlock" "$fhs/etc/pam.d/tilo-unlock"
install -m644 "$repo_root/os/usr/share/xdg-desktop-portal/portals/tilo.portal" \
    "$fhs/usr/share/xdg-desktop-portal/portals/tilo.portal"
install -m644 "$repo_root/os/usr/share/xdg-desktop-portal/tilo-portals.conf" \
    "$fhs/usr/share/xdg-desktop-portal/tilo-portals.conf"
install -m644 "$repo_root/os/usr/lib/systemd/user/tilo-graphical-session.target" \
    "$fhs/usr/lib/systemd/user/tilo-graphical-session.target"
install -m644 "$repo_root/LICENSE" "$fhs/usr/share/licenses/tilo-session/LICENSE"
TILO_EXPECT_ARCH=amd64 "$repo_root/dist/verify-payload.sh" "$fhs"
"$repo_root/dist/verify-devel-payload.sh" "$fhs"

sed -i 's#^TryExec=.*#TryExec=/wrong#' "$fhs/usr/share/wayland-sessions/tilo-dev.desktop"
if "$repo_root/dist/verify-devel-payload.sh" "$fhs" >/dev/null 2>&1; then
    echo "developer payload verifier accepted a wrong TryExec" >&2
    exit 1
fi

nix=$work/nix-result
mkdir -p "$nix"/{bin,libexec,etc/pam.d,share/systemd/user,share/wayland-sessions,share/xdg-desktop-portal/portals,share/licenses/tilo-session}
for target in "$nix/bin/tilo-shell" "$nix/bin/tilo-launcher" \
              "$nix/libexec/tilo-unlock-helper"; do
    install -m755 "$true_bin" "$target"
done
install -m755 "$repo_root/dist/session/tilo-session" "$nix/libexec/tilo-session"
install -m755 "$repo_root/dist/session/tilo-session-runner" "$nix/libexec/tilo-session-runner"
install -m644 "$repo_root/dist/session/tilo.desktop" "$nix/share/wayland-sessions/tilo.desktop"
install -m644 "$repo_root/os/etc/pam.d/tilo-unlock" "$nix/etc/pam.d/tilo-unlock"
install -m644 "$repo_root/os/usr/share/xdg-desktop-portal/portals/tilo.portal" \
    "$nix/share/xdg-desktop-portal/portals/tilo.portal"
install -m644 "$repo_root/os/usr/share/xdg-desktop-portal/tilo-portals.conf" \
    "$nix/share/xdg-desktop-portal/tilo-portals.conf"
install -m644 "$repo_root/os/usr/lib/systemd/user/tilo-graphical-session.target" \
    "$nix/share/systemd/user/tilo-graphical-session.target"
install -m644 "$repo_root/LICENSE" "$nix/share/licenses/tilo-session/LICENSE"
sed -i \
    -e "s#TILO_SHELL=/usr/bin/tilo-shell#TILO_SHELL=$nix/bin/tilo-shell#" \
    -e "s#SESSION_RUNNER=/usr/libexec/tilo-session-runner#SESSION_RUNNER=$nix/libexec/tilo-session-runner#" \
    "$nix/libexec/tilo-session"
sed -i \
    -e 's#SYSTEMCTL=/usr/bin/systemctl#SYSTEMCTL=/nix/store/fixture-systemd/bin/systemctl#' \
    -e 's#SYSTEMD_CAT=/usr/bin/systemd-cat#SYSTEMD_CAT=/nix/store/fixture-systemd/bin/systemd-cat#' \
    -e 's#DBUS_UPDATE=/usr/bin/dbus-update-activation-environment#DBUS_UPDATE=/nix/store/fixture-dbus/bin/dbus-update-activation-environment#' \
    "$nix/libexec/tilo-session-runner"
sed -i \
    -e "s#Exec=/usr/libexec/tilo-session#Exec=$nix/libexec/tilo-session#" \
    -e "s#TryExec=/usr/bin/tilo-shell#TryExec=$nix/bin/tilo-shell#" \
    "$nix/share/wayland-sessions/tilo.desktop"
"$repo_root/dist/ci/verify-nix-result.sh" "$nix" amd64

# The OS hand-off is deterministic and contains only the three native binaries
# already install-tested by the distro package lane.
payload_bin=$work/payload-bin
mkdir -p "$payload_bin" "$work/payload-a" "$work/payload-b"
for binary in shell tilo-launcher tilo-unlock-helper; do
    install -m755 "$true_bin" "$payload_bin/$binary"
done
SOURCE_DATE_EPOCH=123 GITHUB_SHA="$(printf 'a%.0s' {1..40})" \
    "$repo_root/dist/desktop/package-os-payload.sh" \
    "$payload_bin" x86_64 "$work/payload-a"
SOURCE_DATE_EPOCH=123 GITHUB_SHA="$(printf 'a%.0s' {1..40})" \
    "$repo_root/dist/desktop/package-os-payload.sh" \
    "$payload_bin" x86_64 "$work/payload-b"
cmp "$work/payload-a/tilo-desktop-os-x86_64.tar.gz" \
    "$work/payload-b/tilo-desktop-os-x86_64.tar.gz"
mkdir "$work/payload-unpacked"
tar -xzf "$work/payload-a/tilo-desktop-os-x86_64.tar.gz" \
    -C "$work/payload-unpacked"
(cd "$work/payload-unpacked" && sha256sum --check --strict SHA256SUMS)

echo "payload contract fixtures passed"
