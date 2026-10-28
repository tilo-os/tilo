#!/usr/bin/env bash
# Opt-in SELinux enforcing/audit proof for --dev-login installer media.
#
# Kept out of test-installer-cycle.sh so the release-gate script stays free of
# guest-shell and journal inspection channels (imager_release_invariants.sh).
set -euo pipefail

usage() {
  printf 'usage: installer-selinux-audit.sh require-host|assert-ready OUT|assert-clean OUT\n' >&2
  exit 2
}

die() {
  printf 'installer selinux audit: %s\n' "$*" >&2
  exit 1
}

guest_root() {
  local command_quoted
  [ -n "${TILO_INSTALLER_DEV_PASSWORD:-}" ] \
    || die 'TILO_INSTALLER_DEV_PASSWORD is required (build with --dev-login)'
  printf -v command_quoted '%q' "$1"
  # Password on stdin for sudo; sshpass owns the SSH password prompt.
  printf '%s\n' "$TILO_INSTALLER_DEV_PASSWORD" | \
    SSHPASS="$TILO_INSTALLER_DEV_PASSWORD" sshpass -e ssh \
      -p "${TILO_SSH_PORT:-2222}" \
      -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
      -o ConnectTimeout=5 -o LogLevel=ERROR \
      tester@127.0.0.1 "sudo -S -p '' /usr/bin/bash -ec $command_quoted"
}

cmd="${1:-}"
case "$cmd" in
  require-host)
    [ -n "${TILO_INSTALLER_DEV_PASSWORD:-}" ] \
      || die 'requires TILO_INSTALLER_DEV_PASSWORD and a --dev-login medium'
    command -v sshpass >/dev/null 2>&1 \
      || die 'requires sshpass on the VM host'
    ;;
  assert-ready)
    out="${2:-}"; [ -n "$out" ] || usage
    ready=0
    for _ in $(seq 1 60); do
      # shellcheck disable=SC2016 # Expanded by bash inside the VM, not the host.
      if guest_root '
      test "$(cat /sys/fs/selinux/enforce)" = 1
      test "$(getenforce)" = Enforcing
      systemctl is-active --quiet tilo-installer-setup.service
      semodule -l | grep -Eq "^tilo-installer([[:space:]]|$)"
      command -v auditctl >/dev/null
      command -v ausearch >/dev/null
      status="$(auditctl -s)"
      printf "%s\n" "$status" | grep -Eq "^enabled[[:space:]]+[12]$"
      printf "%s\n" "$status" | grep -Eq "^lost[[:space:]]+0$"
      ' >/dev/null 2>&1; then
        ready=1
        break
      fi
      sleep 2
    done
    [ "$ready" -eq 1 ] \
      || die 'live installer is not enforcing with an active, readable audit stream'
    printf 'selinux=Enforcing\ninstaller_policy=loaded\naudit=enabled\n' \
      > "$out/installer-selinux-proof.txt"
    ;;
  assert-clean)
    out="${2:-}"; [ -n "$out" ] || usage
    # shellcheck disable=SC2016 # Expanded by bash inside the VM, not the host.
    guest_root '
      test "$(cat /sys/fs/selinux/enforce)" = 1
      status="$(auditctl -s)"
      printf "%s\n" "$status" | grep -Eq "^enabled[[:space:]]+[12]$"
      printf "%s\n" "$status" | grep -Eq "^lost[[:space:]]+0$"
      records="$(ausearch -m AVC,USER_AVC -ts boot --raw 2>&1)" ||
        { test "$records" = "<no matches>" || exit 1; }
      ! printf "%s\n" "$records" | grep -Eq "type=(AVC|USER_AVC)"
      log="$(journalctl -b --no-pager -o cat)" || exit 1
      ! printf "%s\n" "$log" | grep -Eqi "type=(AVC|USER_AVC)|avc:[[:space:]]*denied"
    ' >/dev/null \
      || die 'live installer recorded an AVC, lost audit records, or audit inspection failed'
    printf 'avc=0\naudit_lost=0\n' >> "$out/installer-selinux-proof.txt"
    ;;
  *)
    usage
    ;;
esac
