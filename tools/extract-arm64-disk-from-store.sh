#!/usr/bin/env bash
set -Eeuo pipefail

store=${1:-/mnt/c/tilo-build/output-store.raw}
destination=${2:-/mnt/c/tilo-build/local/disk.raw}
expected_logical_bytes=${TILO_DISK_BYTES:-68719476736}
reserve_bytes=${TILO_HOST_RESERVE_BYTES:-8589934592}
mount_dir=$(mktemp -d /tmp/tilo-output-ro.XXXXXX)
temporary="${destination}.incoming.$$"
extent_manifest="${destination}.allocated-extents.incoming.$$"
loop=

cleanup() {
  status=$?
  if mountpoint -q "$mount_dir"; then
    sudo umount "$mount_dir" || status=$?
  fi
  if [[ -n "$loop" ]] && sudo losetup "$loop" >/dev/null 2>&1; then
    # Clear the block-device flag before detaching so a later, unrelated loop
    # association does not inherit read-only state. The filesystem is already
    # unmounted and the loop backing file itself remains opened read-only.
    sudo blockdev --setrw "$loop" 2>/dev/null || true
    sudo losetup -d "$loop" || status=$?
  fi
  rmdir "$mount_dir" 2>/dev/null || true
  if (( status != 0 )); then
    rm -f -- "$temporary" "$extent_manifest"
  fi
  exit "$status"
}
trap cleanup EXIT INT TERM

for tool in losetup blockdev e2fsck mountpoint mount sgdisk partx blkid cp sha256sum filefrag dd; do
  command -v "$tool" >/dev/null || {
    echo "required tool is missing: $tool" >&2
    exit 1
  }
done
[[ -f "$store" ]] || { echo "missing output store: $store" >&2; exit 1; }
mkdir -p "$(dirname "$destination")"

# The creator must be stopped first. ro,noload prevents journal replay, so this
# handoff cannot mutate the retained output/cache filesystem.
loop=$(sudo losetup --find --show --read-only "$store")
sudo blockdev --setro "$loop"
sudo e2fsck -fn "$loop"
sudo mount -o ro,noload "$loop" "$mount_dir"

source_disk="$mount_dir/disk.raw"
[[ -f "$source_disk" ]] || { echo "creator output is missing disk.raw" >&2; exit 1; }

logical_bytes=$(stat -c %s "$source_disk")
allocated_bytes=$(( $(stat -c %b "$source_disk") * 512 ))
(( logical_bytes == expected_logical_bytes )) || {
  echo "unexpected disk size: $logical_bytes (expected $expected_logical_bytes)" >&2
  exit 1
}
(( allocated_bytes > 0 && allocated_bytes < logical_bytes )) || {
  echo "source is empty or unexpectedly dense: allocated=$allocated_bytes logical=$logical_bytes" >&2
  exit 1
}

sudo sgdisk -v "$source_disk"
sudo sgdisk -p "$source_disk"

mapfile -t partitions < <(sudo partx --raw --noheadings --output NR,START,SECTORS "$source_disk")
(( ${#partitions[@]} == 2 )) || {
  echo "unexpected partition count: ${#partitions[@]} (expected 2)" >&2
  exit 1
}
partition_types=()
for partition in "${partitions[@]}"; do
  read -r number start sectors <<<"$partition"
  [[ "$number" =~ ^[0-9]+$ && "$start" =~ ^[0-9]+$ && "$sectors" =~ ^[0-9]+$ ]]
  type=$(sudo blkid -p -o value -s TYPE -O "$((start * 512))" -S "$((sectors * 512))" "$source_disk")
  partition_types+=("$type")
done
[[ "${partition_types[0]}" == vfat ]] || {
  echo "partition 1 has ${partition_types[0]:-no} filesystem; expected vfat" >&2
  exit 1
}
[[ "${partition_types[1]}" == btrfs ]] || {
  echo "partition 2 has ${partition_types[1]:-no} filesystem; expected btrfs" >&2
  exit 1
}

# The WSL VHD grows on C:. Preserve enough physical headroom for the overlay,
# UEFI state and Windows even when WSL reports much more virtual free space.
host_free_bytes=$(df -B1 --output=avail /mnt/c | awk 'NR == 2 { print $1 }')
required_host_bytes=$(( allocated_bytes + reserve_bytes ))
(( host_free_bytes >= required_host_bytes )) || {
  echo "insufficient C: headroom: free=$host_free_bytes required=$required_host_bytes (disk allocation plus reserve)" >&2
  exit 1
}

# Prefer a native Windows sparse destination when the caller supplies the
# native copier. The source stays on this read-only mount for the entire
# conversion. qemu-img reads it through WSL's UNC server, writes straight to
# NTFS, and performs a full byte comparison before atomic promotion. This
# avoids both WSL VHD growth and the NBD/backing-chain path that returned zero
# blocks during local boot.
if [[ -n ${TILO_NATIVE_COPY_SCRIPT:-} ]]; then
  [[ "$destination" == /mnt/?/* ]] || {
    echo 'native handoff destination must be on a Windows drive mounted under /mnt/<drive>' >&2
    exit 1
  }
  [[ -f "$TILO_NATIVE_COPY_SCRIPT" ]] || {
    echo "native sparse copier is missing: $TILO_NATIVE_COPY_SCRIPT" >&2
    exit 1
  }
  command -v powershell.exe >/dev/null || {
    echo 'powershell.exe is unavailable through WSL interop' >&2
    exit 1
  }
  source_windows=$(wslpath -w "$source_disk")
  destination_windows=$(wslpath -w "$destination")
  copier_windows=$(wslpath -w "$TILO_NATIVE_COPY_SCRIPT")
  native_args=(-NoProfile -ExecutionPolicy Bypass -File "$copier_windows"
    -SourcePath "$source_windows" -DestinationPath "$destination_windows"
    -ExpectedBytes "$expected_logical_bytes")
  if [[ -n ${TILO_QEMU_ROOT_WINDOWS:-} ]]; then
    native_args+=(-QemuRoot "$TILO_QEMU_ROOT_WINDOWS")
  fi
  if [[ ${TILO_REPLACE_DESTINATION:-0} == 1 ]]; then
    native_args+=(-Replace)
  fi
  powershell.exe "${native_args[@]}"
  sudo sgdisk -v "$destination"
  printf 'READY disk=%s filesystems=vfat,btrfs transport=native-qemu-img logical_bytes=%s host_free_before=%s\n' \
    "$destination" "$logical_bytes" "$host_free_bytes"
  exit 0
fi

rm -f -- "$temporary"
sudo filefrag -e -b1 "$source_disk" | awk '
  /^[[:space:]]*[0-9]+:/ {
    start=$2; span=$6
    sub(/\.\.$/, "", start); sub(/:$/, "", span)
    if (start ~ /^[0-9]+$/ && span ~ /^[0-9]+$/ && span > 0) print start, span
  }
' >"$extent_manifest"
[[ -s "$extent_manifest" ]] || { echo "could not map allocated source extents" >&2; exit 1; }

sudo cp --reflink=auto --sparse=always --preserve=timestamps -- "$source_disk" "$temporary"
sudo chown "$(id -u):$(id -g)" "$temporary"
sync "$temporary"

hash_allocated_extents() {
  local file=$1 offset length
  while read -r offset length; do
    sudo dd if="$file" iflag=skip_bytes,count_bytes skip="$offset" count="$length" bs=4M status=none
  done <"$extent_manifest" | sha256sum | awk '{ print $1 }'
}
source_extent_hash=$(hash_allocated_extents "$source_disk")
copy_extent_hash=$(hash_allocated_extents "$temporary")
[[ "$copy_extent_hash" == "$source_extent_hash" ]] || {
  echo "allocated data mismatch: source=$source_extent_hash copy=$copy_extent_hash" >&2
  exit 1
}
sudo sgdisk -v "$temporary"

copied_logical=$(stat -c %s "$temporary")
copied_allocated=$(( $(stat -c %b "$temporary") * 512 ))
(( copied_logical == logical_bytes ))
(( copied_allocated > 0 && copied_allocated < copied_logical )) || {
  echo "copy is empty or unexpectedly dense: allocated=$copied_allocated logical=$copied_logical" >&2
  exit 1
}

mv -f -- "$temporary" "$destination"
mv -f -- "$extent_manifest" "${destination}.allocated-extents"
printf '%s  %s allocated extents\n' "$copy_extent_hash" "$(basename "$destination")" >"${destination}.allocated.sha256"
sync "$destination" "${destination}.allocated-extents" "${destination}.allocated.sha256"

printf 'READY disk=%s filesystems=vfat,btrfs allocated_sha256=%s logical_bytes=%s allocated_bytes=%s host_free_before=%s\n' \
  "$destination" "$copy_extent_hash" "$copied_logical" "$copied_allocated" "$host_free_bytes"
