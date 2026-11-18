#!/usr/bin/env bash
# Mirrors this architecture's frozen RPM snapshot into the registry, so every read of it after
# this one is a copy by digest instead of another resolution from Fedora. The snapshot is
# rpms-source-ARCH's: where that found the set already in the registry (the published one, or a
# development snapshot an earlier job mirrored), there is nothing to do; where it built one,
# that image is pushed under `dev-set-<fingerprint>-<arch>`, written once so the tag names one
# digest forever. Nothing signs it: a release tag publishes the signed set under its own tag
# (publish-rpms) from the same bytes.
#
# Trusted jobs only (main, a release tag, a dispatch), which is what the identity token needs;
# a branch or a working tree reads the published set, or builds its own, and mirrors nothing.
#
# usage: rpms-mirror.sh ARCH SRC OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
arch=${1:?usage: rpms-mirror.sh ARCH SRC OUT}
src=${2:?}
out=${3:?}
case $arch in
  x86_64 | aarch64) ;;
  *) die "unknown architecture $arch" ;;
esac
[[ $(uname -m) == "$arch" ]] || die "the $arch snapshot is mirrored natively, not on $(uname -m)"
cd "$repo_root"
mkdir -p "$out"
fingerprint=$(python3 os/dev/rpm-snapshot-fingerprint.py)
[[ $fingerprint =~ ^[0-9a-f]{64}$ ]] || die 'no valid package-set fingerprint'
mirrored=registry.tilo.so/tilo/rpms:dev-set-$fingerprint-$arch
tagged() {
  local status=0 digest
  digest=$(python3 "$repo_root/os/dev/registry-manifest.py" digest "$1" 2>"$scratch/registry.err") || status=$?
  case $status in
    0) printf '%s' "$digest" ;;
    1) return 1 ;;
    *) die "could not ask the registry for $1: $(tail -n 1 "$scratch/registry.err")" ;;
  esac
}
if [[ -f $src/ref ]]; then
  printf 'the snapshot is in the registry already: %s\n' "$(cat "$src/ref")"
  printf '%s\n' "$(cat "$src/ref")" > "$out/ref"
  exit 0
fi
# The source built one here (a package set no tag has published yet): load it and push it under
# the once-written development tag.
[[ -s $src/image.oci.tar ]] || die "$src names neither a snapshot in the registry nor one built here"
if digest=$(tagged "$mirrored"); then
  printf 'the snapshot %s is mirrored already, as registry.tilo.so/tilo/rpms@%s\n' "$arch" "$digest"
  printf '%s\n' "registry.tilo.so/tilo/rpms@$digest" > "$out/ref"
  exit 0
fi
use_buildah buildah "${TILO_BUILDAH_DRIVER:-overlay}"
auth=$scratch/registry-auth.json
# The archive names no image to push by (oci-build.sh's index holds no ref name); the pull answers
# the id it loaded it as.
image=$(b pull --quiet "oci-archive:$src/image.oci.tar")
[[ -n $image ]] || die "could not load $src/image.oci.tar"
printf '%s' "$(id_token registry.tilo.so)" \
  | b login --authfile "$auth" -u oidc --password-stdin registry.tilo.so >/dev/null
b push --quiet --format oci --authfile "$auth" "$image" "docker://$mirrored"
digest=$(tagged "$mirrored") || die "the registry holds no $mirrored after mirroring it"
printf '%s\n' "registry.tilo.so/tilo/rpms@$digest" > "$out/ref"
printf 'mirrored the snapshot as %s\n' "$mirrored"
