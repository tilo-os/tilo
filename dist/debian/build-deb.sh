#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
version=$(<"$repo_root/dist/VERSION")
revision=${TILO_DEB_REVISION:-1}
out=${TILO_PACKAGE_OUT:-$repo_root/dist/debian/build}
arch=${TILO_DEB_ARCH:-$(dpkg --print-architecture)}
case "$arch" in amd64|arm64) ;; *) echo "unsupported Debian architecture: $arch" >&2; exit 1 ;; esac

if [[ ${TILO_SKIP_BUILD:-0} != 1 ]]; then
    bin_dir=$($repo_root/dist/build-binaries.sh | tail -n 1)
else
    bin_dir=${TILO_BIN_DIR:?set TILO_BIN_DIR when TILO_SKIP_BUILD=1}
fi

work=$out/work
[[ ! -e $work ]] || { echo "work path already exists: $work" >&2; exit 1; }
mkdir -p "$out" "$work"
trap 'rm -rf -- "$work"' EXIT
stage=$work/stage
"$repo_root/dist/stage-payload.sh" "$bin_dir" "$stage"

main=$work/tilo-session
dev=$work/tilo-session-devel
mkdir -p "$main"/{DEBIAN,etc/pam.d,usr/bin,usr/libexec,usr/lib/systemd/user,usr/share/wayland-sessions,usr/share/applications,usr/share/xdg-desktop-portal/portals,usr/share/pipewire,usr/share/doc/tilo-session}
install -pm 0755 "$stage/bin/tilo-shell" "$stage/bin/tilo-launcher" "$stage/bin/tilo-import-folders" "$main/usr/bin/"
install -pm 0755 "$stage/libexec/tilo-unlock-helper" "$stage/libexec/tilo-session" \
    "$stage/libexec/tilo-session-runner" "$stage/libexec/tilo-flight-recorder" \
    "$stage/libexec/tilo-session-report" \
    "$main/usr/libexec/"
install -pm 0644 "$stage/pam/tilo-unlock" "$main/etc/pam.d/"
install -pm 0644 "$stage/wayland-sessions/tilo.desktop" "$main/usr/share/wayland-sessions/"
install -pm 0644 "$stage/portal/tilo.portal" "$main/usr/share/xdg-desktop-portal/portals/"
install -pm 0644 "$stage/portal/tilo-portals.conf" "$main/usr/share/xdg-desktop-portal/"
install -pm 0644 "$stage/pipewire/tilo-echo-cancel.conf" "$main/usr/share/pipewire/"
install -pm 0644 "$stage/systemd-user/tilo-graphical-session.target" "$main/usr/lib/systemd/user/"
install -pm 0644 "$stage/license/LICENSE" "$main/usr/share/doc/tilo-session/copyright"
install -pm 0644 "$stage/applications/tilo-import-folders.desktop" "$main/usr/share/applications/"

mkdir -p "$dev"/{DEBIAN,usr/libexec,usr/share/wayland-sessions}
install -pm 0755 "$stage/libexec/tilo-session-dev" "$dev/usr/libexec/"
install -pm 0644 "$stage/wayland-sessions/tilo-app.desktop" "$dev/usr/share/wayland-sessions/"

runtime_stage=$work/runtime-stage
"$repo_root/dist/runtime/stage-payload.sh" "$bin_dir" "$runtime_stage"
runtime=$work/tilo-runtime
mkdir -p "$runtime"/{DEBIAN,usr/bin,usr/libexec/tilo-runtime,usr/lib/systemd/user,usr/share/applications,usr/share/icons/hicolor/scalable/apps,etc/apparmor.d,usr/share/doc/tilo-runtime}
install -pm 0755 "$runtime_stage"/libexec/* "$runtime/usr/libexec/tilo-runtime/"
ln -s ../libexec/tilo-runtime/tilo-runtime "$runtime/usr/bin/tilo-runtime"
install -pm 0644 "$runtime_stage/systemd-user/tilo-runtime.service" "$runtime/usr/lib/systemd/user/"
install -pm 0644 "$runtime_stage/applications/so.tilo.store.desktop" "$runtime/usr/share/applications/"
install -pm 0644 "$runtime_stage/icons/so.tilo.store.svg" "$runtime/usr/share/icons/hicolor/scalable/apps/"
install -pm 0644 "$runtime_stage/apparmor/tilo-runtime-launcher" "$runtime/etc/apparmor.d/"
install -pm 0644 "$runtime_stage/license/LICENSE" "$runtime/usr/share/doc/tilo-runtime/copyright"
# Load the launcher's user-namespace profile where AppArmor is active; Ubuntu
# 24.04+ refuses bubblewrap's namespace to unconfined programs otherwise.
cat > "$runtime/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = configure ] && command -v apparmor_parser >/dev/null 2>&1 \
    && aa-enabled --quiet 2>/dev/null; then
    apparmor_parser --replace --write-cache /etc/apparmor.d/tilo-runtime-launcher || true
fi
EOF
cat > "$runtime/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = remove ] && command -v apparmor_parser >/dev/null 2>&1 \
    && aa-enabled --quiet 2>/dev/null; then
    apparmor_parser --remove /etc/apparmor.d/tilo-runtime-launcher 2>/dev/null || true
fi
EOF
chmod 0755 "$runtime/DEBIAN/postinst" "$runtime/DEBIAN/postrm"
printf '/etc/apparmor.d/tilo-runtime-launcher\n' > "$runtime/DEBIAN/conffiles"

installed_size() { du -sk "$1" | cut -f1; }
mkdir -p "$work/debian"
cat > "$work/debian/control" <<'EOF'
Source: tilo-session
Section: x11
Priority: optional
Maintainer: Tilo contributors <tilo@code.tilo.so>

Package: tilo-session
Architecture: any
Description: Tilo collaborative compositor as a Wayland session
EOF
shlib_line=$(
    cd "$work"
    dpkg-shlibdeps -O \
        -e"$main/usr/bin/tilo-shell" \
        -e"$main/usr/bin/tilo-launcher" \
        -e"$main/usr/bin/tilo-import-folders" \
        -e"$main/usr/libexec/tilo-unlock-helper"
)
shlib_deps=${shlib_line#shlibs:Depends=}
cat > "$main/DEBIAN/control" <<EOF
Package: tilo-session
Version: $version-$revision
Section: x11
Priority: optional
Architecture: $arch
Maintainer: Tilo contributors <tilo@code.tilo.so>
Installed-Size: $(installed_size "$main")
Depends: $shlib_deps, libpam0g, libvulkan1, libegl1, bubblewrap, systemd, xdg-desktop-portal, xdg-dbus-proxy, python3
Recommends: mesa-vulkan-drivers, xwayland-satellite, greetd | gdm3 | sddm
Description: Tilo collaborative compositor as a Wayland session
 Installs the Tilo compositor, launcher, unlock helper, PAM policy, portal
 registration, and display-manager session entry on a Debian-family host.
EOF
cat > "$dev/DEBIAN/control" <<EOF
Package: tilo-session-devel
Version: $version-$revision
Section: x11
Priority: optional
Architecture: all
Maintainer: Tilo contributors <tilo@code.tilo.so>
Installed-Size: $(installed_size "$dev")
Depends: tilo-session (= $version-$revision), coreutils, grep
Description: developer session entry for Tilo
 Starts the explicitly configured, locally built compositor after validating
 ownership, permissions, and its developer-build marker.
EOF

runtime_shlib_line=$(
    cd "$work"
    dpkg-shlibdeps -O \
        -e"$runtime/usr/libexec/tilo-runtime/tilo-runtime" \
        -e"$runtime/usr/libexec/tilo-runtime/tilo-launcher" \
        -e"$runtime/usr/libexec/tilo-runtime/store"
)
runtime_shlib_deps=${runtime_shlib_line#shlibs:Depends=}
# The app's window and GPU stack is dlopen()ed, invisible to dpkg-shlibdeps.
cat > "$runtime/DEBIAN/control" <<EOF
Package: tilo-runtime
Version: $version-$revision
Section: utils
Priority: optional
Architecture: $arch
Maintainer: Tilo contributors <tilo@code.tilo.so>
Installed-Size: $(installed_size "$runtime")
Depends: $runtime_shlib_deps, bubblewrap, xdg-desktop-portal, xdg-dbus-proxy, libvulkan1, libegl1, libwayland-client0, libxkbcommon0
Recommends: mesa-vulkan-drivers, apparmor
Description: Tilo runtime for running Tilo apps on this desktop
 The per-user Tilo runtime: identity, collaboration and app services that
 independently launched Tilo apps attach to, each confined by bubblewrap.
 Includes the Tilo app for finding and opening apps. No compositor or login
 session; install tilo-session for the full Tilo desktop.
EOF

"$repo_root/dist/verify-payload.sh" "$main"
"$repo_root/dist/runtime/verify-payload.sh" "$runtime"
dpkg-deb --build --root-owner-group "$runtime" "$out/tilo-runtime_${version}-${revision}_${arch}.deb"
dpkg-deb --build --root-owner-group "$main" "$out/tilo-session_${version}-${revision}_${arch}.deb"
dpkg-deb --build --root-owner-group "$dev" "$out/tilo-session-devel_${version}-${revision}_all.deb"
printf '%s\n' "$out"/*.deb
