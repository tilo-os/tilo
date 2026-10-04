#!/usr/bin/env bash
# Build an unpublished preview of this checkout's env-runtime, for checking
# tool builds on a branch before its runtime is published.
#
#   preview-env-runtime.sh OUT_TAPP
#
# Installs the packages os/Containerfile.env-runtime names into an empty
# root from the distribution's live repositories (the published runtime
# uses the frozen snapshot instead, so the two differ in versions), and
# seals usr/ and etc/ into an env-runtime tapp with a throwaway key. Never
# publish the result. Runs as root in a disposable container.
# Needs: dnf5, tilo-app-package on PATH or as TILO_APP_PACKAGE.
set -euo pipefail

die() { printf 'preview-env-runtime: %s\n' "$*" >&2; exit 2; }

[ "$#" -eq 1 ] || die "usage: $0 OUT_TAPP"
out=$1
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
definition="$repo/os/Containerfile.env-runtime"
packer=${TILO_APP_PACKAGE:-tilo-app-package}
version="$("$repo/os/dev/resolve-env-runtime.sh" --version)"

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
sed -n 's/^# tilo-rpm-snapshot-package: //p' "$definition" > "$work/packages.txt"
[ "$(wc -l < "$work/packages.txt")" -ge 10 ] || die "$definition names too few packages"
mkdir "$work/root"
xargs -a "$work/packages.txt" dnf5 --installroot="$work/root" --releasever=44 \
  --use-host-config --setopt=install_weak_deps=False --setopt=tsflags=nodocs \
  install -y --quiet
rpm --root "$work/root" -qa --qf '%{NAME}-%{EPOCHNUM}:%{VERSION}-%{RELEASE}.%{ARCH}\n' \
  | LC_ALL=C sort > "$work/installed.txt"
mkdir "$work/tree"
mv "$work/root/usr" "$work/root/etc" "$work/tree/"
chmod -R u+rwX,go+rX "$work/tree"
"$packer" seal-image --kind env-runtime --tree "$work/tree" \
  --id so.tilo.env-runtime --version "$version" --name 'Tilo environment runtime (preview)' \
  --arch "$(uname -m)" --packages "$work/installed.txt" --output "$out"
