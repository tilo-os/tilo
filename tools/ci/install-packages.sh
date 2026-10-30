#!/usr/bin/env bash
# Install the RPMs a check's job needs that its image lacks, from one or more lists
# under tools/ci/packages/ (a package per line; `#` starts a comment).
#
#   install-packages.sh tools/ci/packages/<name>.txt...
#
# Where everything is installed already it returns at once, so a job's script can
# always run it.
set -euo pipefail
(($# > 0)) || { echo "usage: install-packages.sh LIST..." >&2; exit 2; }
mapfile -t wanted < <(sed -e 's/#.*//' -e 's/[[:space:]]//g' -e '/^$/d' "$@")
missing=()
for p in "${wanted[@]}"; do
  rpm -q --whatprovides "$p" >/dev/null 2>&1 || missing+=("$p")
done
((${#missing[@]} == 0)) && exit 0
dnf --setopt=retries=3 --setopt=install_weak_deps=False install -y "${missing[@]}"
