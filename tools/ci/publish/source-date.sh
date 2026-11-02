#!/usr/bin/env bash
# The date an image is built at, for oci-build.sh's SOURCE_DATE_EPOCH: the commit time of the
# newest commit that changed any of PATH..., the files the image is built from, written to OUT.
# oci-build.sh and build-lib.sh, which every such build runs, are always among them.
#
# usage: source-date.sh OUT PATH...
#
# Dating an image by its commit made every commit a new image, and the check that builds it had
# to read Git, so t kept its result per commit and built it again for each. Dated by its files,
# an image stays the same until a commit changes one of them, and changes when one does: a file
# in it of a different release is still told apart by its date (oci-build.sh). Only the check
# that runs this reads Git; it takes a second, and the build needs its output, not the commit.
#
# A PATH may be a directory (every file below it). Each must exist: an input renamed or removed
# without this list following it would stop moving the date, and the image would change under
# an unchanged one.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/build-lib.sh"
out=${1:?usage: source-date.sh OUT PATH...}
shift
(($#)) || die 'usage: source-date.sh OUT PATH...'
paths=("$@" tools/ci/publish/oci-build.sh tools/ci/publish/build-lib.sh)
for path in "${paths[@]}"; do
  [[ -e $repo_root/$path ]] || die "$path, an input of the image, is not in the tree"
done
epoch=$(git -C "$repo_root" log -1 --format=%ct HEAD -- "${paths[@]}")
[[ $epoch =~ ^[0-9]+$ && $epoch -gt 1 ]] || die "no commit changed ${paths[*]}: nothing to date the image by"
mkdir -p "$(dirname -- "$out")"
printf '%s\n' "$epoch" > "$out"
printf 'dated %s (%s) by %s\n' "$epoch" "$(git -C "$repo_root" log -1 --format='%h %cI' HEAD -- "${paths[@]}")" "${paths[*]}"
