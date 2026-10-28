#!/usr/bin/env bash
# The installer-only bootc, compiled once per pinned source and patch set.
#
#   bootc-alongside-helper.sh ARCH OUT
#
# os/Containerfile.bootc-alongside-helper pins the upstream release, its checksum and the build
# image, and applies Tilo's two patches; this builds it as os/dev/build-custom-installer.sh does
# and writes the binary to OUT/bootc. t keys the check by that Containerfile and the patches, so
# dev-installer-medium-ARCH takes the binary from the cache (TILO_BOOTC_ALONGSIDE) instead of
# compiling bootc for every medium.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

arch=${1:?usage: bootc-alongside-helper.sh ARCH OUT}
out=${2:?usage: bootc-alongside-helper.sh ARCH OUT}
[[ $(uname -m) == "$arch" ]] || die "the $arch helper builds natively, not on $(uname -m)"

# podman and its storage driver: the check's setup layer has them already (t.toml).
bash tools/ci/install-packages.sh tools/ci/packages/dev-installer-medium.txt
modprobe fuse 2>/dev/null || true

ref=localhost/tilo-bootc-alongside-helper:t
# The build fetches the pinned source archive and its crates, so it uses the guest's network.
podman build --network=host -f os/Containerfile.bootc-alongside-helper -t "$ref" .
mkdir -p "$out"
container=$(podman create "$ref" /bootc)
trap 'podman rm -f "$container" >/dev/null 2>&1 || true' EXIT
podman cp "$container:/bootc" "$out/bootc"
chmod 0755 "$out/bootc"
printf 'built %s: %s\n' "$out/bootc" "$("$out/bootc" --version)"
