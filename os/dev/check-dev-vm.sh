#!/usr/bin/env bash
# Prove a booted dev VM is usable by tilo-vm: its `status` step runs as the
# device's daemon starts it, and answers with the architecture and base the
# dev image was built for.
#
#   os/dev/check-dev-vm.sh --arch x86_64 [--base-digest sha256:...]
#
# Runs against the VM os/dev/vm.sh booted (same TILO_* environment). A step is
# a templated unit, tilo-device-<action>@<invocation>.service, reading its
# arguments from /run/tilo/device-steps/<invocation>/args.json and leaving its
# answer in result.json there.
set -euo pipefail

dev_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
arch=""
base_digest=""
while [ $# -gt 0 ]; do
  case "$1" in
    --arch) arch="$2"; shift ;;
    --base-digest) base_digest="$2"; shift ;;
    *) printf 'check-dev-vm: unknown arg: %s\n' "$1" >&2; exit 2 ;;
  esac
  shift
done
case "$arch" in x86_64|aarch64) ;; *) echo 'check-dev-vm: --arch x86_64|aarch64 is required' >&2; exit 2 ;; esac
[[ -z $base_digest || $base_digest =~ ^sha256:[0-9a-f]{64}$ ]] || {
  echo "check-dev-vm: --base-digest must be sha256:<64 hex>" >&2
  exit 2
}
pass="${TILO_PASS:-tilo}"

for attempt in $(seq 1 60); do
  if "$dev_dir/vm.sh" ssh true >/dev/null 2>&1; then
    break
  fi
  if [[ $attempt == 60 ]]; then
    "$dev_dir/vm.sh" serial | tail -120
    echo '::error::the dev VM booted but never accepted SSH' >&2
    exit 1
  fi
  sleep 2
done

# CI reaches the VM as the dev login account, which is in wheel. Validate its
# sudo once, then start the step as the daemon does.
step='set -e; i=$(tr -d - < /proc/sys/kernel/random/uuid); d=/run/tilo/device-steps/$i; mkdir -p "$d"; echo "{}" > "$d/args.json"; systemctl start "tilo-device-status@$i.service"; cat "$d/result.json"; rm -rf "$d"'
reply="$("$dev_dir/vm.sh" ssh "printf '%s\n' '$pass' | sudo -S -p '' -v && sudo -n sh -c '$step'")" || {
  "$dev_dir/vm.sh" ssh 'systemctl --no-pager --failed; journalctl -b --no-pager -n 80' || true
  echo '::error::the status step did not answer' >&2
  exit 1
}
# The result is {"ok": <the step's answer>} or {"err": <why it failed>}.
jq -e --arg arch "$arch" --arg base "$base_digest" '
  .ok.arch == $arch
  and ($base == "" or .ok.base.image_digest == $base)
' >/dev/null <<<"$reply" || {
  printf '%s\n' "$reply" >&2
  echo "::error::the status step answered, but not as a $arch dev VM${base_digest:+ built on $base_digest}" >&2
  exit 1
}
jq -r '.ok | "::notice::device steps \(.daemon_version) answer on \(.arch), base \(.base.image_digest // "unknown")\(if (.issues | length) > 0 then "; issues: \(.issues | map(.problem? // tostring) | join("; "))" else "" end)"' <<<"$reply"
