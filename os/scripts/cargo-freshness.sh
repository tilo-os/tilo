#!/bin/sh
# Make Cargo's freshness check follow file contents in an image build.
#
#   cargo-freshness.sh WORKSPACE RECORD
#
# The image builders compile in a fresh `COPY` of the workspace against a target
# directory kept in a cache mount. Cargo compares mtimes, and a build that clamps
# timestamps (SOURCE_DATE_EPOCH, --rewrite-timestamp) can hand it an edited source
# that looks older than the cached artifact, so Cargo would reuse a stale rlib.
# The builders therefore ran `cargo clean` first, and every build recompiled the
# whole workspace, with sccache as the only thing between a warm rebuild and a
# cold one (the links and thin LTO are never cached).
#
# This records a SHA-256 per workspace file in RECORD (inside the target cache)
# and touches exactly the files whose contents differ from the last record, so
# they are newer than every cached artifact and Cargo rebuilds what depends on
# them, and only that. Exit 0: the target directory may be reused. Exit 3: there
# is no record yet (or it is unreadable), so the caller must clean, as before.
set -eu

root=$1
case "$2" in /*) record=$2 ;; *) record="$PWD/$2" ;; esac
now=$(mktemp)
trap 'rm -f "$now"' EXIT
cd "$root"
find . \( -path ./target -o -path ./.cargo-home -o -path ./.sccache -o -path ./.git \) -prune \
  -o -type f -print0 | sort -z | xargs -0 -r sha256sum > "$now"

# Every file starts at the same time a reusing build gives an unchanged one.
# Cargo fingerprints a path package whose build script declares no inputs
# (workspace-hack, vendored crates) by its newest file time, so a clean build
# that kept the checkout's times made the next build see every such package
# change and rebuild nearly everything once more.
if [ ! -s "$record" ]; then
  sed 's/^[0-9a-f]*  //' "$now" | tr '\n' '\0' | xargs -0 -r touch -c -h -d @1 --
  cp "$now" "$record"
  echo "cargo-freshness: no content record yet; cleaning the target directory once"
  exit 3
fi
# Unchanged files: older than every cached artifact, whatever mtime the COPY gave
# them, so an identical source never forces a rebuild.
awk 'NR == FNR { seen[$0] = 1; next } ($0 in seen) { sub(/^[0-9a-f]+  /, ""); print }' "$record" "$now" |
  tr '\n' '\0' | xargs -0 -r touch -c -h -d @1 --
# Files that are new or whose hash changed: newer than every cached artifact.
changed=$(awk 'NR == FNR { seen[$0] = 1; next } !($0 in seen) { sub(/^[0-9a-f]+  /, ""); print }' "$record" "$now")
count=0
if [ -n "$changed" ]; then
  printf '%s\n' "$changed" | while IFS= read -r file; do touch -c -- "$file"; done
  count=$(printf '%s\n' "$changed" | wc -l)
fi
cp "$now" "$record"
echo "cargo-freshness: $count changed file(s) marked newer than the cached build; reusing the target directory"
