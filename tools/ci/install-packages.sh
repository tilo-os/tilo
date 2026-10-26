#!/usr/bin/env bash
# Install the RPMs a check's job needs that its image lacks, from one or more lists
# under tools/ci/packages/ (a package per line; `#` starts a comment).
#
#   install-packages.sh tools/ci/packages/<name>.txt...
#
# A check runs it as its `setup` (t.toml), so t makes the installs a layer over the
# check's image once, cached by the image, this script and the lists; the job's own
# script runs it too, where it finds everything installed and returns at once, so
# the same script still works in an image without that layer.
set -euo pipefail
(($# > 0)) || { echo "usage: install-packages.sh LIST..." >&2; exit 2; }
mapfile -t wanted < <(sed -e 's/#.*//' -e 's/[[:space:]]//g' -e '/^$/d' "$@")
missing=()
for p in "${wanted[@]}"; do
  rpm -q --whatprovides "$p" >/dev/null 2>&1 || missing+=("$p")
done
((${#missing[@]} == 0)) && exit 0
dnf --setopt=retries=3 --setopt=install_weak_deps=False install -y "${missing[@]}"
