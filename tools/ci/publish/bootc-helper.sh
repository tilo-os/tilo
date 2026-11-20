#!/usr/bin/env bash
# A patched bootc, built once for its sources by a step of its own instead of inside every step
# that takes it. As root in a micro-VM step.
#
#   bootc-helper.sh alongside OUT   the bootc the live installer carries to install beside
#                                   another system (os/Containerfile.bootc-alongside-helper;
#                                   os/dev/build-custom-installer.sh puts it at
#                                   /usr/libexec/tilo-bootc-alongside)
#   bootc-helper.sh reseal OUT      the bootc that computes the composefs digest a sealed image's
#                                   UKI carries (os/Containerfile.bootc-helper; build-lib.sh's
#                                   bootc_helper takes it as TILO_BOOTC_RESEAL)
#
# Either is OUT/bootc. The build context holds only the files the Containerfile copies, so the step
# is keyed by them.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"
usage='usage: bootc-helper.sh alongside|reseal OUT'
case ${1:-} in
  alongside) containerfile=os/Containerfile.bootc-alongside-helper ;;
  reseal) containerfile=os/Containerfile.bootc-helper ;;
  *) die "$usage" ;;
esac
out=${2:?$usage}
modprobe fuse 2>/dev/null || true
context=$(mktemp -d "${TMPDIR:-/var/tmp}/bootc-helper.XXXXXX")
trap 'rm -rf "$context"' EXIT
# The sources of the Containerfile's COPY lines from the context (not from another stage).
mapfile -t sources < <(awk '$1 == "COPY" && $2 !~ /^--/ { for (i = 2; i < NF; i++) print $i }' "$containerfile")
((${#sources[@]} > 0)) || die "$containerfile copies nothing"
for pattern in "$containerfile" "${sources[@]}"; do
  for file in $pattern; do
    [[ -f $file ]] || die "$containerfile copies $pattern, which names no file"
    install -D -m "$(stat -c %a "$file")" "$file" "$context/$file"
  done
done
podman build -f "$context/$containerfile" -t localhost/tilo-bootc-helper "$context"
container=$(podman create localhost/tilo-bootc-helper /bootc)
rm -rf "$out"
mkdir -p "$out"
podman cp "$container:/bootc" "$out/bootc"
podman rm -f "$container" >/dev/null
chmod 0755 "$out/bootc"
printf 'built %s\n' "$(sha256sum "$out/bootc")"
