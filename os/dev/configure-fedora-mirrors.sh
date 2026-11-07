#!/usr/bin/env bash
# Point a builder image's Fedora repository files at fixed HTTPS locations, and
# off the metalink that selects them. The metalink is mirrors.fedoraproject.org,
# a wildcard name whose answer differs per resolver and can be an address that
# never answers: the first `dnf install` in a build that reaches it then waits
# for its connection attempt (24 minutes on the arm64 runner), downloads
# nothing and fails. The RPM snapshot resolves from the same list.
#
# usage: configure-fedora-mirrors.sh [REPOSITORY DIRECTORY]
#
# Runs inside a build stage (the pinned builder image, the snapshot's configured
# base), never on a machine: it rewrites /etc/dnf/dnf.conf as well as the
# repository files.
set -euo pipefail
repos=${1:-/etc/yum.repos.d}
release=$(. /etc/os-release && printf '%s' "$VERSION_ID")
arch=$(uname -m)
# A release Fedora still serves is under /pub/fedora/linux on its mirrors; one
# that has gone end of life moves to /pub/archive/fedora/linux, where the
# builder images' older releases are. Ask once, for this image's own release,
# which of the two holds it, so a build follows one list and not a list per
# location: dnf tries every baseurl of a repository before failing, and a
# wrong one answers 404 to every request.
current="https://ftp.fau.de/fedora/linux https://dl.fedoraproject.org/pub/fedora/linux"
archived="https://archives.fedoraproject.org/pub/archive/fedora/linux https://dl.fedoraproject.org/pub/archive/fedora/linux"
reaches() {
  local base
  for base in $1; do
    curl -fs -I --max-time 15 -o /dev/null \
      "$base/releases/$release/Everything/$arch/os/repodata/repomd.xml" && return 0
  done
  return 1
}
if reaches "$current"; then
  bases=$current
elif reaches "$archived"; then
  bases=$archived
else
  echo "configure-fedora-mirrors: no Fedora mirror answered for release $release ($arch):" >&2
  echo "  $current" >&2
  echo "  $archived" >&2
  exit 1
fi
# Two hosts from that list, in the order dnf tries them until one answers: the
# second carries a release where a download from the first fails, so one mirror
# throttling or resetting connections does not fail the build ("All mirrors were
# tried"). RPM signatures still provide the authenticity check.
first=${bases%% *}
second=${bases#* }
sed -i \
  -e "s|^#baseurl=http://download.example/pub/fedora/linux/releases/\(.*\)$|baseurl=$first/releases/\1 $second/releases/\1|" \
  -e '/^metalink=.*repo=fedora-/s/^/#/' \
  "$repos/fedora.repo"
sed -i \
  -e "s|^#baseurl=http://download.example/pub/fedora/linux/updates/\(.*\)$|baseurl=$first/updates/\1 $second/updates/\1|" \
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
