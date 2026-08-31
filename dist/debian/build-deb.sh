#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
version=$(<"$repo_root/dist/VERSION")
out=${QOS_PACKAGE_OUT:-$repo_root/dist/debian/build}
arch=${QOS_DEB_ARCH:-$(dpkg --print-architecture)}
case "$arch" in amd64|arm64) ;; *) echo "unsupported Debian architecture: $arch" >&2; exit 1 ;; esac

if [[ ${QOS_SKIP_BUILD:-0} != 1 ]]; then
    bin_dir=$($repo_root/dist/build-binaries.sh | tail -n 1)
else
    bin_dir=${QOS_BIN_DIR:?set QOS_BIN_DIR when QOS_SKIP_BUILD=1}
fi

work=$out/work
[[ ! -e $work ]] || { echo "work path already exists: $work" >&2; exit 1; }
mkdir -p "$out" "$work"
trap 'rm -rf -- "$work"' EXIT
stage=$work/stage
"$repo_root/dist/stage-payload.sh" "$bin_dir" "$stage"

main=$work/qos-session
dev=$work/qos-session-devel
mkdir -p "$main"/{DEBIAN,etc/pam.d,usr/bin,usr/libexec,usr/share/wayland-sessions,usr/share/xdg-desktop-portal/portals}
install -pm 0755 "$stage/bin/qos-shell" "$stage/bin/qos-launcher" "$main/usr/bin/"
install -pm 0755 "$stage/libexec/qos-unlock-helper" "$stage/libexec/qos-session" "$main/usr/libexec/"
install -pm 0644 "$stage/pam/qos-unlock" "$main/etc/pam.d/"
install -pm 0644 "$stage/wayland-sessions/qos.desktop" "$main/usr/share/wayland-sessions/"
install -pm 0644 "$stage/portal/qos.portal" "$main/usr/share/xdg-desktop-portal/portals/"

mkdir -p "$dev"/{DEBIAN,usr/libexec,usr/share/wayland-sessions}
install -pm 0755 "$stage/libexec/qos-session-dev" "$dev/usr/libexec/"
install -pm 0644 "$stage/wayland-sessions/qos-dev.desktop" "$dev/usr/share/wayland-sessions/"

installed_size() { du -sk "$1" | cut -f1; }
mkdir -p "$work/debian"
cat > "$work/debian/control" <<'EOF'
Source: qos-session
Section: x11
Priority: optional
Maintainer: qOS contributors <qos@code.podesta.ai>

Package: qos-session
Architecture: any
Description: qOS collaborative compositor as a Wayland session
EOF
shlib_line=$(
    cd "$work"
    dpkg-shlibdeps -O \
        -e"$main/usr/bin/qos-shell" \
        -e"$main/usr/bin/qos-launcher" \
        -e"$main/usr/libexec/qos-unlock-helper"
)
shlib_deps=${shlib_line#shlibs:Depends=}
cat > "$main/DEBIAN/control" <<EOF
Package: qos-session
Version: $version-1
Section: x11
Priority: optional
Architecture: $arch
Maintainer: qOS contributors <qos@code.podesta.ai>
Installed-Size: $(installed_size "$main")
Depends: $shlib_deps, libpam0g, libvulkan1, libegl1
Recommends: mesa-vulkan-drivers, xdg-desktop-portal, greetd | gdm3 | sddm
Description: qOS collaborative compositor as a Wayland session
 Installs the qOS compositor, launcher, unlock helper, PAM policy, portal
 registration, and display-manager session entry on a Debian-family host.
EOF
cat > "$dev/DEBIAN/control" <<EOF
Package: qos-session-devel
Version: $version-1
Section: x11
Priority: optional
Architecture: all
Maintainer: qOS contributors <qos@code.podesta.ai>
Installed-Size: $(installed_size "$dev")
Depends: qos-session (= $version-1), coreutils, grep
Description: developer session entry for qOS
 Starts the explicitly configured, locally built compositor after validating
 ownership, permissions, and its developer-build marker.
EOF

"$repo_root/dist/verify-payload.sh" "$main"
dpkg-deb --build --root-owner-group "$main" "$out/qos-session_${version}-1_${arch}.deb"
dpkg-deb --build --root-owner-group "$dev" "$out/qos-session-devel_${version}-1_all.deb"
printf '%s\n' "$out"/*.deb
