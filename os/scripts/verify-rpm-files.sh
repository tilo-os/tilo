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
# Digesting every file is CPU-bound, so batches run on every core. Each batch
# passes its report on a line per write, which a pipe keeps whole between the
# concurrent writers.
bad="$(printf '%s\n' "$pkgs" \
  | xargs -n 16 -P "$(nproc)" sh -c '
      rpm -V --nomtime --nomode --nouser --nogroup --nordev --nocaps --nolinkto --nodeps --noscripts "$@" 2>/dev/null \
        | while IFS= read -r line; do printf "%s\n" "$line"; done' sh \
  | awk '$1 == "missing" { next }
         { type = (NF >= 3 ? $2 : "") }
         type == "c" || type == "g" { next }
         $NF ~ /\/__pycache__\/[^\/]+\.pyc$/ { next }
         substr($1, 1, 1) == "S" || substr($1, 3, 1) == "5" { print }' || true)"
if [ -n "$bad" ]; then
  printf 'files differ from the packages that installed them:\n%s\n' "$bad" >&2
  # Lost file data reads back as zeros: name the all-zero 4 KiB blocks.
  printf '%s\n' "$bad" | awk '{ print $NF }' | python3 -c '
import sys
for path in (line.strip() for line in sys.stdin):
    try:
        data = open(path, "rb").read()
    except OSError as error:
        print(f"  {path}: {error}", file=sys.stderr)
        continue
    zero = bytes(4096)
    blocks = [i for i in range(0, (len(data) + 4095) // 4096)
              if data[i * 4096:(i + 1) * 4096] == zero[:len(data[i * 4096:(i + 1) * 4096])]]
    ranges, start = [], None
    for i, b in enumerate(blocks):
        if start is None:
            start = b
        if i + 1 == len(blocks) or blocks[i + 1] != b + 1:
            ranges.append(f"{start}-{b}" if b > start else f"{start}")
            start = None
    total = (len(data) + 4095) // 4096
    listed = " ".join(ranges) or "none"
    print(f"  {path}: {len(data)} bytes, {len(blocks)} of {total} 4 KiB blocks all zero: {listed}",
          file=sys.stderr)
' || true
  exit 1
fi
echo "verified the files of $count packages this build installed"
