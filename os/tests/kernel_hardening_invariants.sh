#!/usr/bin/env bash
# Prevent the configured kernel/runtime hardening baseline from drifting away
# from the live boot audit. A setting that is only written to a drop-in is an
# intention, not evidence: an /etc override or unsupported kernel can defeat it.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
sysctl_file="$repo/os/usr/lib/sysctl.d/60-qos-hardening.conf"
audit="$repo/os/usr/libexec/qos-security-audit"
kernel_config="$repo/os/kernel/config-fragments/qos.config"
kargs="$repo/os/usr/lib/bootc/kargs.d/01-hardening.toml"
fail=0

bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }
ok() { printf 'ok    %s\n' "$*"; }

while read -r key expected; do
  if grep -Fqx "check_sysctl $key $expected" "$audit"; then
    ok "$key is checked at boot"
  else
    bad "$key = $expected is configured but not checked at boot"
  fi
done < <(awk '$1 !~ /^#/ && NF == 3 && $2 == "=" { print $1, $3 }' "$sysctl_file")

for required in \
  CONFIG_MODULE_SIG_FORCE=y \
  CONFIG_STRICT_KERNEL_RWX=y \
  CONFIG_STRICT_MODULE_RWX=y \
  CONFIG_STACKPROTECTOR_STRONG=y \
  CONFIG_FORTIFY_SOURCE=y \
  CONFIG_HARDENED_USERCOPY=y \
  CONFIG_SLAB_FREELIST_RANDOM=y \
  CONFIG_SLAB_FREELIST_HARDENED=y \
  CONFIG_SECURITY_LANDLOCK=y \
  CONFIG_SECCOMP_FILTER=y \
  CONFIG_SECURITY_YAMA=y \
  CONFIG_SECURITY_LOCKDOWN_LSM_EARLY=y \
  CONFIG_PROC_KCORE=n; do
  grep -Fqx "$required" "$kernel_config" \
    || bad "required kernel setting missing: $required"
done
[ "$fail" -ne 0 ] || ok 'required kernel self-protection options are pinned'

for required in \
  '"lockdown=integrity"' \
  '"init_on_alloc=1"' \
  '"init_on_free=1"' \
  '"slab_nomerge"' \
  '"page_alloc.shuffle=1"' \
  '"randomize_kstack_offset=on"' \
  '"vsyscall=none"'; do
  grep -Fq "$required" "$kargs" || bad "required kernel argument missing: $required"
done
[ "$fail" -ne 0 ] || ok 'required boot-time hardening arguments are pinned'

exit "$fail"
