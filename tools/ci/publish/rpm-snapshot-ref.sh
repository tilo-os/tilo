#!/usr/bin/env bash
# The frozen RPM snapshot for this tree's package set, for the builds that install from it
# (relay, env-runtime): the published snapshot, verified (signature, provenance, both
# architectures) by os/dev/resolve-rpm-snapshot.sh, written to OUT/rpms-ref.
#
# Stable publishes a new package set in the same run first: publish-rpms is an optional need
# (it runs on stable only), so its snapshot resolves here in the same pass. Where it doesn't run
# and the package set is not published yet, OUT/unpublished names the snapshot's tag instead,
# and the builds install from the run's own image-rpms-ARCH, unsigned (lib.sh
# snapshot_for_build); nothing signs or publishes such a build.
#
# usage: rpm-snapshot-ref.sh OUT
# Runs in ci-fast, which has python3 and jq; Cosign comes from the pinned asset.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
out=${1:?usage: rpm-snapshot-ref.sh OUT}
# What publish-rpms leaves when it ran in this run.
published=/target/publish/rpms/index/receipt.json
use_cosign
mkdir -p "$out"
rm -f "$out/rpms-ref" "$out/unpublished"
status=0
bash "$repo_root/os/dev/resolve-rpm-snapshot.sh" --exists || status=$?
case $status in
  0)
    ref=$(bash "$repo_root/os/dev/resolve-rpm-snapshot.sh")
    [[ $ref =~ ^registry\.tilo\.so/tilo/rpms@sha256:[0-9a-f]{64}$ ]] || die "unexpected snapshot $ref"
    printf '%s\n' "$ref" > "$out/rpms-ref"
    printf 'frozen RPM snapshot %s\n' "$ref" ;;
  1)
    [[ ! -e $published ]] || die "publish-rpms published this package set in this run, yet its snapshot does not resolve ($published)"
    tag=$(bash "$repo_root/os/dev/resolve-rpm-snapshot.sh" --tag)
    printf '%s\n' "$tag" > "$out/unpublished"
    printf 'the frozen RPM snapshot %s is not published (stable publishes it): builds install from this run'"'"'s image-rpms, unsigned\n' "$tag" ;;
  *) die 'could not ask the registry for the published RPM snapshot' ;;
esac
