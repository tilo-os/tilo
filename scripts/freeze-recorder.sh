#!/usr/bin/env bash
# Watch GPU memory, name what is eating it, and stop the run before the
# machine dies of it.
#
# What the last freeze looked like, sampled every 500ms and fsynced, so
# these numbers survived the power button:
#
#   01:46:38  GTT 12357 MB   VRAM 507/512   avail 9892 MB
#   01:47:17  GTT 15643 MB   VRAM 503/512   avail 6419 MB   <- last line
#
# 3.2 GB of GTT in 39 seconds -- about 84 MB/s -- and then a plateau at
# 15643 MB where it sat until the machine stopped. System RAM was fine
# (6.4 GB available) and swap was untouched, so this is not the machine
# running out of memory. It is the *GPU* running out: GTT is system RAM
# mapped for the GPU, the iGPU's window onto it is 16404 MB, and the
# plateau is that window being full. An allocation that cannot be served
# and cannot fail takes the box with it, which is why nothing was ever
# written down -- no GPU reset, no hung task, no OOM, no panic, three
# times running.
#
# So this does two things a passive log cannot:
#
#   1. Names the processes holding GTT, from DRM fdinfo, so the leak has
#      an owner instead of a rate. `drm-memory-gtt` is per open file, so
#      the numbers are summed per pid.
#   2. Stops the demo scope when GTT crosses a line, while there is still
#      room to act. A run that ends early is an annoyance; a run that
#      ends in a reboot has cost this evening four of them.
#
# Usage:
#   scripts/freeze-recorder.sh &
#   cat /var/home/quinten/qos-freeze.log
#
#   QOS_FREEZE_LIMIT_MB=13000   where to intervene (0 disables)

OUT="${1:-/var/home/quinten/qos-freeze.log}"
CARD=/sys/class/drm/card1/device
DGPU=/sys/bus/pci/devices/0000:01:00.0/power
LIMIT_MB="${QOS_FREEZE_LIMIT_MB:-13000}"

{
  echo "# ---- recorder started $(date -Is), abort above ${LIMIT_MB}MB GTT ----"
  echo "# time vram_mb gtt_mb avail_mb swap_mb procs  top GTT holders"
} >> "$OUT"

mb() { echo $(( ${1:-0} / 1048576 )); }

# Who is holding GTT, biggest first. Summed per pid because
# `drm-memory-gtt` appears once per open DRM file.
holders() {
  for p in $(pgrep -d' ' -x 'shell|chat|photos|file-explorer|word-processor|qos-agent|shots' 2>/dev/null); do
    [ -d "/proc/$p" ] || continue
    kb=$(awk '/^drm-memory-gtt:/ {gsub(/[^0-9]/,"",$2); s+=$2} END {print s+0}' \
         /proc/"$p"/fdinfo/* 2>/dev/null)
    [ "${kb:-0}" -gt 0 ] 2>/dev/null || continue
    echo "$((kb/1024))MB:$(cat /proc/"$p"/comm 2>/dev/null)($p)"
  done | sort -rn | head -4 | tr '\n' ' '
}

tripped=0
while true; do
  vu=$(mb "$(cat $CARD/mem_info_vram_used 2>/dev/null || echo 0)")
  gu=$(mb "$(cat $CARD/mem_info_gtt_used 2>/dev/null || echo 0)")
  ma=$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo 2>/dev/null)
  sw=$(awk '/SwapTotal/{t=$2} /SwapFree/{f=$2} END{print int((t-f)/1024)}' /proc/meminfo 2>/dev/null)
  n=$(ps -eo comm --no-headers 2>/dev/null \
      | grep -cE '^(shell|chat|photos|file-explorer|word-processor|qos-agent)$')

  printf '%s %s %s %s %s %s  %s\n' \
    "$(date +%H:%M:%S)" "$vu" "$gu" "$ma" "$sw" "$n" "$(holders)" \
    | dd of="$OUT" oflag=append conv=notrunc,fsync status=none 2>/dev/null

  # The valve. Stopping the scope takes the whole cgroup, which is the
  # only thing that reliably releases GPU memory here -- the leak is not
  # being given back by whatever holds it, so the holder has to go.
  if [ "$LIMIT_MB" -gt 0 ] && [ "${gu:-0}" -gt "$LIMIT_MB" ] && [ "$tripped" -eq 0 ]; then
    tripped=1
    {
      echo "# !! GTT ${gu}MB over ${LIMIT_MB}MB -- stopping the run to save the machine"
      echo "# !! holders: $(holders)"
    } | dd of="$OUT" oflag=append conv=notrunc,fsync status=none 2>/dev/null
    systemctl --user stop "$(systemctl --user list-units --all --no-pager 2>/dev/null \
      | awk '/qos-demo-[0-9]+\.scope/ {print $1; exit}')" 2>/dev/null
    notify-send -u critical "qOS demo stopped" \
      "GPU memory hit ${gu}MB; the run was stopped before the machine froze." 2>/dev/null
  fi
  [ "${gu:-0}" -lt $(( LIMIT_MB - 2000 )) ] && tripped=0

  sleep 0.5
done
