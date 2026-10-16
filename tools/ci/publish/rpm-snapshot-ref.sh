#!/usr/bin/env bash
# The published frozen RPM snapshot for this tree's package set, verified (signature,
# provenance, both architectures) by os/dev/resolve-rpm-snapshot.sh, written to OUT/rpms-ref for
# builds that install from it. Fails while the snapshot for this package set is not published.
#
# usage: rpm-snapshot-ref.sh OUT
# Runs in ci-fast, which has python3 and jq; Cosign comes from the pinned asset.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
out=${1:?usage: rpm-snapshot-ref.sh OUT}
use_cosign
ref=$(bash "$repo_root/os/dev/resolve-rpm-snapshot.sh")
[[ $ref =~ ^registry\.tilo\.so/tilo/rpms@sha256:[0-9a-f]{64}$ ]] || die "unexpected snapshot $ref"
mkdir -p "$out"
printf '%s\n' "$ref" > "$out/rpms-ref"
printf 'frozen RPM snapshot %s\n' "$ref"
