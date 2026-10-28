#!/usr/bin/env bash
# Publish one architecture's dev VM image and disk, independently of every
# other architecture of the same tag.
#
#   os/dev/publish-dev-disk.sh
#
# Run once the disk passed its gates: t's publish-os-dev-<arch>
# (tools/ci/publish/os-dev-publish.sh). It writes, in this order:
#
#   registry.tilo.so/tilo/tilo-vm:<tag>-<arch>          the dev image child
#   registry.tilo.so/tilo/tilo-vm:<tag>                 OCI index of every child
#                                                      of <tag> published so far
#   <packages>/tilo-vm-disk/<tag>/tilo-vm-<tag>-<arch>.<format>[.sha256]
#   <packages>/tilo-vm-disk/<tag>/tilo-vm-disk-<arch>.json[.sha256]
#   registry.tilo.so/tilo/tilo-vm:latest                (CHANNEL=true only)
#
# tilo-vm-disk-<arch>.json is the commit point for <arch>: it is written only
# after its disks and image exist, and `tilo-vm vm create` offers a tag to a
# host only once that host's file exists. The other architecture's lane adds
# its own child to the index and its own manifest whenever it finishes; neither
# waits for, or can be failed by, the other.
#
# The index is rebuilt from the per-architecture child tags rather than edited,
# and each lane repeats that until a fresh read agrees. Two lanes finishing at
# once therefore converge on the index holding both children: the lane that
# writes last read both child tags, because each lane pushes its child before
# it reads any.
#
# Environment (all required unless marked):
#   DEV_IMAGE          local dev image (containers-storage), e.g. localhost/tilo-vm:ci
#   DEV_TAG            tilo-vm tag and disk package version
#   ARCH               x86_64 or aarch64 (this lane's native architecture)
#   SOURCE_REVISION    full commit SHA the dev image was built from
#   DISK_TOOLING_REVISION full commit SHA supplying disk installer tooling
#   SOURCE_DISK        the installed qcow2, never booted
#   SOURCE_BOOT        aarch64: the direct-boot bundle directory
#   DISK_FORMATS       space-separated: qcow2 vhdx (default qcow2)
#   BASE_REPOSITORY    registry.tilo.so/tilo/tilo or registry.tilo.so/tilo/tilo-ci
#   BASE_TRUST         release or integration
#   BASE_INDEX_DIGEST  the base reference's top-level digest (index or child)
#   BASE_CHILD_DIGEST  the base's native child digest the dev image is FROM
#   CHANNEL            true advances tilo-vm:latest, only forward (default false)
#   TILO_PACKAGE_TOKEN, TILO_PACKAGE_PUBLIC_BASE_URL   generic package access
#   TILO_VM_PUBLISH_DIR  scratch for converted disks (default /var/tmp/tilo-vm-publish)
#   PUSHED_DIGEST      the child is already pushed as <tag>-<arch> with this
#                      digest (a BuildKit push); DEV_IMAGE is then unused
#   IMAGE_ONLY         1 publishes the child and index only: no disk, no disk
#                      manifest, no latest (Build / OS dev; SOURCE_DISK and the
#                      package settings are then ignored)
# Registry credentials for registry.tilo.so come from the job's login.
set -euo pipefail

REPOSITORY=registry.tilo.so/tilo/tilo-vm
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

die() { printf '::error::%s\n' "$*" >&2; exit 1; }
note() { printf '::notice::%s\n' "$*"; }

IMAGE_ONLY=${IMAGE_ONLY:-0}
[[ $IMAGE_ONLY == 0 || $IMAGE_ONLY == 1 ]] || die "IMAGE_ONLY must be 0 or 1"
required=(DEV_TAG ARCH SOURCE_REVISION DISK_TOOLING_REVISION BASE_REPOSITORY BASE_TRUST
          BASE_INDEX_DIGEST BASE_CHILD_DIGEST)
[[ $IMAGE_ONLY == 1 ]] || required+=(SOURCE_DISK TILO_PACKAGE_TOKEN TILO_PACKAGE_PUBLIC_BASE_URL)
[[ -n ${PUSHED_DIGEST:-} ]] || required+=(DEV_IMAGE)
for name in "${required[@]}"; do
  [[ -n ${!name:-} ]] || die "$name is required"
done
DISK_FORMATS=${DISK_FORMATS:-qcow2}
CHANNEL=${CHANNEL:-false}
out=${TILO_VM_PUBLISH_DIR:-/var/tmp/tilo-vm-publish}

[[ $SOURCE_REVISION =~ ^[0-9a-f]{40}$ ]] || die "SOURCE_REVISION must be a full commit SHA"
[[ $DISK_TOOLING_REVISION =~ ^[0-9a-f]{40}$ ]] || die "DISK_TOOLING_REVISION must be a full commit SHA"
[[ $DEV_TAG =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]{0,119}$ && $DEV_TAG != latest && $DEV_TAG != *-candidate-* ]] \
  || die "invalid tilo-vm tag: $DEV_TAG"
case "$ARCH" in
  x86_64) platform=amd64 ;;
  aarch64) platform=arm64 ;;
  *) die "unsupported architecture: $ARCH" ;;
esac
case "$BASE_TRUST:$BASE_REPOSITORY" in
  release:registry.tilo.so/tilo/tilo) ;;
  # An unsigned base must never take a name a signed release would use.
  integration:registry.tilo.so/tilo/tilo-ci) [[ $DEV_TAG == *-ci ]] || die "a tilo-ci base publishes only -ci tags, not $DEV_TAG" ;;
  *) die "base trust $BASE_TRUST does not match repository $BASE_REPOSITORY" ;;
esac
for digest in "$BASE_INDEX_DIGEST" "$BASE_CHILD_DIGEST"; do
  [[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "base digest $digest is not sha256:<64 hex>"
done
[[ $CHANNEL == true || $CHANNEL == false ]] || die "CHANNEL must be true or false"
[[ $IMAGE_ONLY == 1 ]] || test -s "$SOURCE_DISK" || die "no installed disk at $SOURCE_DISK"
if [[ -n ${PUSHED_DIGEST:-} ]]; then
  [[ $PUSHED_DIGEST =~ ^sha256:[0-9a-f]{64}$ ]] || die "PUSHED_DIGEST is not sha256:<64 hex>"
else
  podman image exists "$DEV_IMAGE" || die "$DEV_IMAGE is not in local storage"
fi

manifest_digest() { skopeo inspect --raw "docker://$1" | sha256sum | awk '{print "sha256:" $1}'; }
if [[ $IMAGE_ONLY == 0 ]]; then
# Large-disk runners do not resolve the in-cluster Forgejo service, so uploads
# cross the public TLS edge.
package_api=${TILO_PACKAGE_PUBLIC_BASE_URL%/}
[[ $package_api == https://*/api/packages/*/generic && $package_api != *'.svc.cluster.local'* ]] \
  || die "TILO_PACKAGE_PUBLIC_BASE_URL must be a public HTTPS generic-package base"
package="$package_api/tilo-vm-disk/$DEV_TAG"
manifest_file="tilo-vm-disk-$ARCH.json"

# The per-architecture manifest is immutable once written: an exact rerun of
# a lane that already committed has nothing left to do.
status=$(curl --silent --show-error --retry 3 --output /dev/null --write-out '%{http_code}' \
  --header "Authorization: Bearer $TILO_PACKAGE_TOKEN" "$package/$manifest_file")
case "$status" in
  200) note "tilo-vm-disk $DEV_TAG already publishes $ARCH ($package/$manifest_file)"; exit 0 ;;
  404) ;;
  3??) die "reading $package/$manifest_file was redirected (HTTP $status): the package base moved; update TILO_PACKAGE_PUBLIC_BASE_URL" ;;
  *) die "reading $package/$manifest_file answered HTTP $status" ;;
esac
fi

# ---- 1. the child ------------------------------------------------------------
# Not committed until the manifest below exists, so a rerun may replace it.
child_tag="$REPOSITORY:$DEV_TAG-$ARCH"
if [[ -n ${PUSHED_DIGEST:-} ]]; then
  image_digest=$PUSHED_DIGEST
  [[ $(manifest_digest "$child_tag") == "$image_digest" ]] || die "$child_tag does not name $image_digest"
else
  digest_file=$(mktemp /var/tmp/tilo-vm-digest.XXXXXX)
  podman push --digestfile "$digest_file" "$DEV_IMAGE" "docker://$child_tag"
  image_digest=$(tr -d '[:space:]' < "$digest_file")
  rm -f "$digest_file"
fi
[[ $image_digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "podman push reported no digest for $child_tag"
config=$(skopeo inspect "docker://$REPOSITORY@$image_digest")
[[ $(jq -r .Architecture <<< "$config") == "$platform" ]] || die "$child_tag is not linux/$platform"
[[ $(jq -r '.Labels["org.opencontainers.image.revision"] // empty' <<< "$config") == "$SOURCE_REVISION" ]] \
  || die "$child_tag does not carry revision $SOURCE_REVISION"
# $child_tag moves with every rerun of this series, and the registry drops a
# manifest that loses its last tag. A tag named after the digest keeps this
# child reachable for the disk manifest that records it; pushing the same
# bytes again re-tags the same digest.
skopeo copy --preserve-digests "docker://$REPOSITORY@$image_digest" \
  "docker://$REPOSITORY:$DEV_TAG-$ARCH-${image_digest:7:16}" >/dev/null
note "pushed $child_tag ($image_digest)"

# ---- 2. the index of every child published so far ------------------------------
# One child per architecture whose tag names this source revision. This lane's
# child must be the digest it just pushed; anything else is a concurrent writer
# of the same architecture, which must not be silently merged.
children() {
  local arch arch_platform raw digest child_config
  for arch in x86_64 aarch64; do
    case "$arch" in x86_64) arch_platform=amd64 ;; aarch64) arch_platform=arm64 ;; esac
    raw=$(skopeo inspect --raw "docker://$REPOSITORY:$DEV_TAG-$arch" 2>/dev/null) || continue
    digest="sha256:$(printf '%s' "$raw" | sha256sum | awk '{print $1}')"
    child_config=$(skopeo inspect "docker://$REPOSITORY@$digest") || continue
    [[ $(jq -r .Architecture <<< "$child_config") == "$arch_platform" ]] || continue
    [[ $(jq -r '.Labels["org.opencontainers.image.revision"] // empty' <<< "$child_config") == "$SOURCE_REVISION" ]] || continue
    jq -nc --arg arch "$arch" --arg digest "$digest" '{architecture:$arch,digest:$digest}'
  done | jq -sc 'sort_by(.architecture)'
}
index_children() {
  local raw
  raw=$(skopeo inspect --raw "docker://$1" 2>/dev/null) || { echo '[]'; return; }
  jq -c '[.manifests[]? | {
      architecture: (if .platform.architecture == "amd64" then "x86_64"
                     elif .platform.architecture == "arm64" then "aarch64"
                     else .platform.architecture end),
      digest: .digest
    }] | sort_by(.architecture)' <<< "$raw"
}
index_tag="$REPOSITORY:$DEV_TAG"
converged=false
for attempt in 1 2 3 4 5 6; do
  wanted=$(children)
  [[ $(jq -r --arg arch "$ARCH" '.[] | select(.architecture == $arch) | .digest' <<< "$wanted") == "$image_digest" ]] \
    || die "$child_tag no longer names $image_digest: another $ARCH lane is publishing $DEV_TAG"
  if [[ $(index_children "$index_tag") == "$wanted" ]]; then
    converged=true
    break
  fi
  # buildx writes the index straight to the registry; podman needs a local
  # store, which a lane that merely has podman installed cannot always set up.
  if docker buildx version >/dev/null 2>&1; then
    mapfile -t sources < <(jq -r --arg repo "$REPOSITORY" '.[] | "\($repo)@\(.digest)"' <<< "$wanted")
    docker buildx imagetools create --tag "$index_tag" "${sources[@]}"
  else
    list="tilo-vm-index-$DEV_TAG-$ARCH-$$"
    podman manifest rm "$list" >/dev/null 2>&1 || true
    podman manifest create "$list" >/dev/null
    for digest in $(jq -r '.[].digest' <<< "$wanted"); do
      podman manifest add "$list" "docker://$REPOSITORY@$digest" >/dev/null
    done
    podman manifest push --all "$list" "docker://$index_tag"
    podman manifest rm "$list" >/dev/null
  fi
  # A concurrent lane may have written between our read and write; let its
  # write land before reading back.
  sleep $(( attempt * 5 ))
done
[[ $converged == true ]] || die "$index_tag did not converge on the published children"
index_digest=$(manifest_digest "$index_tag")
# The index is rebuilt whenever another architecture lands; keep each one
# reachable by its digest, as for the child above. Only the index is copied:
# its children are already in the repository, and copying them again with
# --all refuses an index whose children mix gzip and zstd layers under
# --preserve-digests.
skopeo copy --multi-arch index-only --preserve-digests "docker://$REPOSITORY@$index_digest" \
  "docker://$REPOSITORY:$DEV_TAG-${index_digest:7:16}" >/dev/null
note "$index_tag ($index_digest) holds $(jq -r '[.[].architecture] | join(", ")' <<< "$wanted")"
[[ $IMAGE_ONLY == 0 ]] || exit 0

# ---- 3. the disks --------------------------------------------------------------
rm -rf -- "$out"
mkdir -p "$out"
virtual_size=$(qemu-img info --output=json "$SOURCE_DISK" | jq -er '."virtual-size"')
disks='[]'
for format in $DISK_FORMATS; do
  file="tilo-vm-$DEV_TAG-$ARCH.$format"
  case "$format" in
    # Compressed clusters are read transparently by QEMU (UTM, libvirt), so
    # the download is small and needs no unpacking.
    qcow2)
      if qemu-img info --output=json "$SOURCE_DISK" | jq -e '."format-specific".data."compression-type" == "zstd"' >/dev/null; then
        # The CI dev-disk lane already wrote zstd qcow2 clusters. A hard link
        # gives the package its versioned name without allocating another disk.
        ln -- "$SOURCE_DISK" "$out/$file"
      else
        qemu-img convert -p -f qcow2 -O qcow2 -c -o compression_type=zstd "$SOURCE_DISK" "$out/$file"
      fi ;;
    vhdx) qemu-img convert -p -f qcow2 -O vhdx -o subformat=dynamic "$SOURCE_DISK" "$out/$file" ;;
    *) die "unsupported disk format: $format" ;;
  esac
  qemu-img check -f "$format" "$out/$file" >/dev/null
  (cd "$out" && sha256sum "$file" > "$file.sha256")
  disks=$(jq -c --arg format "$format" --arg file "$file" --arg url "$package/$file" \
    --arg sha256 "$(awk '{print $1}' "$out/$file.sha256")" --argjson size "$(stat -c %s "$out/$file")" \
    --argjson virtual "$virtual_size" --arg arch "$ARCH" --arg image "$image_digest" --arg base "$BASE_CHILD_DIGEST" \
    '. + [{arch:$arch,format:$format,file:$file,url:$url,sha256:$sha256,size:$size,
           virtualSize:$virtual,imageDigest:$image,baseDigest:$base}]' <<< "$disks")
done
[[ $(jq length <<< "$disks") -gt 0 ]] || die "no disk format requested"

direct_boot='[]'
if [[ $ARCH == aarch64 ]]; then
  boot=${SOURCE_BOOT:?SOURCE_BOOT is required for aarch64}
  test -s "$boot/Image" && test -s "$boot/initrd" && test -s "$boot/boot.json" \
    || die "$boot holds no complete direct-boot bundle"
  kernel_file="tilo-vm-$DEV_TAG-$ARCH-direct-Image"
  initrd_file="tilo-vm-$DEV_TAG-$ARCH-direct-initrd"
  boot_file="tilo-vm-$DEV_TAG-$ARCH-direct-boot.json"
  cp -- "$boot/Image" "$out/$kernel_file"
  cp -- "$boot/initrd" "$out/$initrd_file"
  jq --arg kernel "$kernel_file" --arg initrd "$initrd_file" '.kernel=$kernel | .initrd=$initrd' \
    "$boot/boot.json" > "$out/$boot_file"
  files='[]'
  for file in "$kernel_file" "$initrd_file" "$boot_file"; do
    (cd "$out" && sha256sum "$file" > "$file.sha256")
    files=$(jq -c --arg file "$file" --arg url "$package/$file" \
      --arg sha256 "$(awk '{print $1}' "$out/$file.sha256")" --argjson size "$(stat -c %s "$out/$file")" \
      '. + [{file:$file,url:$url,sha256:$sha256,size:$size}]' <<< "$files")
  done
  direct_boot=$(jq -nc --arg arch "$ARCH" --arg manifest "$boot_file" --argjson files "$files" \
    '[{arch:$arch,manifest:$manifest,files:$files}]')
fi

uploads=$(mktemp "${RUNNER_TEMP:-/var/tmp}/tilo-vm-disk-uploads.XXXXXX")
while read -r file; do
  printf '%s\t%s\n' "$out/$file" "$package/$file" "$out/$file.sha256" "$package/$file.sha256" >> "$uploads"
done < <(jq -r '.[].file' <<< "$disks"; jq -r '.[].files[].file' <<< "$direct_boot")
TILO_PACKAGE_UPLOAD_WORKERS=2 "$REPO/tools/upload_immutable_packages.sh" "$uploads"

# ---- 4. the commit point for this architecture ---------------------------------
jq -n --arg tag "$DEV_TAG" --arg arch "$ARCH" --arg source "$SOURCE_REVISION" \
  --arg tooling "$DISK_TOOLING_REVISION" \
  --arg image "$index_tag" --arg child "$child_tag" --arg imageDigest "$image_digest" \
  --arg baseImage "$BASE_REPOSITORY" --arg baseTrust "$BASE_TRUST" --arg baseDigest "$BASE_INDEX_DIGEST" \
  --argjson disks "$disks" --argjson directBoot "$direct_boot" '
  {
    schema: "https://tilo.sh/dev/disk/v1",
    tag: $tag,
    arch: $arch,
    sourceRevision: $source,
    diskToolingRevision: $tooling,
    image: $image,
    childImage: $child,
    imageDigest: $imageDigest,
    baseImage: $baseImage,
    baseTrust: $baseTrust,
    baseDigest: $baseDigest,
    directBoot: $directBoot,
    disks: $disks
  }' > "$out/$manifest_file"
(cd "$out" && sha256sum "$manifest_file" > "$manifest_file.sha256")
printf '%s\t%s\n' "$out/$manifest_file" "$package/$manifest_file" \
  "$out/$manifest_file.sha256" "$package/$manifest_file.sha256" > "$uploads"
"$REPO/tools/upload_immutable_packages.sh" "$uploads"
rm -f "$uploads"
cat "$out/$manifest_file"
if [[ -n ${GITHUB_STEP_SUMMARY:-} ]]; then
  {
    echo "### tilo-vm:$DEV_TAG ($ARCH)"
    echo
    echo "Disk manifest: \`$package/$manifest_file\`"
    echo
    echo '| Arch | Format | Size | SHA-256 |'
    echo '| --- | --- | ---: | --- |'
    jq -r '.disks[] | "| \(.arch) | \(.format) | \(.size) | `\(.sha256)` |"' "$out/$manifest_file"
  } >> "$GITHUB_STEP_SUMMARY"
fi
note "published tilo-vm-disk $DEV_TAG for $ARCH"

# ---- 5. latest -----------------------------------------------------------------
# The only mutable name besides the growing index. It follows the newest
# revision's index and never moves backwards: the revision it names must be
# this one or an ancestor. The same convergence as the index applies: point it
# at the index as read now, and repeat until a fresh read agrees.
[[ $CHANNEL == true ]] || exit 0
latest="$REPOSITORY:latest"
revision_of() {
  local raw child
  raw=$(skopeo inspect --raw "docker://$1" 2>/dev/null) || return 0
  child=$(jq -r '.manifests[0].digest // empty' <<< "$raw")
  [[ -n $child ]] || return 0
  skopeo inspect "docker://$REPOSITORY@$child" | jq -r '.Labels["org.opencontainers.image.revision"] // empty'
}
is_ancestor() {
  # Shallow CI checkouts lack the history; fetch commits only, no trees.
  if [[ $(git -C "$REPO" rev-parse --is-shallow-repository) == true ]]; then
    git -C "$REPO" fetch --quiet --no-tags --filter=tree:0 --unshallow origin || true
  fi
  git -C "$REPO" merge-base --is-ancestor "$1" "$SOURCE_REVISION" 2>/dev/null
}
for attempt in 1 2 3 4 5 6; do
  target=$(manifest_digest "$index_tag")
  if [[ $(manifest_digest "$latest" 2>/dev/null || true) == "$target" ]]; then
    note "tilo-vm:latest -> $DEV_TAG ($target)"
    exit 0
  fi
  current=$(revision_of "$latest")
  if [[ -n $current && $current != "$SOURCE_REVISION" ]] && ! is_ancestor "$current"; then
    note "tilo-vm:latest names $current, which is not an ancestor of $SOURCE_REVISION; leaving it"
    exit 0
  fi
  # Byte-for-byte, as for the digest tag above: a re-encoded index has
  # another digest and never matches $target.
  skopeo copy --multi-arch index-only --preserve-digests \
    "docker://$REPOSITORY@$target" "docker://$latest"
  sleep $(( attempt * 5 ))
done
die "tilo-vm:latest did not converge on $index_tag"
