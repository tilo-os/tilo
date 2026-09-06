# qos-session — the qOS compositor as a Wayland session on someone else's
# distribution.
#
# Why an RPM at all, on an image-based host: a login manager only offers
# sessions it finds in /usr/share/wayland-sessions, that directory is inside
# the read-only composefs on Silverblue/Bazzite, and neither
# /usr/local/share/wayland-sessions nor ~/.local/share/wayland-sessions is
# scanned by GDM. So the entry has to arrive as part of /usr, and on an
# ostree host the supported way to add to /usr is a layered package.
# `rpm-ostree install ./qos-session-*.rpm` puts this in the next deployment;
# `rpm-ostree usroverlay` plus unpacking it is the same file layout without a
# reboot, for trying it out (see DEVELOPMENT.md).
#
# The payload is prebuilt. `%build` compiles nothing: the workspace pins a
# pre-release crypto stack in Cargo.lock, needs a container full of native
# -devel packages, and takes tens of minutes — none of which belongs inside
# rpmbuild. dist/rpm/build-rpm.sh runs the cargo build and hands the results
# here as a tarball, and refuses to package anything it did not just build.

%global qos_summary qOS compositor as a Wayland session
%{!?qos_version:%global qos_version 0.1.5}

# The payload is already built and stripped; there is no source to extract
# debug symbols from, and an empty -debuginfo subpackage would just fail the
# build.
%global debug_package %{nil}

Name:           qos-session
Version:        %{qos_version}
Release:        1%{?dist}
Summary:        %{qos_summary}
License:        AGPL-3.0-only
URL:            https://code.podesta.ai/qos/qos
Source0:        %{name}-%{version}.tar.gz

# The payload contains native ELF binaries. CI builds it on each enabled
# native release architecture; rpmbuild rejects every other architecture.
ExclusiveArch:  x86_64 aarch64

# Everything below is dlopen()ed at runtime, so the automatic ELF dependency
# scan cannot see any of it — `ldd` on the compositor lists the libinput/seat/
# xkbcommon stack and stops there. Left implicit, the package would install
# cleanly onto a host with no Vulkan loader and the session would die at the
# login screen instead, which is the worst of the two failures.
#
# The unlock helper resolves libpam itself (see its `pam` module).
Requires:       pam
# Every qOS-packaged application is launched inside a bubblewrap namespace.
# This is a hard runtime dependency even though it is invoked by name and
# therefore invisible to RPM's ELF dependency generator. Existing host apps
# deliberately keep their original confinement (or lack of it).
Requires:       bubblewrap
# Sandboxed qOS applications reach file choosers and other desktop APIs only
# through the portal frontend and a filtered proxy. Without either helper the
# compositor starts, but ordinary app workflows are incomplete.
Requires:       xdg-desktop-portal
Requires:       xdg-dbus-proxy
# The session wrapper synchronizes desktop identity with both systemd's user
# manager and classic D-Bus activation, so host services such as GCR's prompt
# helper cannot retain the previous desktop's routing state.
Requires:       dbus-tools
# The wrapper owns graphical-session.target while qOS is logged in. This makes
# standard session services available and stops them before another desktop
# starts.
Requires:       systemd
# Rootless Xwayland maps legacy X11 windows into ordinary Wayland toplevels,
# preserving applications the user installed before adding qOS.
Requires:       xwayland-satellite
# wgpu reaches Vulkan through ash, which dlopens libvulkan.so.1.
Requires:       vulkan-loader
# libEGL.so.1, for the DRM backend's buffer path. Require the ABI capability,
# not Fedora's current provider package name, so RPM can select whichever
# implementation supplies it without tripping explicit-library policy.
Requires:       libEGL.so.1()(64bit)
# An ICD for the loader to find. Recommends rather than Requires because the
# driver is hardware-specific — a Mesa host wants this one, an NVIDIA host gets
# its ICD from the proprietary or nvidia-open driver package instead, and
# depending on Mesa there would be wrong.
Recommends:     mesa-vulkan-drivers
# qOS network controls use NetworkManager's nmcli API. The session remains
# usable without it and reports unavailable controls, so keep this soft for
# hosts that deliberately use another network stack.
Recommends:     NetworkManager
# The session is only reachable through a login manager that scans
# /usr/share/wayland-sessions. Not a hard Requires — greetd, SDDM and GDM all
# qualify and the package should not pick one for the host.
Recommends:     (gdm or sddm or greetd)

%description
The qOS compositor, its app launcher and its session-unlock helper, installed
into the paths a Wayland login manager and the compositor itself expect, so
qOS can be selected as a session on a host that runs some other desktop the
rest of the time.

This is not the qOS operating system image. It installs a session, not a boot
chain: no FUSE workspace mount, no greeter, and no app registry seeded by the
image. Existing XDG and Flatpak applications remain available as clearly
labeled, unconfined host apps; rootless Xwayland keeps legacy X11 apps usable.

%package devel
Summary:        %{qos_summary} — developer session entry
Requires:       %{name} = %{version}-%{release}
# Every check the entry makes is a stat, a realpath or a grep.
Requires:       coreutils
Requires:       grep

%description devel
Adds a second entry to the session picker that starts the compositor named in
~/.config/qos/dev-session instead of the installed one, so a compositor built
minutes ago can be logged into without reinstalling this package.

Install this only on a machine you develop qOS on. It decides which program
runs as a login session, from a file in the user's home directory. It is kept
to a sub-package precisely so that decision is visible in `rpm -q` and absent
by default; see the header of /usr/libexec/qos-session-dev for the checks it
makes before it execs anything, and DEVELOPMENT.md for the threat model.

%prep
%setup -q

%build
# Deliberately empty — see the header. The payload arrives prebuilt.

%install
install -Dpm 0755 bin/qos-shell             %{buildroot}%{_bindir}/qos-shell
install -Dpm 0755 bin/qos-launcher          %{buildroot}%{_bindir}/qos-launcher
# /usr/libexec, not on $PATH: only the compositor invokes it, and the path is
# compiled into the compositor as lock_auth::DEFAULT_UNLOCK_HELPER. Installing
# it anywhere else means a session that can be locked and never unlocked.
install -Dpm 0755 libexec/qos-unlock-helper %{buildroot}%{_libexecdir}/qos-unlock-helper
install -Dpm 0755 libexec/qos-session       %{buildroot}%{_libexecdir}/qos-session
install -Dpm 0644 pam/qos-unlock            %{buildroot}%{_sysconfdir}/pam.d/qos-unlock
install -Dpm 0644 wayland-sessions/qos.desktop \
                  %{buildroot}%{_datadir}/wayland-sessions/qos.desktop

# Portal backend. The desktop-specific preference file makes
# xdg-desktop-portal route requests here only inside a qOS session; the
# descriptor declares the bus name and interfaces that implementation owns.
install -Dpm 0644 portal/qos.portal \
                  %{buildroot}%{_datadir}/xdg-desktop-portal/portals/qos.portal
install -Dpm 0644 portal/nirios-portals.conf \
                  %{buildroot}%{_datadir}/xdg-desktop-portal/nirios-portals.conf
install -Dpm 0644 systemd-user/qos-graphical-session.target \
                  %{buildroot}%{_userunitdir}/qos-graphical-session.target
install -Dpm 0644 license/LICENSE \
                  %{buildroot}%{_datadir}/licenses/%{name}/LICENSE

install -Dpm 0755 libexec/qos-session-dev   %{buildroot}%{_libexecdir}/qos-session-dev
install -Dpm 0644 wayland-sessions/qos-dev.desktop \
                  %{buildroot}%{_datadir}/wayland-sessions/qos-dev.desktop

%check
# A session entry that names a binary the package does not install is the
# failure mode this whole package exists to avoid, so prove the two agree
# rather than trusting the spec to have been edited consistently.
for entry in %{buildroot}%{_datadir}/wayland-sessions/*.desktop; do
    exec_path=$(sed -n 's/^Exec=\([^ ]*\).*/\1/p' "$entry")
    test -n "$exec_path" || { echo "no Exec= in $entry" >&2; exit 1; }
    test -x "%{buildroot}${exec_path}" || {
        echo "$entry points at ${exec_path}, which this package does not install" >&2
        exit 1
    }
done
# The compositor looks for the unlock helper at exactly one compiled-in path.
test -x %{buildroot}%{_libexecdir}/qos-unlock-helper
# And the production session wrapper must not be redirectable.
grep -q '^QOS_SHELL=/usr/bin/qos-shell$' %{buildroot}%{_libexecdir}/qos-session
# A host has no unit that runs the one-time setup, so the wrappers have to.
for wrapper in qos-session qos-session-dev; do
    grep -q -- '--firstboot' "%{buildroot}%{_libexecdir}/$wrapper" || {
        echo "$wrapper does not pass --firstboot; the identity would never be provisioned" >&2
        exit 1
    }
    grep -q -- '--host-desktop-session' "%{buildroot}%{_libexecdir}/$wrapper" || {
        echo "$wrapper does not identify itself as a host desktop session" >&2
        exit 1
    }
done
# The portal backend is only reachable if something claims the session's name.
grep -q '^UseIn=nirios$' %{buildroot}%{_datadir}/xdg-desktop-portal/portals/qos.portal
grep -q '^default=qos$' %{buildroot}%{_datadir}/xdg-desktop-portal/nirios-portals.conf
grep -q '^DesktopNames=nirios$' %{buildroot}%{_datadir}/wayland-sessions/qos.desktop
grep -q '^Requires=graphical-session-pre.target graphical-session.target$' \
    %{buildroot}%{_userunitdir}/qos-graphical-session.target
# The compositor owns the portal bus name in-process. A separate activation
# service would race it with a backend attached to an empty workspace.
test ! -e %{buildroot}%{_datadir}/dbus-1/services/org.freedesktop.impl.portal.desktop.qos.service
test ! -e %{buildroot}%{_userunitdir}/qos-portal.service

%files
%{_bindir}/qos-shell
%{_bindir}/qos-launcher
%{_libexecdir}/qos-unlock-helper
%{_libexecdir}/qos-session
%config(noreplace) %{_sysconfdir}/pam.d/qos-unlock
%{_datadir}/wayland-sessions/qos.desktop
%{_datadir}/xdg-desktop-portal/portals/qos.portal
%{_datadir}/xdg-desktop-portal/nirios-portals.conf
%{_userunitdir}/qos-graphical-session.target
%license %{_datadir}/licenses/%{name}/LICENSE

%files devel
%{_libexecdir}/qos-session-dev
%{_datadir}/wayland-sessions/qos-dev.desktop

%changelog
* Sat Sep 05 2026 qOS <qos@code.podesta.ai> - 0.1.5-1
- Keep ARM64 packaging available behind a native-runner capability switch.

* Fri Sep 04 2026 qOS <qos@code.podesta.ai> - 0.1.4-1
- Require Bubblewrap for sandboxed application launches.
- Require the portal frontend and filtered D-Bus proxy for sandboxed apps.
- Build only the compositor binary from the shell package.
- Own the standard graphical-session lifecycle while qOS is logged in.
- Keep X11-only host applications usable through rootless Xwayland.
- Select the qOS portal backend with a desktop-specific preference file.

* Thu Sep 03 2026 qOS <qos@code.podesta.ai> - 0.1.3-1
- Publish release assets with a dedicated repository-scoped credential

* Thu Sep 03 2026 qOS <qos@code.podesta.ai> - 0.1.2-1
- Use a fully qualified Forgejo release action URL

* Thu Sep 03 2026 qOS <qos@code.podesta.ai> - 0.1.1-1
- Fix the Forgejo release action source for package publication.

* Sat Aug 15 2026 qOS <qos@code.podesta.ai> - 0.1.0-1
- First host session package: compositor, launcher, unlock helper, PAM stack
  portal and session entry, plus an optional developer session entry.
