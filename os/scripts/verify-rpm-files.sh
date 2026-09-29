#!/bin/sh
# Fails when a file of a package this build installed has a different size or
# digest than the rpmdb records for it: a layer that lost file data (run 48898
# committed a shared library that was mostly zeros) is caught here, before the
# image leaves the build.
#
# The image build's packages are those installed at or after
# SOURCE_DATE_EPOCH; the base image's are older and are left to the base,
# which rewrites some of its own files on purpose. Config and ghost files may
# change, and Python rewrites stale bytecode it imports, so neither counts.
set -eu
epoch="${SOURCE_DATE_EPOCH:-0}"
if [ "$epoch" = 0 ]; then
  echo "SOURCE_DATE_EPOCH is unset; not telling this build's packages from the base's, so not verifying them"
  exit 0
fi
pkgs="$(rpm -qa --qf '%{INSTALLTIME} %{NEVRA}\n' | awk -v e="$epoch" '$1 >= e { print $2 }')"
[ -n "$pkgs" ] || { echo "no package was installed at or after SOURCE_DATE_EPOCH=$epoch" >&2; exit 1; }
count=$(printf '%s\n' "$pkgs" | wc -l)
bad="$(printf '%s\n' "$pkgs" \
  | xargs rpm -V --nomtime --nomode --nouser --nogroup --nordev --nocaps --nolinkto --nodeps --noscripts 2>/dev/null \
  | awk '$1 == "missing" { next }
         { type = (NF >= 3 ? $2 : "") }
         type == "c" || type == "g" { next }
         $NF ~ /\/__pycache__\/[^\/]+\.pyc$/ { next }
         substr($1, 1, 1) == "S" || substr($1, 3, 1) == "5" { print }' || true)"
if [ -n "$bad" ]; then
  printf 'files differ from the packages that installed them:\n%s\n' "$bad" >&2
  exit 1
fi
echo "verified the files of $count packages this build installed"
