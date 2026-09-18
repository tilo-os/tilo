#!/bin/bash
# Prove the recovery story by booting it.
#
#   ./os/dev/composefs/recovery-series.sh [step ...]
#
#     entry     boot generation 1 and establish the repair entry exists, is
#               sealed with tilo.mode=repair, and is not the default
#     repair    boot INTO repair mode and drive its console over the serial line
#     rollback  stage a deliberately broken generation and show the machine
#               going back to the working one by itself
#
# With no arguments it runs all three, in that order, because each leaves the
# machine in the state the next one starts from.
#
# Expects generation 1 built and installed:
#     ./os/dev/composefs/build.sh 1 && ./os/dev/composefs/install.sh 1
#
# ## Test scope
#
# The boot chain is the production one: the same sealed UKI, the same
# systemd-boot, the same tilo-deployment, tilo-boot-health, tilo-boot-mode-generator
# and loader entries the desktop image ships (os/dev/composefs/build.sh copies
# them out of os/ rather than restating them).
#
# This image has no greetd, so tilo-boot-health uses
# tilo-probe-session.service to represent session readiness. Everything
# downstream of that verdict (boot-complete.target,
# systemd-bless-boot, the try counter, sd-boot's ordering) is untouched.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/common.sh"

VM="$HERE/vm.sh"
CONSOLE_DRIVER="$TILO_OS_DIR/dev/serial-console.py"
OUT="${TILO_RECOVERY_OUT:-$TILO_CFS_RUN/recovery}"
mkdir -p "$OUT"

fail=0
step()  { printf '\n\033[1;35m=== %s\033[0m\n' "$*" >&2; }
ok()    { printf '\033[1;32mok\033[0m    %s\n' "$*" >&2; }
bad()   { printf '\033[1;31mFAIL\033[0m  %s\n' "$*" >&2; fail=1; }

guest() { "$VM" ssh "$@"; }

# --- 1. the repair entry exists and is what it claims to be ------------------
proof_entry() {
  step "boot generation 1 and look at the boot menu it produced"
  "$VM" stop >/dev/null 2>&1 || true
  "$VM" boot 1 || { bad "generation 1 did not start"; return 1; }
  "$VM" wait  || { bad "generation 1 never reached ssh"; return 1; }

  # The boot-side check is the one that can see any of this. Its exit status is
  # the assertion — it fails rather than skips when it cannot observe.
  if guest "sudo /usr/libexec/tilo-recovery-boot-check" 2>&1 | tee "$OUT/01-entry.txt"; then
    ok "boot-side recovery check passed (transcript: $OUT/01-entry.txt)"
  else
    bad "boot-side recovery check FAILED; see $OUT/01-entry.txt"
  fi

  # And the artefacts themselves, so the transcript stands on its own.
  guest "sudo bootctl list; echo; sudo ls -l /boot/loader/entries /boot/EFI/Linux; echo; sudo bootc status" \
    > "$OUT/01-bootctl.txt" 2>&1
  guest "sudo /usr/libexec/tilo-uki-cmdline show /boot/EFI/Linux/tilo-repair.efi" \
    > "$OUT/01-repair-cmdline.txt" 2>&1
  if grep -q 'tilo\.mode=repair' "$OUT/01-repair-cmdline.txt"; then
    ok "the repair UKI's sealed command line says tilo.mode=repair"
  else
    bad "the repair UKI does not carry tilo.mode=repair; it would boot the desktop"
  fi
}

# --- 2. repair mode boots, authenticates, and can act ------------------------
proof_repair() {
  step "boot INTO repair mode and drive its console"
  "$VM" ssh "sudo bootctl set-oneshot tilo-repair.efi" > "$OUT/02-oneshot.txt" 2>&1 \
    || { bad "could not set the one-shot boot entry"; return 1; }
  ok "next boot pinned to tilo-repair.efi"
  "$VM" ssh "sudo systemctl poweroff" >/dev/null 2>&1
  sleep 8
  "$VM" stop >/dev/null 2>&1 || true

  # Repair mode masks sshd, so the serial line is the only input channel.
  "$VM" boot 1 --serial-socket || { bad "repair-mode boot did not start"; return 1; }
  local sock; sock="$("$VM" console)"
  for _ in $(seq 1 30); do [ -S "$sock" ] && break; sleep 1; done
  [ -S "$sock" ] || { bad "no serial socket at $sock"; return 1; }

  # The console is the evidence. Each `expect` fails the run if it never
  # arrives, so a pass means the expected text was received.
  python3 "$CONSOLE_DRIVER" "$sock" --log "$OUT/02-repair-console.log" \
    timeout:400 \
    expect:"tilo\\.mode=repair" \
    expect:"Reached target tilo-repair.target" \
    expect:"Tilo repair mode" \
    expect:"mode repair" \
    expect:"What would you like to do" \
    send:"6" \
    expect:"Boot loader entries" \
    expect:"What would you like to do" \
    send:"4" \
    expect:"remounted read-write|already read-write" \
    expect:"What would you like to do"
  local rc=$?
  if [ "$rc" = 0 ]; then
    ok "repair mode came up on the serial console and answered (log: $OUT/02-repair-console.log)"
  else
    bad "the repair console did not do what it should; see $OUT/02-repair-console.log"
  fi
  "$VM" stop >/dev/null 2>&1 || true
}

# --- 3. a broken generation rolls itself back --------------------------------
proof_rollback() {
  step "stage a generation whose session never comes up"
  if ! podman image exists "$TILO_CFS_IMAGE:gen2"; then
    bad "generation 2 is not built. Build the broken one with:
        TILO_CFS_SESSION_OK=0 ./os/dev/composefs/build.sh 2"
    return 1
  fi
  if ! podman container exists tilo-cfs-registry 2>/dev/null; then
    podman run -d --name tilo-cfs-registry -p "$TILO_CFS_REG_PORT:5000" \
      docker.io/library/registry:2 >/dev/null
  else
    podman start tilo-cfs-registry >/dev/null 2>&1 || true
  fi
  podman push --tls-verify=false "$TILO_CFS_IMAGE:gen2" \
    "$TILO_CFS_REG_HOST/$TILO_CFS_REPO:latest" >/dev/null 2>&1 \
    || { bad "could not publish generation 2"; return 1; }

  "$VM" stop >/dev/null 2>&1 || true
  "$VM" boot 1 || { bad "could not boot to stage the update"; return 1; }
  "$VM" wait  || { bad "no ssh to stage the update"; return 1; }
  local booted_before
  booted_before="$(guest 'tr " " "\n" < /proc/cmdline | sed -n "s/^composefs=//p"' 2>/dev/null | tr -d '\r')"

  guest "sudo bootc upgrade" > "$OUT/03-upgrade.txt" 2>&1
  # The staged entry has to carry a try counter BEFORE the reboot into it.
  guest "sudo /usr/libexec/tilo-deployment entries; sudo ls -l /boot/loader/entries" \
    > "$OUT/03-entries-after-stage.txt" 2>&1
  if grep -qE 'bootc.*\+[0-9]+\.conf' "$OUT/03-entries-after-stage.txt"; then
    ok "the staged deployment's loader entry carries a try counter"
  else
    bad "no try counter on the staged entry — a broken update would be retried
        for ever. See $OUT/03-entries-after-stage.txt"
  fi

  # Now let it fail. Each boot spends one try; after the third sd-boot stops
  # offering it first and the previous deployment wins.
  local i
  for i in 1 2 3 4; do
    step "boot attempt $i"
    "$VM" stop >/dev/null 2>&1 || true
    "$VM" boot 1 || { bad "boot $i did not start"; return 1; }
    # A broken generation never reaches ssh, so waiting for it is the wrong
    # test; watch the serial line instead and record what happened either way.
    local waited=0
    while [ "$waited" -lt 150 ]; do
      "$VM" ssh true >/dev/null 2>&1 && break
      sleep 5; waited=$((waited+5))
    done
    if "$VM" ssh true >/dev/null 2>&1; then
      guest 'echo "cmdline: $(cat /proc/cmdline)"; echo; sudo bootctl list; echo; sudo ls -l /boot/loader/entries; echo; sudo journalctl -b -u tilo-boot-health --no-pager' \
        > "$OUT/03-boot$i.txt" 2>&1
      local now
      now="$(guest 'tr " " "\n" < /proc/cmdline | sed -n "s/^composefs=//p"' 2>/dev/null | tr -d '\r')"
      if [ -n "$booted_before" ] && [ "$now" = "$booted_before" ] && [ "$i" -gt 1 ]; then
        ok "boot $i came up on the ORIGINAL deployment — the machine rolled itself back"
        cp -f "$OUT/03-boot$i.txt" "$OUT/03-rolled-back.txt"
        "$VM" stop >/dev/null 2>&1 || true
        return 0
      fi
      printf '      boot %s is on %.16s\n' "$i" "${now:-unknown}" >&2
    else
      "$VM" serial > "$OUT/03-boot$i-serial.txt" 2>&1 || true
      printf '      boot %s never reached ssh (serial in %s)\n' "$i" "$OUT/03-boot$i-serial.txt" >&2
    fi
  done
  bad "after 4 boots the machine had not gone back to the working deployment.
      Transcripts: $OUT/03-boot*.txt"
  "$VM" stop >/dev/null 2>&1 || true
}

steps=("$@")
[ ${#steps[@]} -gt 0 ] || steps=(entry repair rollback)
for s in "${steps[@]}"; do
  case "$s" in
    entry) proof_entry ;;
    repair) proof_repair ;;
    rollback) proof_rollback ;;
    *) bad "unknown step: $s"; ;;
  esac
done

printf '\n'
[ "$fail" = 0 ] && printf 'ALL PROOFS PASSED (evidence in %s)\n' "$OUT" \
                || printf 'SOME PROOFS FAILED (evidence in %s)\n' "$OUT" >&2
exit "$fail"
