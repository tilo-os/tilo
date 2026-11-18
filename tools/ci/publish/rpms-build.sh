#!/usr/bin/env bash
# The resolution half of the frozen RPM snapshot for this node's architecture: this tree's
# package set as the registry holds it — the published snapshot where a release tag published
# one, else the development snapshot this or an earlier job mirrored there — and, where neither
# exists, resolved from Fedora and mirrored. Either way it leaves OUT/ref, the digest-pinned
# reference the set is at, so every read of the snapshot after this one is a copy by digest
# (rpms-copy.sh) and t can serve it for an unchanged tree. Holds no secrets except the registry
# token it mirrors with.
#
# Where the job may not mirror (a branch, a working tree), and the registry has no set for this
# tree, OUT is the snapshot it built here (image.oci.tar, digest, epoch) and no reference, which
# is what a local build installs from. --ref-only is the mirror check's: it writes OUT/ref and
# copies nothing, and leaves the built snapshot where the registry could take none.
#
# usage: rpms-build.sh ARCH OUT [--ref-only]
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
arch=${1:?usage: rpms-build.sh ARCH OUT [--ref-only]}
out=${2:?}
ref_only=0
[[ ${3:-} == --ref-only ]] && ref_only=1
case $arch in
  x86_64) platform=linux/amd64 ;;
  aarch64) platform=linux/arm64 ;;
  *) die "unknown architecture $arch" ;;
esac
[[ $(uname -m) == "$arch" ]] || die "the $arch snapshot is built natively, not on $(uname -m)"
cd "$repo_root"
mkdir -p "$out"
. os/base-snapshot.env
fingerprint=$(python3 os/dev/rpm-snapshot-fingerprint.py)
[[ $fingerprint =~ ^[0-9a-f]{64}$ ]] || die 'no valid package-set fingerprint'
# Both tags are written `once` (oci-publish.sh): a package set is published once, so a tag names
# one digest forever and a read of it is a read of content the tree names.
published=registry.tilo.so/tilo/rpms:set-$fingerprint-$arch
mirrored=registry.tilo.so/tilo/rpms:dev-set-$fingerprint-$arch
# The digest TAG names; 1 when the registry has no such manifest, as resolve-rpm-snapshot.sh
# tells them apart.
tagged() {
  local status=0 digest
  digest=$(python3 "$repo_root/os/dev/registry-manifest.py" digest "$1" 2>"$scratch/registry.err") || status=$?
  case $status in
    0) printf '%s' "$digest" ;;
    1) return 1 ;;
    *) die "could not ask the registry for $1: $(tail -n 1 "$scratch/registry.err")" ;;
  esac
}
# The archive in OUT, mirrored under TAG: the image oci-build.sh left in its store, as it is.
mirror() {
  local tag=$1 auth=$scratch/registry-auth.json
  use_buildah buildah "${TILO_BUILDAH_DRIVER:-overlay}"
  printf '%s' "$(id_token registry.tilo.so)" \
    | b login --authfile "$auth" -u oidc --password-stdin registry.tilo.so >/dev/null
  b push --quiet --format oci --authfile "$auth" localhost/publish:build "docker://$tag"
  printf 'mirrored the snapshot as %s\n' "$tag"
}

ref=
if digest=$(tagged "$published"); then
  ref=registry.tilo.so/tilo/rpms@$digest
elif digest=$(tagged "$mirrored"); then
  ref=registry.tilo.so/tilo/rpms@$digest
else
  # The package set rpm-package-set derived from the OS Containerfiles, where the check gave it:
  # the snapshot is keyed by that list, not by the Containerfiles, which change far more often
  # than the packages they name. Derived here otherwise (a build outside t).
  set_file=${RPM_PACKAGE_SET:-/target/publish/rpm-package-set/$arch.txt}
  if [[ -s $set_file ]]; then
    packages=$(tr '\n' ' ' < "$set_file")
  else
    packages=$(python3 os/dev/derive-rpm-package-set.py --arch "$arch" | tr '\n' ' ')
  fi
  [[ -n ${packages// /} ]] || die "no packages for $arch"
  assets=$(pinned_assets virtio-win-0.1.302-1.noarch.rpm)
  # The image is dated when its packages were resolved, not by the commit oci-build.sh would read
  # from the log: an OS build from the snapshot sets its offline dnf clock to the newest of its
  # base's and its snapshot's dates (os/dev/image-epoch.sh), and rpm refuses a package signed
  # after that clock. Given as SOURCE_DATE_EPOCH, so nothing reads Git and the check's key is not
  # its commit's.
  # The image's last instruction copies only the repository metadata, so a layer per instruction
  # gives consumers of the snapshot that small layer to read (verify-rpm-snapshot-buildkit.py)
  # instead of streaming the multi-GB package layer.
  export TILO_OCI_LAYERS=1
  SOURCE_DATE_EPOCH=$(date -u +%s) bash tools/ci/publish/oci-build.sh . os/Containerfile.rpms "$out" \
    --platform "$platform" --build-context "github-assets=$assets" \
    --build-arg "BASE_REF=$BASE_REF" --build-arg "PACKAGES=$packages"
  # A trusted job of a followed branch mirrors the set it resolved, so every later read of this
  # tree's package set is a copy by digest instead of another resolution; nothing signs it, and a
  # release tag publishes the signed set under its own tag (publish-rpms). A branch or a working
  # tree may not write there, and its own snapshot is what it installs from.
  case ${T_REF:-} in
    main | tag:*)
      mirror "$mirrored"
      digest=$(tagged "$mirrored") || die "the registry holds no $mirrored after mirroring it"
      ref=registry.tilo.so/tilo/rpms@$digest
      # The registry holds the set from here on: what the job built was the way to it.
      rm -f "$out/image.oci.tar" "$out/digest"
      printf 'the snapshot %s is at %s\n' "$arch" "$ref" ;;
  esac
fi
[[ -z $ref ]] || printf '%s\n' "$ref" > "$out/ref"
if ((ref_only)); then
  [[ -n $ref ]] || printf 'no registry may hold this package set yet: %s keeps the snapshot it built\n' "$out"
  exit 0
fi
bash tools/ci/publish/rpms-copy.sh "$arch" "$out" "$out"
