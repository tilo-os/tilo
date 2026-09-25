#!/usr/bin/env bash
# Prove a booted dev VM is usable by tilo-vm: SSH answers, and tilo-vmd
# completes its handshake as root with the architecture and base the dev
# image was built for.
#
#   os/dev/check-dev-vm.sh --arch x86_64 [--base-digest sha256:...]
#
# Runs against the VM os/dev/vm.sh booted (same TILO_* environment). The
# handshake is what `tilo-vm` sends first over `ssh <vm> tilo-vmd rpc`
# (crates/dev/tilo-vm-protocol); `hello` never changes shape, so protocol 0
# is answered by any daemon with its own version.
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

# tilo-vm connects as root; CI reaches the VM as the dev login account, which
# is in wheel. Validate its sudo once, then run the daemon exactly as tilo-vm does.
hello='{"jsonrpc":"2.0","id":1,"method":"hello","params":{"protocol":0,"client_version":"check-dev-vm"}}'
reply="$("$dev_dir/vm.sh" ssh "printf '%s\n' '$pass' | sudo -S -p '' -v && printf '%s\n' '$hello' | sudo -n tilo-vmd rpc")" || {
  "$dev_dir/vm.sh" ssh 'systemctl --no-pager --failed; journalctl -b --no-pager -n 80' || true
  echo '::error::tilo-vmd rpc did not answer the handshake' >&2
  exit 1
}
printf '%s\n' "$reply" | head -1 | jq -e --arg arch "$arch" --arg base "$base_digest" '
  .id == 1
  and (.result.protocol | type == "number" and . > 0)
  and .result.arch == $arch
  and ($base == "" or .result.base.image_digest == $base)
' >/dev/null || {
  printf '%s\n' "$reply" >&2
  echo "::error::tilo-vmd answered, but not as a $arch dev VM${base_digest:+ built on $base_digest}" >&2
  exit 1
}
printf '%s\n' "$reply" | head -1 | jq -r '"::notice::tilo-vmd \(.result.daemon_version) (protocol \(.result.protocol)) answers on \(.result.arch), base \(.result.base.image_digest // "unknown")\(if (.result.issues | length) > 0 then "; issues: \(.result.issues | map(.problem? // tostring) | join("; "))" else "" end)"'
