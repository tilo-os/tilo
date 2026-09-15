#!/usr/bin/env bash
# Test the packaged payload, not a parallel pre-packaging build tree.
set -euo pipefail
rpm_file=$(realpath "${1:?usage: test-rpm.sh KERNEL.rpm NEW_OUTPUT_DIRECTORY}")
out=${2:?new output directory required}
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
test "$(rpm -qp --qf '%{NAME}' "$rpm_file")" = qos-kernel-core
test "$(rpm -qp --qf '%{ARCH}' "$rpm_file")" = "$(uname -m)"
rpm -K "$rpm_file"
release=$(rpm -qp --qf '%{VERSION}' "$rpm_file")
tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT
mkdir "$tmp/stage"
(cd "$tmp/stage"; rpm2cpio "$rpm_file" | cpio -idm --quiet --no-absolute-filenames)
tar -czf "$tmp/stage.tar.gz" -C "$tmp/stage" .
bash "$script_dir/boot-smoke.sh" "$tmp/stage.tar.gz" "$release" "$out"
sha256sum "$rpm_file" > "$out/rpm.sha256"
