#!/bin/sh
# Make Cargo's freshness check follow file contents in an image build.
#
#   cargo-freshness.sh WORKSPACE RECORD    before the build
#   cargo-freshness.sh --built RECORD      after every Cargo build succeeded
#
# The image builders compile in a fresh `COPY` of the workspace against a target
# directory kept in a cache mount. Cargo compares mtimes, and a build that clamps
# timestamps (SOURCE_DATE_EPOCH, --rewrite-timestamp) can hand it an edited source
# that looks older than the cached artifact, so Cargo would reuse a stale rlib.
# The builders therefore ran `cargo clean` first, and every build recompiled the
# whole workspace, with sccache as the only thing between a warm rebuild and a
# cold one (the links and thin LTO are never cached).
#
# RECORD (inside the target cache) holds a SHA-256 per workspace file that every
# artifact in the target directory was built from. The first form touches
# exactly the files whose contents differ from it, so they are newer than every
# cached artifact and Cargo rebuilds what depends on them, and only that. Exit
# 0: the target directory may be reused. Exit 3: there is no record yet (or it
# is unreadable), so the caller must clean, as before.
#
# A changed file leaves the record until `--built` confirms the build that
# compiled it. A build that fails, is cancelled or never reaches a profile (the
# thin apps build beside the release binaries and die with them) leaves that
# profile's artifacts of the old contents behind; had the record already taken
# the new contents, the next build would see the file unchanged, mark it old,
# and Cargo would link the stale rlib, as a vendored crate's changed API did.
set -eu

if [ "$1" = --built ]; then
  case "$2" in /*) record=$2 ;; *) record="$PWD/$2" ;; esac
  # Without a pending record the first form did not finish; vouch for nothing.
  if [ -e "$record.next" ]; then
    mv -f -- "$record.next" "$record"
    echo "cargo-freshness: the target directory matches this workspace"
  else
    rm -f -- "$record"
  fi
  exit 0
fi

root=$1
case "$2" in /*) record=$2 ;; *) record="$PWD/$2" ;; esac
now=$(mktemp)
trap 'rm -f "$now"' EXIT
cd "$root"
find . \( -path ./target -o -path ./.cargo-home -o -path ./.sccache -o -path ./.git \) -prune \
  -o -type f -print0 | sort -z | xargs -0 -r sha256sum > "$now"
rm -f -- "$record.next"

# Every file starts at the same time a reusing build gives an unchanged one.
# Cargo fingerprints a path package whose build script declares no inputs
# (workspace-hack, vendored crates) by its newest file time, so a clean build
# that kept the checkout's times made the next build see every such package
# change and rebuild nearly everything once more.
if [ ! -s "$record" ]; then
  sed 's/^[0-9a-f]*  //' "$now" | tr '\n' '\0' | xargs -0 -r touch -c -h -d @1 --
  rm -f -- "$record"
  cp "$now" "$record.next"
  echo "cargo-freshness: no content record yet; cleaning the target directory once"
  exit 3
fi
# Unchanged files: older than every cached artifact, whatever mtime the COPY gave
# them, so an identical source never forces a rebuild.
unchanged=$(mktemp)
trap 'rm -f "$now" "$unchanged"' EXIT
awk 'NR == FNR { seen[$0] = 1; next } ($0 in seen)' "$record" "$now" > "$unchanged"
sed 's/^[0-9a-f]*  //' "$unchanged" | tr '\n' '\0' | xargs -0 -r touch -c -h -d @1 --
# Files that are new or whose hash changed: newer than every cached artifact.
changed=$(awk 'NR == FNR { seen[$0] = 1; next } !($0 in seen) { sub(/^[0-9a-f]+  /, ""); print }' "$record" "$now")
count=0
if [ -n "$changed" ]; then
  printf '%s\n' "$changed" | while IFS= read -r file; do touch -c -- "$file"; done
  count=$(printf '%s\n' "$changed" | wc -l)
fi
# Until this build succeeds, the record keeps only what it already vouched for.
cp "$unchanged" "$record"
cp "$now" "$record.next"
echo "cargo-freshness: $count changed file(s) marked newer than the cached build; reusing the target directory"
