# tilo-session — the Tilo compositor as a Wayland session on someone else's
# distribution.
#
# Why an RPM at all, on an image-based host: a login manager only offers
# sessions it finds in /usr/share/wayland-sessions, that directory is inside
# the read-only composefs on Silverblue/Bazzite, and neither
# /usr/local/share/wayland-sessions nor ~/.local/share/wayland-sessions is
# scanned by GDM. So the entry has to arrive as part of /usr, and on an
# ostree host the supported way to add to /usr is a layered package.
# `rpm-ostree install ./tilo-session-[0-9]*.rpm` puts this in the next deployment;
# `rpm-ostree usroverlay` plus unpacking it is the same file layout without a
# reboot, for trying it out (see DEVELOPMENT.md).
#
# The payload is prebuilt. `%build` compiles nothing: the workspace pins a
# pre-release crypto stack in Cargo.lock, needs a container full of native
# -devel packages, and takes tens of minutes — none of which belongs inside
# rpmbuild. dist/rpm/build-rpm.sh runs the cargo build and hands the results
# here as a tarball, and refuses to package anything it did not build.

%global tilo_summary Tilo compositor as a Wayland session
%{!?tilo_version:%global tilo_version 0.1.13}
# Release builds keep 1; a developer build names its commit here (e.g.
# 0.202609271530.git5764267571) so `rpm -q tilo-session` on a host says which
# tree is layered. See dist/rpm/host-update.sh.
%{!?tilo_release:%global tilo_release 1}

# The payload is prebuilt. Its native binaries deliberately keep their symbol
# table (`[profile.release]` in Cargo.toml) so a crash log on the user's
# machine names each frame's function, and carry no debug info to extract.
%global debug_package %{nil}

# Nothing in the payload is stripped here. The signed Tilo runtime is already
# stripped before its bundle manifest is hashed and signed, and Fedora's
# generic brp-strip pass would mutate the bundled ELF loader and shared
# libraries after that signature was created, leaving a formally packaged but
# unusable runtime; on the session binaries it would remove the symbol table
# the crash log needs. Keep the ordinary post-processing chain, but make its strip
# helpers no-ops for this prebuilt payload.
%global __strip /bin/true

# These signature-covered application trees are private sandbox payloads, not
# libraries exposed to the host. Keep RPM's automatic dependency generation
# for package binaries and exclude only these two private stores.
%global __provides_exclude_from ^/usr/lib/tilo-(apps|runtime)/.*$
%global __requires_exclude_from ^/usr/lib/tilo-(apps|runtime)/.*$
# The env-runtime environment image in the app store is a signed userland:
# rewriting its scripts' interpreter lines would change signed bytes.
%global __brp_mangle_shebangs_exclude_from ^/usr/lib/tilo-(apps|runtime)/.*$

Name:           tilo-session
Version:        %{tilo_version}
Release:        %{tilo_release}%{?dist}
Summary:        %{tilo_summary}
License:        AGPL-3.0-only
URL:            https://code.tilo.so/tilo/tilo
Source0:        %{name}-%{version}.tar.gz

# Tilo was renamed to Tilo at 0.1.9.  Keep this as an ordinary RPM rename so a
# package-manager update replaces the old package instead of installing a
# second login session beside it.  The bounded Obsoletes deliberately leaves
# any hypothetical post-rename Tilo fork alone; the versioned Provides lets
# dependencies on the former package name remain satisfiable during the
# transition.  Conflicts would prevent the solver from performing the rename,
# so none belongs here.
Provides:       tilo-session = %{version}-%{release}
Obsoletes:      tilo-session < 0.1.9

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
# Every Tilo-packaged application is launched inside a bubblewrap namespace.
# This is a hard runtime dependency even though it is invoked by name and
# therefore invisible to RPM's ELF dependency generator. Existing host apps
# deliberately keep their original confinement (or lack of it).
Requires:       bubblewrap
# A shell environment (`ush`) runs in a network namespace of its own; `pasta`
# connects it to the host's network, unprivileged. An environment granted
# hosts refuses to start without it rather than run unfiltered.
Requires:       passt
# Sandboxed Tilo applications reach file choosers and other desktop APIs only
# through the portal frontend and a filtered proxy. Without either helper the
# compositor starts, but ordinary app workflows are incomplete.
Requires:       xdg-desktop-portal
Requires:       xdg-dbus-proxy
# The session wrapper synchronizes desktop identity with both systemd's user
# manager and classic D-Bus activation, so host services such as GCR's prompt
# helper cannot retain the previous desktop's routing state.
Requires:       fuse3
Requires:       python3
Requires:       dbus-tools
# The wrapper owns graphical-session.target while Tilo is logged in. This makes
# standard session services available and stops them before another desktop
# starts.
Requires:       systemd
# The power service constructs both the logind and UPower proxies before it can
# receive PrepareForSleep. Without UPower it deliberately declines to run, so
# the mandatory lock-before-suspend path would be absent rather than merely
# losing a battery indicator.
Requires:       upower
# Rootless Xwayland maps legacy X11 windows into ordinary Wayland toplevels,
# preserving applications the user installed before adding Tilo.
Requires:       xwayland-satellite
# Legacy VMs (tilo-legacy): each app's windows reach the compositor through a
# waypipe client over vsock, and granted directories are shared into the VM by
# an unprivileged virtiofsd.
Requires:       waypipe
Requires:       virtiofsd
# Xwayland compiles an independent server-side XKB map. Settings changes use
# the standard authenticated X11 utility to update that map in place, so
# Electron/X11 windows keep working without logout or bridge restart.
Requires:       setxkbmap
# wgpu reaches Vulkan through ash, which dlopens libvulkan.so.1.
Requires:       vulkan-loader
# libEGL.so.1, for the DRM backend's buffer path. Require the ABI capability,
# not Fedora's current provider package name, so RPM can select whichever
# implementation supplies it without tripping explicit-library policy.
Requires:       libEGL.so.1()(64bit)
# A session without its Tilo application store is not a usable desktop: Quick
# Settings opens Settings through the ordinary package launcher, and the
# launcher has no system rows to offer on a first login.  Keep the store in a
# version-locked subpackage so normal `rpm-ostree update` advances the shell,
# app runtime and apps as one deployment while the large immutable payload is
# still separately visible to package managers.
Requires:       %{name}-apps = %{version}-%{release}
# An ICD for the loader to find. Recommends rather than Requires because the
# driver is hardware-specific — a Mesa host wants this one, an NVIDIA host gets
# its ICD from the proprietary or nvidia-open driver package instead, and
# depending on Mesa there would be wrong.
Recommends:     mesa-vulkan-drivers
# Tilo network controls use NetworkManager's nmcli API. The session remains
# usable without it and reports unavailable controls, so keep this soft for
# hosts that deliberately use another network stack.
Recommends:     NetworkManager
# The session is only reachable through a login manager that scans
# /usr/share/wayland-sessions. Not a hard Requires — greetd, SDDM and GDM all
# qualify and the package should not pick one for the host.
Recommends:     (gdm or sddm or greetd)

%description
The Tilo compositor, its app launcher and its session-unlock helper, installed
into the paths a Wayland login manager and the compositor itself expect, so
Tilo can be selected as a session on a host that runs some other desktop the
rest of the time.

This is not the Tilo operating system image. It installs a session and its
immutable Tilo application store, not a boot chain: no FUSE workspace mount or
greeter. Existing XDG and Flatpak applications remain available as clearly
labeled, unconfined host apps; rootless Xwayland keeps legacy X11 apps usable.
The system app store is merged with the user's own Tilo app store, and user
installs shadow matching preinstalled app ids.

%package apps
Summary:        %{tilo_summary} — immutable Tilo application store
Requires:       %{name} = %{version}-%{release}
Requires:       bubblewrap
Requires:       systemd
Provides:       tilo-session-apps = %{version}-%{release}
Obsoletes:      tilo-session-apps < 0.1.9

%description apps
The Tilo applications and signed shared runtime used by the Tilo host session.
It supplies Settings, Terminal, Files, Store, and the rest of the default Tilo
collection through the same read-only system registry that the Tilo OS image
uses. The launcher merges it with the user's writable Tilo store; user-installed
applications and their state are never overwritten by an RPM update.

%package devel
Summary:        %{tilo_summary} — developer session entry
Requires:       %{name} = %{version}-%{release}
Provides:       tilo-session-devel = %{version}-%{release}
Obsoletes:      tilo-session-devel < 0.1.9
# Every check the entry makes is a stat, a realpath or a grep.
Requires:       coreutils
Requires:       grep

%description devel
Adds a second entry to the session picker that starts the compositor named in
~/.config/tilo/dev-session instead of the installed one, so a compositor built
minutes ago can be logged into without reinstalling this package.

Install this only on a machine you develop Tilo on. It decides which program
runs as a login session, from a file in the user's home directory. It is kept
to a sub-package precisely so that decision is visible in `rpm -q` and absent
by default; see the header of /usr/libexec/tilo-session-dev for the checks it
makes before it execs anything, and DEVELOPMENT.md for the threat model.

%prep
%setup -q

%build
# Deliberately empty — see the header. The payload arrives prebuilt.

%install
install -Dpm 0755 bin/tilo-shell             %{buildroot}%{_bindir}/tilo-shell
install -Dpm 0755 bin/fs %{buildroot}%{_bindir}/fs
install -Dpm 0755 bin/tilo-relay-admin %{buildroot}%{_bindir}/tilo-relay-admin
install -Dpm 0755 bin/tilo-enroll %{buildroot}%{_bindir}/tilo-enroll
install -Dpm 0755 libexec/tilo-files-workspace %{buildroot}%{_libexecdir}/tilo-files-workspace
install -Dpm 0644 systemd-user/tilo-files-workspace.service %{buildroot}%{_userunitdir}/tilo-files-workspace.service
install -Dpm 0755 bin/tilo-launcher          %{buildroot}%{_bindir}/tilo-launcher
install -Dpm 0755 bin/tilo-import-folders %{buildroot}%{_bindir}/tilo-import-folders
install -Dpm 0644 applications/tilo-import-folders.desktop %{buildroot}%{_datadir}/applications/tilo-import-folders.desktop
# /usr/libexec, not on $PATH: only the compositor invokes it, and the path is
# compiled into the compositor as lock_auth::DEFAULT_UNLOCK_HELPER. Installing
# it anywhere else means a session that can be locked and never unlocked.
install -Dpm 0755 libexec/tilo-unlock-helper %{buildroot}%{_libexecdir}/tilo-unlock-helper
install -Dpm 0755 libexec/tilo-session       %{buildroot}%{_libexecdir}/tilo-session
install -Dpm 0755 libexec/tilo-session-runner %{buildroot}%{_libexecdir}/tilo-session-runner
install -Dpm 0755 libexec/tilo-flight-recorder %{buildroot}%{_libexecdir}/tilo-flight-recorder
install -Dpm 0755 libexec/tilo-session-report %{buildroot}%{_libexecdir}/tilo-session-report
install -Dpm 0644 pam/tilo-unlock            %{buildroot}%{_sysconfdir}/pam.d/tilo-unlock
install -Dpm 0644 wayland-sessions/tilo.desktop \
                  %{buildroot}%{_datadir}/wayland-sessions/tilo.desktop

# Portal backend. The desktop-specific preference file makes
# xdg-desktop-portal route requests here only inside a Tilo session; the
# descriptor declares the bus name and interfaces that implementation owns.
install -Dpm 0644 portal/tilo.portal \
                  %{buildroot}%{_datadir}/xdg-desktop-portal/portals/tilo.portal
install -Dpm 0644 portal/tilo-portals.conf \
                  %{buildroot}%{_datadir}/xdg-desktop-portal/tilo-portals.conf
install -Dpm 0644 systemd-user/tilo-graphical-session.target \
                  %{buildroot}%{_userunitdir}/tilo-graphical-session.target
# Settings offers "Reduce echo in calls" only where `pipewire -c` finds this.
install -Dpm 0644 pipewire/tilo-echo-cancel.conf \
                  %{buildroot}%{_datadir}/pipewire/tilo-echo-cancel.conf
install -Dpm 0644 license/LICENSE \
                  %{buildroot}%{_datadir}/licenses/%{name}/LICENSE

# The app-store source tree is staged at its final FHS paths so the registry's
# absolute payload paths are correct after installation.  Nothing here writes
# into a user's home directory.
cp -a apps/usr/. %{buildroot}%{_prefix}/
install -Dpm 0644 apps/etc/tilo/runtime-bundle-trusted-publishers.txt \
                  %{buildroot}%{_sysconfdir}/tilo/runtime-bundle-trusted-publishers.txt
install -Dpm 0755 apps/bin/app %{buildroot}%{_bindir}/app
install -Dpm 0755 apps/bin/tilo-install %{buildroot}%{_bindir}/tilo-install
install -Dpm 0755 apps/bin/tilo-app-package %{buildroot}%{_bindir}/tilo-app-package
install -Dpm 0755 apps/bin/tilo-runtime-bundle %{buildroot}%{_bindir}/tilo-runtime-bundle
install -Dpm 0755 apps/bin/tilo-model-manager %{buildroot}%{_bindir}/tilo-model-manager
install -Dpm 0755 apps/bin/tilo-model-gateway %{buildroot}%{_bindir}/tilo-model-gateway
install -Dpm 0755 apps/bin/tilo-model-http %{buildroot}%{_bindir}/tilo-model-http
install -Dpm 0755 apps/libexec/tilo-cli-shim %{buildroot}%{_libexecdir}/tilo-cli-shim
install -Dpm 0755 apps/libexec/tilo-app-host %{buildroot}%{_libexecdir}/tilo-app-host
install -Dpm 0755 apps/libexec/tilo-ush %{buildroot}%{_libexecdir}/tilo-ush
install -Dpm 0755 apps/libexec/tilo-relay-environment %{buildroot}%{_libexecdir}/tilo-relay-environment
install -Dpm 0755 apps/libexec/tilo-relay-offers %{buildroot}%{_libexecdir}/tilo-relay-offers
install -Dpm 0755 apps/bin/tilo-legacy %{buildroot}%{_bindir}/tilo-legacy
for unit in tilo-app-host tilo-models tilo-model-gateway tilo-ush; do
    install -Dpm 0644 apps/systemd-user/$unit.service \
        %{buildroot}%{_userunitdir}/$unit.service
    install -Dpm 0644 apps/systemd-user/$unit.socket \
        %{buildroot}%{_userunitdir}/$unit.socket
done
install -Dpm 0644 apps/user-tmpfiles/tilo-model-manager.conf \
                  %{buildroot}%{_datadir}/user-tmpfiles.d/tilo-model-manager.conf
install -Dpm 0644 apps/profile.d/tilo-apps.sh \
                  %{buildroot}%{_sysconfdir}/profile.d/tilo-apps.sh
install -Dpm 0644 apps/license/LICENSE \
                  %{buildroot}%{_datadir}/licenses/%{name}-apps/LICENSE

install -Dpm 0755 libexec/tilo-session-dev   %{buildroot}%{_libexecdir}/tilo-session-dev
install -Dpm 0644 wayland-sessions/tilo-app.desktop \
                  %{buildroot}%{_datadir}/wayland-sessions/tilo-app.desktop

%check
# Files readiness and normal same-person enrollment are first-party tools.
# Keep automatic ELF requirements enabled for these host executables; only the
# separately signed private app/runtime stores are excluded above.
for program in fs tilo-relay-admin tilo-enroll; do
    test -x %{buildroot}%{_bindir}/$program
done
test -x %{buildroot}%{_bindir}/tilo-import-folders
grep -Fxq "Exec=/usr/bin/tilo-import-folders --import-folders" %{buildroot}%{_datadir}/applications/tilo-import-folders.desktop
%{buildroot}%{_bindir}/tilo-import-folders --tilo-package-smoke
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
test -x %{buildroot}%{_libexecdir}/tilo-unlock-helper
# And the production session wrapper must not be redirectable.
grep -q '^TILO_SHELL=/usr/bin/tilo-shell$' %{buildroot}%{_libexecdir}/tilo-session
grep -q '^SESSION_RUNNER=/usr/libexec/tilo-session-runner$' %{buildroot}%{_libexecdir}/tilo-session
# A host has no unit that runs the one-time setup, so the shared lifecycle has
# to supply both declarations regardless of which entry wrapper selected it.
grep -q -- '--firstboot' %{buildroot}%{_libexecdir}/tilo-session-runner
grep -q -- '--host-desktop-session' %{buildroot}%{_libexecdir}/tilo-session-runner
# The portal backend is only reachable if something claims the session's name.
grep -q '^UseIn=tilo$' %{buildroot}%{_datadir}/xdg-desktop-portal/portals/tilo.portal
grep -q '^default=tilo$' %{buildroot}%{_datadir}/xdg-desktop-portal/tilo-portals.conf
grep -q '^DesktopNames=tilo$' %{buildroot}%{_datadir}/wayland-sessions/tilo.desktop
grep -q 'libpipewire-module-echo-cancel' %{buildroot}%{_datadir}/pipewire/tilo-echo-cancel.conf
grep -q '^Requires=graphical-session-pre.target graphical-session.target$' \
    %{buildroot}%{_userunitdir}/tilo-graphical-session.target
grep -q '^Wants=tilo-ush.service$' \
    %{buildroot}%{_userunitdir}/tilo-graphical-session.target
# The compositor owns the portal bus name in-process. A separate activation
# service would race it with a backend attached to an empty workspace.
test ! -e %{buildroot}%{_datadir}/dbus-1/services/org.freedesktop.impl.portal.desktop.tilo.service
test ! -e %{buildroot}%{_userunitdir}/tilo-portal.service
# Tilo Settings is launched from Quick Settings. A package that leaves its
# system registry empty regresses that click into a "no longer installed"
# notification, so verify the app and signed runtime are present.
test -s %{buildroot}%{_datadir}/tilo-apps/registry.toml
grep -q '^id = "so.tilo.settings"$' %{buildroot}%{_datadir}/tilo-apps/registry.toml
grep -q '^payload_root = "/usr/lib/tilo-apps/' %{buildroot}%{_datadir}/tilo-apps/registry.toml
test -d %{buildroot}%{_prefix}/lib/tilo-apps/pkgs
test ! -e %{buildroot}%{_datadir}/tilo-apps/pkgs
# One runtime build of the 2026.07 release, named by its ABI digest.
test -f %{buildroot}%{_prefix}/lib/tilo-runtime/2026.07+native.*/bundle.toml
test "$(stat -c %%a %{buildroot}%{_prefix}/lib/tilo-runtime/2026.07+native.*/bundle.toml)" = 644
# The runtime collector's lock lives in a per-user root; the system root is
# read-only and never collected, so staging leaves no .gc.lock to ship.
test ! -e %{buildroot}%{_prefix}/lib/tilo-runtime/.gc.lock
grep -q '/usr/lib/tilo-apps/bin' %{buildroot}%{_sysconfdir}/profile.d/tilo-apps.sh
test -s %{buildroot}%{_sysconfdir}/tilo/runtime-bundle-trusted-publishers.txt
# What the ush supervisor runs, at the paths it looks for them.
test -x %{buildroot}%{_libexecdir}/tilo-relay-environment
test -x %{buildroot}%{_libexecdir}/tilo-relay-offers
test -x %{buildroot}%{_bindir}/tilo-legacy
for unit in tilo-app-host tilo-models tilo-model-gateway tilo-ush; do
    test -f %{buildroot}%{_userunitdir}/$unit.service
    test -f %{buildroot}%{_userunitdir}/$unit.socket
    grep -q "^Service=$unit.service$" %{buildroot}%{_userunitdir}/$unit.socket
done

%files
%{_bindir}/tilo-shell
%{_bindir}/fs
%{_bindir}/tilo-relay-admin
%{_bindir}/tilo-enroll
%{_libexecdir}/tilo-files-workspace
%{_userunitdir}/tilo-files-workspace.service
%{_bindir}/tilo-launcher
%{_bindir}/tilo-import-folders
%{_datadir}/applications/tilo-import-folders.desktop
%{_libexecdir}/tilo-unlock-helper
%{_libexecdir}/tilo-session
%{_libexecdir}/tilo-session-runner
%{_libexecdir}/tilo-flight-recorder
%{_libexecdir}/tilo-session-report
%config(noreplace) %{_sysconfdir}/pam.d/tilo-unlock
%{_datadir}/wayland-sessions/tilo.desktop
%{_datadir}/xdg-desktop-portal/portals/tilo.portal
%{_datadir}/xdg-desktop-portal/tilo-portals.conf
%{_datadir}/pipewire/tilo-echo-cancel.conf
%{_userunitdir}/tilo-graphical-session.target
%license %{_datadir}/licenses/%{name}/LICENSE

%files apps
%{_bindir}/app
%{_datadir}/tilo/trust/tilo-main-registry-owner.pub
%{_bindir}/tilo-install
%{_bindir}/tilo-app-package
%{_bindir}/tilo-runtime-bundle
%{_bindir}/tilo-model-manager
%{_bindir}/tilo-model-gateway
%{_bindir}/tilo-model-http
%{_libexecdir}/tilo-cli-shim
%{_libexecdir}/tilo-app-host
%{_libexecdir}/tilo-ush
%{_libexecdir}/tilo-relay-environment
%{_libexecdir}/tilo-relay-offers
%{_bindir}/tilo-legacy
%{_userunitdir}/tilo-app-host.service
%{_userunitdir}/tilo-app-host.socket
%{_userunitdir}/tilo-models.service
%{_userunitdir}/tilo-models.socket
%{_userunitdir}/tilo-model-gateway.service
%{_userunitdir}/tilo-model-gateway.socket
%{_userunitdir}/tilo-ush.service
%{_userunitdir}/tilo-ush.socket
%{_datadir}/user-tmpfiles.d/tilo-model-manager.conf
%config %{_sysconfdir}/profile.d/tilo-apps.sh
%{_sysconfdir}/tilo/runtime-bundle-trusted-publishers.txt
%{_datadir}/tilo-apps
%{_prefix}/lib/tilo-apps
%{_prefix}/lib/tilo-runtime
%license %{_datadir}/licenses/%{name}-apps/LICENSE

%files devel
%{_libexecdir}/tilo-session-dev
%{_datadir}/wayland-sessions/tilo-app.desktop

%changelog
* Tue Sep 08 2026 Tilo <tilo@code.tilo.so> - 0.1.9-1
- Preserve requested placement across provisional Xwayland identities.
- Persist workspace creation at its mutation boundary.
- Refine compact onboarding, help, settings, and lock-screen layouts.

* Mon Sep 07 2026 Tilo <tilo@code.tilo.so> - 0.1.8-1
- Rebase the host-session fixes onto current main for laptop retesting.

* Mon Sep 07 2026 Tilo <tilo@code.tilo.so> - 0.1.7-1
- Expand production-rendered onboarding and lock-screen acceptance coverage.
- Harden German AltGr, Quick Settings, and real lock lifecycle regressions.

* Mon Sep 07 2026 Tilo <tilo@code.tilo.so> - 0.1.6-1
- Restore host-session Xwayland, ordered logout, and desktop UX.

* Sat Sep 05 2026 Tilo <tilo@code.tilo.so> - 0.1.5-1
- Keep ARM64 packaging available behind a native-runner capability switch.

* Fri Sep 04 2026 Tilo <tilo@code.tilo.so> - 0.1.4-1
- Require Bubblewrap for sandboxed application launches.
- Require the portal frontend and filtered D-Bus proxy for sandboxed apps.
- Build only the compositor binary from the shell package.
- Own the standard graphical-session lifecycle while Tilo is logged in.
- Keep X11-only host applications usable through rootless Xwayland.
- Select the Tilo portal backend with a desktop-specific preference file.

* Thu Sep 03 2026 Tilo <tilo@code.tilo.so> - 0.1.3-1
- Publish release assets with a dedicated repository-scoped credential

* Thu Sep 03 2026 Tilo <tilo@code.tilo.so> - 0.1.2-1
- Use a fully qualified Forgejo release action URL

* Thu Sep 03 2026 Tilo <tilo@code.tilo.so> - 0.1.1-1
- Fix the Forgejo release action source for package publication.

* Sat Aug 15 2026 Tilo <tilo@code.tilo.so> - 0.1.0-1
- First host session package: compositor, launcher, unlock helper, PAM stack
  portal and session entry, plus an optional developer session entry.
