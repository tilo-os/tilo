#!/usr/bin/env bash
# Save the installed dev disk before boot validation. This package is a
# diagnostic checkpoint; the validated tilo-vm-disk manifest remains gated on
# reaching greetd and tilo-vmd in publish-dev-disk.sh.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
die() { printf '::error::%s\n' "$*" >&2; exit 1; }
# DEV_IMAGE_DIGEST_FILE instead of DEV_IMAGE_DIGEST: the image is still being
# pushed while the disk uploads; its push writes the digest there (or
# <file>.failed), and the manifest waits for it.
[[ -n ${DEV_IMAGE_DIGEST:-} || -z ${DEV_IMAGE_DIGEST_FILE:-} ]] || DEV_IMAGE_DIGEST=pending
for name in ARCH SOURCE_REVISION DISK_TOOLING_REVISION DEV_TAG DEV_IMAGE_DIGEST \
            BASE_CHILD_DIGEST BASE_TRUST SOURCE_DISK TILO_PACKAGE_TOKEN TILO_PACKAGE_PUBLIC_BASE_URL \
            GITHUB_RUN_ID GITHUB_RUN_ATTEMPT; do
  [[ -n ${!name:-} ]] || die "$name is required"
done
[[ $ARCH == x86_64 || $ARCH == aarch64 ]] || die "unsupported architecture: $ARCH"
[[ $SOURCE_REVISION =~ ^[0-9a-f]{40}$ && $DISK_TOOLING_REVISION =~ ^[0-9a-f]{40}$ ]] \
  || die 'source and tooling revisions must be full Git SHAs'
[[ ( $DEV_IMAGE_DIGEST == pending || $DEV_IMAGE_DIGEST =~ ^sha256:[0-9a-f]{64}$ ) && $BASE_CHILD_DIGEST =~ ^sha256:[0-9a-f]{64}$ ]] \
  || die 'image and base child digests must be sha256 values'
[[ $GITHUB_RUN_ID =~ ^[0-9]+$ && $GITHUB_RUN_ATTEMPT =~ ^[0-9]+$ ]] \
  || die 'run identity must be numeric'
[[ $DEV_TAG =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]{0,119}$ ]] || die 'invalid dev tag'
[[ $BASE_TRUST == release || $BASE_TRUST == integration ]] || die 'invalid base trust'
[[ ${TILO_PACKAGE_PUBLIC_BASE_URL%/} == https://*/api/packages/*/generic ]] \
  || die 'package base must be a public HTTPS generic-package URL'
test -s "$SOURCE_DISK" || die "no installed disk at $SOURCE_DISK"

# DISK_VARIANT (optional) tells apart disks one run publishes for the same
# architecture, such as one per board.
variant="${DISK_VARIANT:-}"
[[ -z $variant || $variant =~ ^[a-z0-9][a-z0-9-]{0,62}$ ]] || die 'DISK_VARIANT must be lowercase letters, digits and dashes'
version="${SOURCE_REVISION:0:12}-${DISK_TOOLING_REVISION:0:12}-${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}-${ARCH}${variant:+-$variant}"
package="${TILO_PACKAGE_PUBLIC_BASE_URL%/}/tilo-vm-disk-candidate/$version"
out=$(mktemp -d "${RUNNER_TEMP:-/var/tmp}/tilo-vm-disk-candidate.XXXXXX")
trap 'rm -rf -- "$out"' EXIT

disk="tilo-vm-candidate-$ARCH.qcow2"
# Upload the installed disk directly. Keeping a second qcow2 on the 100 GiB
# Kata runner exhausted its Docker overlay before the boot gate could run.
qemu-img check -f qcow2 "$SOURCE_DISK" >/dev/null
sha256sum "$SOURCE_DISK" | awk -v file="$disk" '{print $1 "  " file}' > "$out/$disk.sha256"
disk_sha=$(awk '{print $1}' "$out/$disk.sha256")
disk_bytes=$(stat -c %s "$SOURCE_DISK")
virtual_bytes=$(qemu-img info --output=json "$SOURCE_DISK" | jq -er '."virtual-size"')

files=$(jq -nc --arg file "$disk" --arg url "$package/$disk" \
  --arg sha "$disk_sha" --argjson size "$disk_bytes" --argjson virtual "$virtual_bytes" \
  '[{role:"disk",file:$file,url:$url,sha256:$sha,size:$size,virtualSize:$virtual}]')
if [[ $ARCH == aarch64 ]]; then
  boot=${SOURCE_BOOT:?SOURCE_BOOT is required for aarch64}
  for file in Image initrd boot.json; do
    test -s "$boot/$file" || die "missing direct boot file: $boot/$file"
    cp -- "$boot/$file" "$out/$file"
    (cd "$out" && sha256sum "$file" > "$file.sha256")
    files=$(jq -c --arg file "$file" --arg url "$package/$file" \
      --arg sha "$(awk '{print $1}' "$out/$file.sha256")" \
      --argjson size "$(stat -c %s "$out/$file")" \
      '. + [{role:"direct-boot",file:$file,url:$url,sha256:$sha,size:$size}]' <<< "$files")
  done
  jq -e '.kernel == "Image" and .initrd == "initrd"' "$out/boot.json" >/dev/null \
    || die 'direct boot bundle must refer to its matching Image and initrd'
fi

uploads="$out/uploads.tsv"
while read -r file; do
  if [[ $file == "$disk" ]]; then path=$SOURCE_DISK; else path="$out/$file"; fi
  printf '%s\t%s\n' "$path" "$package/$file" \
    "$out/$file.sha256" "$package/$file.sha256" >> "$uploads"
done < <(jq -r '.[].file' <<< "$files")
TILO_PACKAGE_UPLOAD_WORKERS=2 "$repo/tools/upload_immutable_packages.sh" "$uploads"
if [[ $DEV_IMAGE_DIGEST == pending ]]; then
  while [[ ! -s $DEV_IMAGE_DIGEST_FILE && ! -e $DEV_IMAGE_DIGEST_FILE.failed ]]; do sleep 2; done
  [[ ! -e $DEV_IMAGE_DIGEST_FILE.failed ]] || die 'the image push failed'
  DEV_IMAGE_DIGEST=$(tr -d '[:space:]' < "$DEV_IMAGE_DIGEST_FILE")
  [[ $DEV_IMAGE_DIGEST =~ ^sha256:[0-9a-f]{64}$ ]] || die 'the image push wrote no digest'
fi

# This explicit unvalidated schema cannot be mistaken for the validated
# https://tilo.sh/dev/disk/v1 manifest used by tilo-vm vm create --tag.
manifest=candidate.json
jq -n --arg tag "$DEV_TAG" --arg arch "$ARCH" --arg source "$SOURCE_REVISION" \
  --arg tooling "$DISK_TOOLING_REVISION" --arg image "$DEV_IMAGE_DIGEST" \
  --arg base "$BASE_CHILD_DIGEST" --arg trust "$BASE_TRUST" \
  --argjson files "$files" '{
    schema:"https://tilo.sh/dev/disk-candidate/v1", validation:"unvalidated",
    tag:$tag, arch:$arch, sourceRevision:$source, diskToolingRevision:$tooling,
    imageDigest:$image, baseChildDigest:$base, baseTrust:$trust, files:$files
  }' > "$out/$manifest"
(cd "$out" && sha256sum "$manifest" > "$manifest.sha256")
printf '%s\t%s\n' "$out/$manifest" "$package/$manifest" \
  "$out/$manifest.sha256" "$package/$manifest.sha256" > "$uploads"
"$repo/tools/upload_immutable_packages.sh" "$uploads"
echo "::notice::UNVALIDATED installed disk checkpoint: $package/$manifest"

# Main also keeps a durable copy per board and architecture, which candidate
# retention never prunes: tilo-vm-disk-latest/<arch>[-<variant>], replaced by
# each newer main disk and never by one from an older commit (main builds
# overlap). A failure here leaves the checkpoint above published.
publish_latest() {
  local target="${ARCH}${variant:+-$variant}"
  local latest="${TILO_PACKAGE_PUBLIC_BASE_URL%/}/tilo-vm-disk-latest/$target"
  local auth=(-H "Authorization: token $TILO_PACKAGE_TOKEN")
  local current ahead code
  current=$(curl -fsS "$latest/$manifest" 2>/dev/null | jq -r '.sourceRevision // empty' 2>/dev/null || true)
  if [[ -n $current && $current != "$SOURCE_REVISION" ]]; then
    ahead=$(curl -fsS "${auth[@]}" \
      "${GITHUB_SERVER_URL:?}/api/v1/repos/${GITHUB_REPOSITORY:?}/compare/$SOURCE_REVISION...$current" \
      | jq -er '.total_commits') || return 1
    if [[ $ahead != 0 ]]; then
      echo "::notice::$latest holds $current, which is not an ancestor of $SOURCE_REVISION; leaving it"
      return 0
    fi
  fi
  code=$(curl -sS -o /dev/null -w '%{http_code}' -X DELETE "${auth[@]}" "$latest")
  [[ $code == 204 || $code == 404 ]] || return 1
  mkdir -p "$out/latest"
  jq --arg base "$latest" '.files |= map(.url = ($base + "/" + .file))' "$out/$manifest" > "$out/latest/$manifest"
  (cd "$out/latest" && sha256sum "$manifest" > "$manifest.sha256")
  : > "$uploads"
  while read -r file; do
    if [[ $file == "$disk" ]]; then path=$SOURCE_DISK; else path="$out/$file"; fi
    printf '%s\t%s\n' "$path" "$latest/$file" "$out/$file.sha256" "$latest/$file.sha256" >> "$uploads"
  done < <(jq -r '.files[].file' "$out/$manifest")
  printf '%s\t%s\n' "$out/latest/$manifest" "$latest/$manifest" \
    "$out/latest/$manifest.sha256" "$latest/$manifest.sha256" >> "$uploads"
  TILO_PACKAGE_UPLOAD_WORKERS=2 "$repo/tools/upload_immutable_packages.sh" "$uploads" || return 1
  echo "::notice::durable disk for $target: $latest/$disk"
}
if [[ ${GITHUB_REF:-} == refs/heads/main ]]; then
  publish_latest || echo "::warning::could not update tilo-vm-disk-latest/${ARCH}${variant:+-$variant}"
fi
echo "::notice::disk SHA-256: $disk_sha; bytes: $disk_bytes; source: $SOURCE_REVISION; tooling: $DISK_TOOLING_REVISION"
if [[ -n ${GITHUB_STEP_SUMMARY:-} ]]; then
  {
    echo '### Unvalidated installed dev disk checkpoint'
    echo
    echo "Manifest: \`$package/$manifest\`"
    echo
    echo 'This disk is available for WHPX diagnostics. It is not the validated dev-disk channel.'
  } >> "$GITHUB_STEP_SUMMARY"
fi
