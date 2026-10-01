#!/bin/bash
# Drive an update series across three generations and then roll back.
#
#   ./os/dev/composefs/update-series.sh
#
# bootc#2208 reports rollback breaking after a series of rebases and staying
# broken through reboots. Tilo binds rollback state to TPM-sealed LUKS keyslots,
# so a broken rollback is a machine that cannot unseal — which makes this the
# step most likely to force a rethink, not a formality to run at the end.
#
# The series is served from a plain-HTTP registry on the VM host. The guest
# reaches it at 10.0.2.2:5000 through user-mode networking, which is why the
# image carries an insecure-registry drop-in for exactly that address.
#
# Expects generations 2 and 3 to be built already (build.sh 2; build.sh 3) and
# generation 1 installed and bootable (install.sh 1).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/common.sh"

VM="$HERE/vm.sh"
LOG="$TILO_CFS_RUN/series.log"
: > "$LOG"

step() { printf '\n\033[1;35m=== %s\033[0m\n' "$*" | tee -a "$LOG" >&2; }
note() { printf '%s\n' "$*" | tee -a "$LOG" >&2; }

# --- the registry ------------------------------------------------------------
registry_up() {
  if ! podman container exists tilo-cfs-registry 2>/dev/null; then
    msg "starting a registry on :$TILO_CFS_REG_PORT"
    podman run -d --name tilo-cfs-registry \
      -p "$TILO_CFS_REG_PORT:5000" docker.io/library/registry:2 >/dev/null
  elif [ "$(podman inspect -f '{{.State.Running}}' tilo-cfs-registry)" != true ]; then
    podman start tilo-cfs-registry >/dev/null
  fi
  for _ in $(seq 1 30); do
    curl -sf "http://$TILO_CFS_REG_HOST/v2/" >/dev/null && return 0
    sleep 1
  done
  die "registry did not come up on $TILO_CFS_REG_HOST"
}

publish() {
  local gen="$1"
  msg "publishing generation $gen as $TILO_CFS_REG_HOST/$TILO_CFS_REPO:latest"
  podman push --tls-verify=false \
    "$TILO_CFS_IMAGE:gen$gen" "$TILO_CFS_REG_HOST/$TILO_CFS_REPO:latest"
}

guest() { "$VM" ssh "$@"; }

# What the guest reports about its own generation and rollback state, in one line.
snapshot() {
  guest 'set -e
    gen=$(tr " " "\n" < /proc/cmdline | sed -n "s/^tilo\.generation=//p" | head -1)
    cfs=$(tr " " "\n" < /proc/cmdline | sed -n "s/^composefs=//p" | head -1)
    pcr11=$(tpm2_pcrread sha256:11 2>/dev/null | sed -n "s/.*11 *: *0x\([0-9A-Fa-f]*\).*/\1/p")
    echo "booted_generation=${gen:-none}"
    echo "composefs=${cfs:0:16}"
    echo "pcr11=${pcr11}"
    echo "ukis=$(ls /boot/EFI/Linux/ 2>/dev/null | tr "\n" " ")"
    echo "deployments=$(ls /sysroot/state/deploy 2>/dev/null | wc -l)"
    bootc status 2>&1 | sed "s/^/  bootc| /"
  ' 2>&1 | tee -a "$LOG"
}

registry_up

step "generation 1: boot the freshly installed disk"
"$VM" boot 1
"$VM" wait
snapshot
note "--- boot-side evidence, generation 1 ---"
"$VM" ssh 'python3 -' < "$HERE/guest-check.py" 2>&1 | tee -a "$LOG" || \
  warn "boot-side checks reported failures on generation 1"

for gen in 2 3; do
  step "update to generation $gen"
  publish "$gen"
  # bootc#2334 is titled for the upgrade path, not the install path, so expect
  # the same digest mismatch here. The recovery is the same two passes: read the
  # digest the upgrade wants out of its own error, rebuild the UKI sealing it,
  # republish, retry. If the second attempt reports a *different* expected digest
  # again, the value depends on the UKI and the seal cannot be closed.
  if ! guest "bootc upgrade" 2>&1 | tee -a "$LOG.up$gen" | tee -a "$LOG"; then
    want="$(composefs_expected_digest "$LOG.up$gen")"
    if [ -z "$want" ]; then
      warn "bootc upgrade to generation $gen FAILED for another reason"; break
    fi
    note "upgrade wants sha512:${want:0:32}… — rebuilding generation $gen sealing it"
    TILO_CFS_DIGEST="$want" bash "$HERE/build.sh" "$gen" >>"$LOG" 2>&1
    publish "$gen"
    if ! guest "bootc upgrade" 2>&1 | tee -a "$LOG.up${gen}b" | tee -a "$LOG"; then
      want2="$(composefs_expected_digest "$LOG.up${gen}b")"
      if [ -n "$want2" ] && [ "$want2" != "$want" ]; then
        warn "the expected digest MOVED (${want:0:16}… -> ${want2:0:16}…): the seal is circular"
      fi
      warn "bootc upgrade to generation $gen FAILED after the second pass"; break
    fi
  fi
  note "--- staged state before reboot ---"
  guest 'bootc status' 2>&1 | tee -a "$LOG"
  guest 'ls -l /boot/EFI/Linux/ /boot/loader/entries 2>&1' 2>&1 | tee -a "$LOG"
  "$VM" reboot
  note "--- after reboot onto generation $gen ---"
  snapshot
  note "--- boot-side evidence, generation $gen ---"
  "$VM" ssh 'python3 -' < "$HERE/guest-check.py" 2>&1 | tee -a "$LOG" || \
    warn "boot-side checks reported failures on generation $gen"
done

step "roll back"
guest 'bootc rollback' 2>&1 | tee -a "$LOG" || warn "bootc rollback FAILED"
guest 'bootc status' 2>&1 | tee -a "$LOG"
"$VM" reboot
note "--- after rolling back ---"
snapshot

step "roll back again (does a second rollback still resolve?)"
guest 'bootc rollback' 2>&1 | tee -a "$LOG" || warn "second bootc rollback FAILED"
"$VM" reboot
note "--- after the second rollback ---"
snapshot

step "done — transcript in $LOG"
