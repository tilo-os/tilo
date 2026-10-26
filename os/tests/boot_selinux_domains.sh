#!/usr/bin/bash
# Two SELinux rules every boot depends on, in os/usr/share/tilo/selinux/tilo-base.cil:
#
# * bootc run by a Tilo service enters install_t. The deployment services are
#   scripts in unconfined_service_t, most under NoNewPrivileges; without the
#   transition every `bootc status` is denied mac_admin on its chcon probe
#   and re-executes a copy of itself from /tmp, and bootc never holds the
#   mac_admin it needs to write labels from a newer image's policy.
# * systemd-tpm2-setup.service (init_t) can write the TPM measurement log the
#   initrd starts. Without its own type the log is /run/log's
#   syslogd_var_run_t after the relabel, init_t may not write it, and the
#   unit fails on every boot of a measured UKI.
set -euo pipefail
repo="$(cd "$(dirname "$0")/../.." && pwd)"
policy="$repo/os/usr/share/tilo/selinux/tilo-base.cil"
fail=0
ok() { printf 'ok: %s\n' "$*"; }
bad() { printf 'FAIL: %s\n' "$*" >&2; fail=1; }
rule() { sed 's/;.*//' "$policy" | grep -qxF "$1"; }

# The services that run bootc, found from their ExecStart scripts.
bootc_units=()
for unit in "$repo"/os/usr/lib/systemd/system/*.service; do
  grep -q '^SELinuxContext=' "$unit" && continue
  while read -r exe; do
    script="$repo/os/${exe#/}"
    [ -f "$script" ] || continue
    if grep -v '^[[:space:]]*#' "$script" | grep -Eq '(^|[[:space:];|&(`$])bootc[[:space:]]'; then
      bootc_units+=("${unit##*/}")
      break
    fi
  done < <(sed -n 's/^Exec[A-Za-z]*=[-+!@:]*\([^ ]*\).*/\1/p' "$unit")
done
[ "${#bootc_units[@]}" -gt 0 ] || bad "found no service that runs bootc; the search above is broken"

if rule '(typetransition unconfined_service_t install_exec_t process install_t)' \
  && rule '(allow unconfined_service_t install_t (process (transition)))'; then
  ok "bootc enters install_t from the ${#bootc_units[@]} services that run it"
else
  bad "tilo-base.cil does not move bootc into install_t from unconfined_service_t (${bootc_units[*]})"
fi
nnp=()
for unit in "${bootc_units[@]}"; do
  grep -qx 'NoNewPrivileges=yes' "$repo/os/usr/lib/systemd/system/$unit" && nnp+=("$unit")
done
if [ "${#nnp[@]}" -eq 0 ] \
  || rule '(allow unconfined_service_t install_t (process2 (nnp_transition nosuid_transition)))'; then
  ok "bootc enters install_t under NoNewPrivileges too"
else
  bad "${nnp[*]} run bootc under NoNewPrivileges, but tilo-base.cil has no nnp_transition into install_t"
fi

log_type=tilo_tpm2_measure_log_t
if rule "(filecon \"/run/log/systemd/tpm2-measure\\.log\" file (system_u object_r $log_type ((s0) (s0))))" \
  && rule "(typetransition init_t syslogd_var_run_t file \"tpm2-measure.log\" $log_type)" \
  && sed 's/;.*//' "$policy" | grep -Eq "^\(allow init_t $log_type \(file \([^)]*\bwrite\b"; then
  ok "systemd-tpm2-setup (init_t) can write the TPM measurement log, however it was created"
else
  bad "tilo-base.cil does not give init_t the TPM measurement log; systemd-tpm2-setup.service fails"
fi

exit "$fail"
