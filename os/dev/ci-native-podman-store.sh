#!/usr/bin/env bash
# Give this CI job's Podman a native overlay store on a disposable
# ext4 loop volume, and print the storage.conf that selects it.
#
#   export CONTAINERS_STORAGE_CONF="$(os/dev/ci-native-podman-store.sh)"
#
# The default nested store sits on the Kata job container's overlayfs, where
# Podman falls back to fuse-overlayfs: every layer commit then walks the whole
# multi-gigabyte parent. On a 1 GiB + 20,000-file probe a 5-byte child COPY
# took 2.69 s on the default store and 0.67 s here; a dev image build FROM the
# OS image spent about ten minutes per RUN commit on it. The sparse backing file
# and mount live for this job only; Kata teardown releases them.
#
# Needs the guest's loop devices (os/dev/ci-prepare-disk-devices.sh or the dev
# disk workflow's device step), passwordless sudo, mkfs.ext4 and jq. When
# GITHUB_ENV is set, later steps of the job use the store too.
set -euo pipefail

size=${TILO_PODMAN_STORE_SIZE:-80G}
scratch=${RUNNER_TEMP:-/var/tmp}
name="tilo-podman-${GITHUB_RUN_ID:-local}-${GITHUB_RUN_ATTEMPT:-0}"
backing="$scratch/$name.img"
root="$scratch/$name"

uid=$(id -u)
graph="$root/graph"
# disk-image.sh binds this parent at /var/lib/containers in the bootc container.
# Use its existing layout so both Podman invocations see the same image layers.
if [[ $uid == 0 ]]; then
  graph="$root/containers/storage"
fi

if ! [[ -s $root/storage.conf ]] || ! mountpoint -q "$root"; then
  truncate -s "$size" "$backing"
  sudo -n mkfs.ext4 -F -q -i 32768 "$backing" >&2
  mkdir -p "$root"
  loop=$(sudo -n losetup --find --show "$backing")
  sudo -n mount -t ext4 "$loop" "$root"
  sudo -n chown "$uid:$(id -g)" "$root"
  printf '[storage]\ndriver="overlay"\ngraphroot="%s"\nrunroot="%s/run"\n' \
    "$graph" "$root" > "$root/storage.conf"
fi
export CONTAINERS_STORAGE_CONF="$root/storage.conf"
podman info --format json | jq -e --arg graph "$graph" '
  .store.graphDriverName == "overlay"
  and .store.graphRoot == $graph
  and .store.graphStatus["Backing Filesystem"] == "extfs"
' >/dev/null || {
  echo '::error::Podman did not adopt the ext4 overlay store' >&2
  exit 1
}
test "$(cat "$graph/overlay/.has-mount-program")" = false || {
  echo '::error::Podman did not select native overlay on the ext4 volume' >&2
  exit 1
}
if [[ -n ${GITHUB_ENV:-} ]]; then
  echo "CONTAINERS_STORAGE_CONF=$CONTAINERS_STORAGE_CONF" >> "$GITHUB_ENV"
  # Only root may reuse this store across sudo. Preserve explicit caller
  # choices, and never expose a rootless user's image store to privileged Podman.
  if [[ $uid == 0 && -z ${TILO_ROOT_PODMAN_STORE:-} && -z ${TILO_ROOT_PODMAN_STORAGE_CONF:-} ]]; then
    {
      echo "TILO_ROOT_PODMAN_STORE=$root"
      echo "TILO_ROOT_PODMAN_STORAGE_CONF=$CONTAINERS_STORAGE_CONF"
    } >> "$GITHUB_ENV"
  fi
fi
echo "::notice::Podman uses native overlay on a disposable ext4 loop volume ($size)" >&2
printf '%s\n' "$CONTAINERS_STORAGE_CONF"
