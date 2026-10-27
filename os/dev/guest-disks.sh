#!/usr/bin/env bash
# The disks of a micro-VM step on t as the OS image's builds use them, as root in the guest
# (os/image/build-layer.sh, tools/ci/publish/os-image.sh). Sourced, not run.
#
# The step's scratch disk (the root's changes, /tmp, /var/tmp, podman's storage) outlives
# nothing: when the command ends, t's guest flushes only its output disk and resets the machine,
# so what is still in the guest's page cache then never reaches the node's storage. A build
# writes most of its bytes several times over, each copy short-lived: buildah writes every
# instruction's changes as a tar in TMPDIR, containers/image copies that tar to a file of its
# own, the store unpacks it, and then the container's copy and both tars are deleted; a pull
# keeps each compressed layer in a file beside the unpacked one until it is unpacked. With the
# kernel's defaults (a page is written once it has been dirty for 30 s, or once a tenth of memory
# is dirty) every one of those copies reached the node's disks: 90 to 180 GB for the OS image's
# top layer, on storage every other step shares.

# Keep written pages in memory, however long that takes, until a share of it is dirty that still
# leaves 3 GiB (and 30%) of the guest for everything else: a file deleted before then is never
# written, nor is what is still dirty when the step ends. Background writeback starts at 60% of
# that share. A dirty page is reclaimed by writing it, which the kernel does under memory
# pressure, but on storage at its throughput cap that is slow: the headroom keeps the build's
# own memory from waiting on it. Below 4 GiB the guest keeps the kernel's limits.
guest_writeback() {
  [[ -w /proc/sys/vm/dirty_bytes ]] || return 0
  local key value total=0 limit
  while read -r key value _; do
    [[ $key == MemTotal: ]] && total=$((value * 1024))
  done < /proc/meminfo
  limit=$((total * 7 / 10))
  ((limit < total - (3 << 30))) || limit=$((total - (3 << 30)))
  ((limit > total / 5)) || return 0
  echo "$limit" > /proc/sys/vm/dirty_bytes
  echo "$((limit * 6 / 10))" > /proc/sys/vm/dirty_background_bytes
  echo 8640000 > /proc/sys/vm/dirty_expire_centisecs
  printf 'TILO_GUEST_DISK writeback: dirty pages written from %s MiB, writers wait at %s MiB, of %s MiB\n' \
    "$((limit * 6 / 10 >> 20))" "$((limit >> 20))" "$((total >> 20))"
}

# The guest's memory every 2 s, into /run (its tmpfs, a few KiB an hour), for the peaks
# guest_disk_report gives per phase: in use without the page cache (MemTotal - MemAvailable) and
# with it (MemTotal - MemFree), which is what the node sees the guest touch.
guest_memory_samples=/run/tilo-guest-memory
guest_memory_watch() {
  : > "$guest_memory_samples"
  (
    trap - ERR
    set +e
    while :; do
      total=0 free=0 available=0
      while read -r key value _; do
        case $key in
          MemTotal:) total=$value ;;
          MemFree:) free=$value ;;
          MemAvailable:) available=$value ;;
        esac
      done < /proc/meminfo
      echo "$(((total - available) >> 10)) $(((total - free) >> 10))" >> "$guest_memory_samples"
      sleep 2
    done
  ) </dev/null >/dev/null 2>&1 &
}

# What the guest wrote to its scratch and output disks during the phase that just ended (MiB,
# as the node's storage receives it), so far in all, and what is dirty in its memory now:
#   TILO_GUEST_DISK phase=NAME scratch=+MiB output=+MiB scratch_total=MiB dirty=MiB
#     peak_used=MiB peak_with_cache=MiB
guest_disk_scratch_was=0 guest_disk_output_was=0 guest_memory_seen=0
guest_disk_report() {
  local b serial sectors scratch=0 output=0 dirty=0 key value used peak_used=0 peak_cached=0 n=0
  for b in /sys/block/vd*; do
    [[ -r $b/stat ]] || continue
    serial=$(cat "$b/serial" 2>/dev/null) || continue
    # The seventh field is the sectors written, of 512 bytes whatever the device's own size.
    read -r _ _ _ _ _ _ sectors _ < "$b/stat" || continue
    case $serial in
      scratch) scratch=$((sectors / 2048)) ;;
      output) output=$((sectors / 2048)) ;;
    esac
  done
  while read -r key value _; do
    [[ $key == Dirty: ]] && dirty=$((value / 1024))
  done < /proc/meminfo
  if [[ -r $guest_memory_samples ]]; then
    while read -r used value; do
      n=$((n + 1))
      ((n > guest_memory_seen)) || continue
      ((used <= peak_used)) || peak_used=$used
      ((value <= peak_cached)) || peak_cached=$value
    done < "$guest_memory_samples"
    guest_memory_seen=$n
  fi
  printf 'TILO_GUEST_DISK phase=%s scratch=+%s output=+%s scratch_total=%s dirty=%s peak_used=%s peak_with_cache=%s\n' "$1" \
    "$((scratch - guest_disk_scratch_was))" "$((output - guest_disk_output_was))" "$scratch" "$dirty" \
    "$peak_used" "$peak_cached"
  guest_disk_scratch_was=$scratch guest_disk_output_was=$output
}
