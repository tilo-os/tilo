#!/usr/bin/env bash
# The rootless-overlay recovery must rebuild every packaged add-on SELinux policy
# plus qOS policy. Fedora policy packages commonly mask semodule failure in RPM
# scriptlets, so a successful dnf transaction alone does not establish this.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
containerfile="$here/../Containerfile"

fail() { printf 'selinux-policy-store: FAIL: %s\n' "$*" >&2; exit 1; }

line_of() {
  local needle="$1"
  grep -nF -- "$needle" "$containerfile" | head -1 | cut -d: -f1
}

copy_line="$(line_of 'cp -a /etc/selinux/targeted /var/tmp/qos-sestore')"
packaged_line="$(line_of 'semodule -n -s targeted -X 200 -i "$policy"')"
qos_line="$(line_of 'semodule -i /usr/share/qos/selinux/qos-base.cil')"

[ -n "$copy_line" ] && [ -n "$packaged_line" ] && [ -n "$qos_line" ] \
  || fail 'policy-store rebuild commands are missing'
[ "$copy_line" -lt "$packaged_line" ] && [ "$packaged_line" -lt "$qos_line" ] \
  || fail 'packaged policies must be reinstalled after copy-up and before qos-base'

archive_scans="$(grep -Fc "find /usr/share/selinux/packages -type f" "$containerfile")"
[ "$archive_scans" -eq 2 ] \
  || fail 'image build must reinstall and verify the same complete archive set'
grep -Fq 'module="${module%.bz2}"; module="${module%.pp}"' "$containerfile" \
  || fail 'image build does not derive each packaged policy module name'
grep -Fq "'\$1 == \"200\" && \$2 == module { found=1 } END { exit !found }'" "$containerfile" \
  || fail 'image build does not verify every packaged module is active'
grep -Fq "grep -Eq '^400[[:space:]]+qos-base[[:space:]]'" "$containerfile" \
  || fail 'image build does not verify the active qos-base module'

printf 'selinux-policy-store: complete packaged-policy and qos-base rebuild invariant holds\n'
