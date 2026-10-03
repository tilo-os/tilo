#!/usr/bin/env bash
# tilo-security-audit counts a kernel option as compiled only when the config
# sets it (arm64 configs carry no line at all for x86-only or removed options),
# and reads packages from its unit's privileged snapshot.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
audit="$repo/os/usr/libexec/tilo-security-audit"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
sed -n '/^kconfig_off() {$/,/^}$/p' "$audit" > "$work/kconfig.sh"
grep -q 'kconfig_off' "$work/kconfig.sh"
# shellcheck disable=SC1091
. "$work/kconfig.sh"
printf '%s\n' 'CONFIG_DEBUG_FS=y' 'CONFIG_TLS=m' '# CONFIG_KEXEC is not set' > "$work/config"
running_kernel_config="$work/config"
kconfig_off KEXEC
kconfig_off X86_DECODER_SELFTEST
! kconfig_off DEBUG_FS
! kconfig_off TLS
running_kernel_config="$work/missing"
! kconfig_off KEXEC
# Package checks read the unit's privileged snapshot: rpm cannot run under the
# unit's NoNewPrivileges= (SELinux denies its domain transition).
sed -n '/^installed() {$/,/^}$/p' "$audit" > "$work/installed.sh"
# shellcheck disable=SC1091
. "$work/installed.sh"
printf '%s\n' gnutls-utils swtpm-tools > "$work/packages"
packages_file="$work/packages"
installed gnutls-utils
installed gnutls-utils swtpm-tools
! installed gnutls-utils nfs-utils
grep -Fq 'rpm -qa --qf "%%{NAME}\n" > /run/tilo-security-audit.packages' \
  "$repo/os/usr/lib/systemd/system/tilo-security-audit.service"
! grep -E '^[^#]*rpm -q [^a]' "$audit" | grep -v 'rpm -q "$package" >/dev/null 2>&1 || return 1'
# The active LSM list is comma-separated.
grep -Fq 'case ",$active_lsms," in' "$audit"
echo 'security audit kernel option checks: ok'
