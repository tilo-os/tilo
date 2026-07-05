# Wrapper spec: publishes an empty `qos-kernel-core` package that Requires:
# our `qos-kernel` binary RPM.  The OS Containerfile does
#   dnf swap kernel-core qos-kernel-core
# which resolves by package name, not by content — this wrapper is what
# makes the name match while the real kernel image + modules live in
# `qos-kernel` (produced by `make binrpm-pkg` upstream, which doesn't
# emit a `-core` subpackage on its own).
#
# _kver is passed on the command line from os/kernel/Containerfile so the
# Requires: line matches the version this image just built.

Name:      qos-kernel-core
Version:   %{_kver}
Release:   1%{?dist}
Summary:   qOS custom kernel (metapackage; pulls in qos-kernel)
License:   GPL-2.0-only
BuildArch: noarch
Provides:  kernel-core = %{version}
Provides:  installonlypkg(kernel)
Requires:  qos-kernel = %{version}

%description
Metapackage that pulls in the qOS custom kernel (qos-kernel).  Provides
kernel-core so `dnf swap kernel-core qos-kernel-core` resolves cleanly
against a Fedora-based rootfs.

%files
# no files — pure metapackage
