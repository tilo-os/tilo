#!/usr/bin/bash
# tilo-firstboot-user.service keeps NoNewPrivileges=yes, so the account tools
# it runs reach their own SELinux domains only through the nnp_transition
# rules in tilo-base.cil. Without them useradd, chpasswd and chage run in
# initrc_t (AVC nnp_transition, SELINUX_ERR security_bounded_transition).
set -euo pipefail
repo="$(cd "$(dirname "$0")/../.." && pwd)"
unit="$repo/os/usr/lib/systemd/system/tilo-firstboot-user.service"
policy="$repo/os/usr/share/tilo/selinux/tilo-base.cil"
script="$repo/os/usr/libexec/tilo-firstboot-user"

grep -qx 'NoNewPrivileges=yes' "$unit" || { echo "the unit no longer sets NoNewPrivileges" >&2; exit 1; }
for tool_domain in useradd:useradd_t chpasswd:passwd_t chage:passwd_t; do
  tool="${tool_domain%%:*}" domain="${tool_domain#*:}"
  grep -Eq "(^|[[:space:]|(])$tool[[:space:]]" "$script" || continue
  grep -qxF "(allow initrc_t $domain (process2 (nnp_transition nosuid_transition)))" "$policy" || {
    echo "tilo-firstboot-user runs $tool under NoNewPrivileges, but tilo-base.cil lets initrc_t not enter $domain" >&2
    exit 1
  }
done
echo 'ok: the first-boot account tools enter their own domains under NoNewPrivileges'
