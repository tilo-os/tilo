#!/usr/bin/env bash
# The bootc the live installer carries to install beside another system (os/dev/
# build-custom-installer.sh puts it at /usr/libexec/tilo-bootc-alongside), built from
# os/Containerfile.bootc-alongside-helper once for its sources instead of inside every medium.
# As root in a micro-VM step.
#
#   bootc-alongside.sh OUT     OUT/bootc
#
# The build context holds only the files the Containerfile copies, so the step is keyed by them.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"
out=${1:?usage: bootc-alongside.sh OUT}
modprobe fuse 2>/dev/null || true
context=$(mktemp -d "${TMPDIR:-/var/tmp}/bootc-alongside.XXXXXX")
trap 'rm -rf "$context"' EXIT
for file in os/Containerfile.bootc-alongside-helper \
  os/external-assets/bootc/0001-probe-missing-partition-metadata.patch \
  os/external-assets/bootc-install-reseal/0002-select-mounted-esp.patch \
  os/dev/configure-fedora-mirrors.sh; do
  install -D -m "$(stat -c %a "$file")" "$file" "$context/$file"
done
podman build -f "$context/os/Containerfile.bootc-alongside-helper" \
  -t localhost/tilo-bootc-alongside "$context"
container=$(podman create localhost/tilo-bootc-alongside /bootc)
rm -rf "$out"
mkdir -p "$out"
podman cp "$container:/bootc" "$out/bootc"
podman rm -f "$container" >/dev/null
chmod 0755 "$out/bootc"
printf 'built %s\n' "$(sha256sum "$out/bootc")"
