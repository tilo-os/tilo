#!/usr/bin/env bash
# The build half of the frozen RPM snapshot for this node's architecture: every package the
# Containerfiles name, signature-checked and laid out as a repository image, into OUT. Holds no
# secrets.
#
# Where this package set is already published, the published snapshot IS this output: it is
# verified (signature, provenance, both architectures) and copied rather than resolved from Fedora
# again, which takes a minute where a resolution takes tens of minutes and would in any case
# produce a different snapshot of the same package set, since Fedora's repositories move. Where it
# is not published yet, os/Containerfile.rpms is built from the live repositories here
# (oci-build.sh), and stable publishes the result.
#
# usage: rpms-build.sh ARCH OUT
#
# OUT/epoch is the snapshot's clock: the newest signature among its packages (snapshot_epoch).
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
arch=${1:?usage: rpms-build.sh ARCH OUT}
out=${2:?}
case $arch in
  x86_64) platform=linux/amd64 ;;
  aarch64) platform=linux/arm64 ;;
  *) die "unknown architecture $arch" ;;
esac
[[ $(uname -m) == "$arch" ]] || die "the $arch snapshot is built natively, not on $(uname -m)"
cd "$repo_root"
. os/base-snapshot.env
# The newest signature among the packages of the snapshot archive $1, as seconds: the clock an OS
# build from the unpublished snapshot takes (with its base's date), which rpm refuses a package
# signed after. The image itself is dated when it was resolved, which a build of the same packages
# minutes later moves, and with it every layer built on it; this moves only with the packages.
snapshot_epoch() {
  local dir blob epoch
  dir=$(mktemp -d "$scratch/snapshot.XXXXXX")
  mkdir "$dir/oci" "$dir/root"
  tar -C "$dir/oci" -xf "$1"
  for blob in "$dir"/oci/blobs/sha256/*; do
    [[ $(head -c 2 "$blob" | od -An -tx1 | tr -d ' \n') == 1f8b ]] || continue
    tar -C "$dir/root" -xzf "$blob"
  done
  epoch=$(LC_ALL=C TZ=UTC rpm -qp --nosignature --nodigest --qf '%{RSAHEADER:pgpsig}\n' "$dir"/root/repo/*.rpm \
    | sed -n 's/^[^,]*, \(.*\), Key ID .*/\1/p' | TZ=UTC date -f - +%s | sort -n | tail -n 1)
  rm -rf -- "$dir"
  [[ $epoch =~ ^[1-9][0-9]*$ ]] || die "no signed package in $1 to date the snapshot by"
  printf '%s\n' "$epoch"
}

# The image a registry serves for REF, as JSON: skopeo where the check image has it, else the
# anonymous reader resolve-rpm-snapshot.sh falls back to.
manifest_raw() {
  if command -v skopeo >/dev/null; then
    skopeo inspect --raw "docker://$1"
  else
    python3 "$repo_root/os/dev/registry-manifest.py" raw "$1"
  fi
}

# A published snapshot for this package set IS this check's output, so reuse it instead of
# resolving Fedora again. The job still reaches the registry, so t never serves this check's
# result (a job that connects out is keyed by every file of its state and not served); that is why
# the copy matters: it is the honest floor for a check that must ask the network.
status=0
bash "$repo_root/os/dev/resolve-rpm-snapshot.sh" --exists || status=$?
case $status in
  0)
    # resolve-rpm-snapshot.sh verifies the published snapshot against this tree's package set, its
    # base and both architectures. It reads the registry's JSON with jq and copies with skopeo, and
    # this check image carries neither (the images resolve their snapshot in ci-fast), so take
    # them here: a few seconds against the resolution they save.
    if ! command -v jq >/dev/null || ! command -v skopeo >/dev/null; then
      dnf install -y --setopt=install_weak_deps=False jq skopeo >/dev/null 2>&1 \
        || die 'could not install jq and skopeo to verify and copy the published snapshot'
    fi
    use_cosign
    ref=$(bash "$repo_root/os/dev/resolve-rpm-snapshot.sh")
    [[ $ref =~ ^registry\.tilo\.so/tilo/rpms@sha256:[0-9a-f]{64}$ ]] || die "unexpected snapshot $ref"
    digest=$(manifest_raw "$ref" | python3 -c '
import json, sys
arch = sys.argv[1]
images = [m["digest"] for m in json.load(sys.stdin).get("manifests", [])
          if m.get("platform", {}).get("os") == "linux"
          and m.get("platform", {}).get("architecture") == arch]
print(images[0] if len(images) == 1 else "")' "${platform#linux/}")
    [[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "the published snapshot $ref names no $arch image"
    # skopeo copies blobs and manifest verbatim, so the archive holds the published digest; buildah
    # rewrites the manifest, which would make this a second snapshot of the same package set.
    skopeo copy --quiet "docker://registry.tilo.so/tilo/rpms@$digest" "oci-archive:$out/image.oci.tar"
    [[ $(archive_digest "$out/image.oci.tar") == "$digest" ]] \
      || die "the copy of $digest is not the published image"
    printf '%s\n' "$digest" > "$out/digest"
    snapshot_epoch "$out/image.oci.tar" > "$out/epoch"
    printf 'reused the published snapshot %s (%s); dated %s by its newest package signature\n' \
      "$ref" "$arch" "$(cat "$out/epoch")"
    exit 0 ;;
  1)
    : ;; # not published: resolve Fedora below
  *) die 'could not ask the registry for the published RPM snapshot' ;;
esac

# The package set rpm-package-set derived from the OS Containerfiles, where the check gave it:
# the snapshot is keyed by that list, not by the Containerfiles, which change far more often than
# the packages they name. Derived here otherwise (a build outside t).
set_file=${RPM_PACKAGE_SET:-/target/publish/rpm-package-set/$arch.txt}
if [[ -s $set_file ]]; then
  packages=$(tr '\n' ' ' < "$set_file")
else
  packages=$(python3 os/dev/derive-rpm-package-set.py --arch "$arch" | tr '\n' ' ')
fi
[[ -n ${packages// /} ]] || die "no packages for $arch"
assets=$(pinned_assets virtio-win-0.1.302-1.noarch.rpm)
# The image is dated when its packages were resolved, not by the commit oci-build.sh would read
# from the log: an OS build from the published snapshot sets its offline dnf clock to the newest
# of its base's and its snapshot's dates (os/dev/image-epoch.sh), and rpm refuses a package signed
# after that clock, as every update signed since the base was mirrored is. A build from this
# unpublished one takes OUT/epoch instead. Given as SOURCE_DATE_EPOCH, so nothing reads Git and the
# check's key is not its commit's (t.toml, image-rpms-ARCH).
SOURCE_DATE_EPOCH=$(date -u +%s) bash tools/ci/publish/oci-build.sh . os/Containerfile.rpms "$out" \
  --platform "$platform" --build-context "github-assets=$assets" \
  --build-arg "BASE_REF=$BASE_REF" --build-arg "PACKAGES=$packages"
snapshot_epoch "$out/image.oci.tar" > "$out/epoch"
printf 'dated %s by its newest package signature\n' "$(cat "$out/epoch")"
