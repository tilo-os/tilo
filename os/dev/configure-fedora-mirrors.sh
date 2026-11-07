#!/usr/bin/env bash
# Point a builder image's Fedora repository files at two current mirrors, and off
# the metalink that selects them. The metalink is mirrors.fedoraproject.org, a
# wildcard name whose answer differs per resolver and can be an address that
# never answers: the first `dnf install` in a build that reaches it then waits
# for its connection attempt (24 minutes on the arm64 runner), downloads
# nothing and fails. The RPM snapshot resolves from the same pair.
#
# usage: configure-fedora-mirrors.sh [REPOSITORY DIRECTORY]
#
# Runs inside a build stage (the pinned builder image, the snapshot's configured
# base), never on a machine: it rewrites /etc/dnf/dnf.conf as well as the
# repository files.
set -euo pipefail
repos=${1:-/etc/yum.repos.d}
# Two, both current: ftp.fau.de, and Fedora's own master (dl.fedoraproject.org)
# where a download from it fails, so one mirror throttling or resetting
# connections does not fail the build ("All mirrors were tried"). RPM
# signatures still provide the authenticity check.
sed -i \
  -e 's|^#baseurl=http://download.example/pub/fedora/linux/releases/\(.*\)$|baseurl=https://ftp.fau.de/fedora/linux/releases/\1 https://dl.fedoraproject.org/pub/fedora/linux/releases/\1|' \
  -e '/^metalink=.*repo=fedora-/s/^/#/' \
  "$repos/fedora.repo"
sed -i \
  -e 's|^#baseurl=http://download.example/pub/fedora/linux/updates/\(.*\)$|baseurl=https://ftp.fau.de/fedora/linux/updates/\1 https://dl.fedoraproject.org/pub/fedora/linux/updates/\1|' \
  -e 's|/Everything/SRPMS/|/Everything/source/tree/|g' \
  -e '/^metalink=.*repo=updates-/s/^/#/' \
  "$repos/fedora-updates.repo"
# Tilo never distributes Cisco's OpenH264 binary: Cisco's patent licence covers
# only binaries the user downloads from Cisco. Nothing these builders install
# comes from that repository, and asking it for metadata is another address a
# build has to reach.
rm -f "$repos/fedora-cisco-openh264.repo"
# Resolve and download with bounded parallelism rather than one package at a time.
printf '[main]\nmax_parallel_downloads=20\n' > /etc/dnf/dnf.conf
