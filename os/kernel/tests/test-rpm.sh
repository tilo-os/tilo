#!/usr/bin/env bash
# Test the packaged payload, not a parallel pre-packaging build tree.
set -euo pipefail
# Forgejo's non-interactive runner PATH can omit the system administration
# directories even when kmod is installed there.
export PATH="/usr/local/sbin:/usr/sbin:/sbin:$PATH"
command -v modinfo >/dev/null
rpm_file=$(realpath "${1:?usage: test-rpm.sh KERNEL.rpm NEW_OUTPUT_DIRECTORY}")
out=${2:?new output directory required}
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
test "$(rpm -qp --qf '%{NAME}' "$rpm_file")" = tilo-kernel-core
test "$(rpm -qp --qf '%{ARCH}' "$rpm_file")" = "$(uname -m)"
rpm -K "$rpm_file"
release=$(rpm -qp --qf '%{VERSION}' "$rpm_file")
tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT
mkdir "$tmp/stage"
(cd "$tmp/stage"; rpm2cpio "$rpm_file" | cpio -idm --quiet --no-absolute-filenames)
mapfile -t loopback_modules < <(find "$tmp/stage/lib/modules/$release" -type f \
  \( -name 'v4l2loopback.ko' -o -name 'v4l2loopback.ko.zst' \))
test "${#loopback_modules[@]}" -eq 1
test "$(modinfo -F name "${loopback_modules[0]}")" = v4l2loopback
test -n "$(modinfo -F signer "${loopback_modules[0]}")"
test -s "$tmp/stage/usr/share/licenses/tilo-kernel-core/v4l2loopback-COPYING"
tar -czf "$tmp/stage.tar.gz" -C "$tmp/stage" .
bash "$script_dir/boot-smoke.sh" "$tmp/stage.tar.gz" "$release" "$out"
sha256sum "$rpm_file" > "$out/rpm.sha256"
