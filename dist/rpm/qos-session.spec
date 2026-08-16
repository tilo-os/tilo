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

# The payload is already built and stripped; there is no source to extract
# debug symbols from, and an empty -debuginfo subpackage would just fail the
# build.
%global debug_package %{nil}

Name:           qos-session
Version:        0.1.0
Release:        1%{?dist}
Summary:        %{qos_summary}
License:        AGPL-3.0-only
URL:            https://code.podesta.ai/qos/qos
Source0:        %{name}-%{version}.tar.gz

# The payload is a set of x86-64 ELF binaries built for this host's
# architecture. Nothing here is portable to another one.
ExclusiveArch:  x86_64

# The unlock helper dlopen()s libpam at runtime rather than linking it, so the
# automatic ELF dependency scan cannot see this one.
Requires:       pam
# The session is only reachable through a login manager that scans
# /usr/share/wayland-sessions. Not a hard Requires — greetd, SDDM and GDM all
# qualify and the package should not pick one for the host.
Recommends:     (gdm or sddm or greetd)

%description
The qOS compositor, its app launcher and its session-unlock helper, installed
into the paths a Wayland login manager and the compositor itself expect, so
qOS can be selected as a session on a host that runs some other desktop the
rest of the time.

This is not the qOS operating system image. It installs a session, not a
boot chain: no FUSE workspace mount, no greeter, no app registry seeded by
the image. The compositor degrades to an empty app list rather than failing
when those are absent.

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
to a subpackage precisely so that decision is visible in `rpm -q` and absent
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

%files
%{_bindir}/qos-shell
%{_bindir}/qos-launcher
%{_libexecdir}/qos-unlock-helper
%{_libexecdir}/qos-session
%config(noreplace) %{_sysconfdir}/pam.d/qos-unlock
%{_datadir}/wayland-sessions/qos.desktop

%files devel
%{_libexecdir}/qos-session-dev
%{_datadir}/wayland-sessions/qos-dev.desktop

%changelog
* Sat Aug 15 2026 qOS <qos@code.podesta.ai> - 0.1.0-1
- First host session package: compositor, launcher, unlock helper, PAM stack
  and session entry, plus an optional developer session entry.
