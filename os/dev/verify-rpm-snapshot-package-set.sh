#!/usr/bin/env bash
# Fail before an expensive OS build when RPMS_REF was made for older package
# consumers. Snapshot images are scratch images, so copy their manifest out of
# a stopped container rather than trying to execute anything in them.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
runtime="${1:-podman}"
ref="${2:-${RPMS_REF:-}}"
[ -n "$ref" ] || { echo 'RPMS_REF is required' >&2; exit 2; }
case "$(uname -m)" in
  x86_64) expected_arch=amd64 ;;
  aarch64) expected_arch=arm64 ;;
  *) echo 'RPM snapshot verification requires a supported native build host' >&2; exit 2 ;;
esac

scratch_root="${TMPDIR:-$repo/os/output/.tmp}"
mkdir -p "$scratch_root"
work="$(mktemp -d -p "$scratch_root" tilo-rpm-snapshot-verify.XXXXXX)"
container=''
cleanup() {
  [ -z "$container" ] || "$runtime" rm "$container" >/dev/null 2>&1 || true
  find "$work" -depth -delete
}
trap cleanup EXIT

"$repo/os/dev/derive-rpm-package-set.py" > "$work/expected"
container="$("$runtime" create "$ref" /bin/true)"
# A single-architecture manifest can still be pulled on the wrong host. The
# direct package names alone cannot establish which RPM architecture it holds.
actual_arch=$("$runtime" image inspect --format '{{.Architecture}}' "$ref")
if [ "$actual_arch" != "$expected_arch" ]; then
  echo "frozen RPM snapshot architecture $actual_arch does not match native build host $expected_arch: $ref" >&2
  echo 'Use the promoted multi-architecture snapshot or the matching native child.' >&2
  exit 1
fi
if ! "$runtime" cp "$container:/repo/tilo-direct-package-specs.txt" "$work/actual"; then
  echo "frozen RPM snapshot has no package manifest: $ref" >&2
  echo 'Run the snapshot-rpms workflow before building the OS image.' >&2
  exit 1
fi

if ! diff -u "$work/actual" "$work/expected"; then
  echo "frozen RPM snapshot does not match the current Containerfiles: $ref" >&2
  echo 'Run the snapshot-rpms workflow before building the OS image.' >&2
  exit 1
fi
echo 'Frozen RPM snapshot package manifest matches every consumer.'
