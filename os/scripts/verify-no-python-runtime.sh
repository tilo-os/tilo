#!/usr/bin/env bash
# Runtime code in the image is compiled. The finished rootfs ships no Python
# program of Tilo's own and no Tilo file that runs one; the one Python program
# Tilo ships on purpose is umu-launcher, through which Proton runs Windows
# games. python3 is installed for umu-launcher and for the distribution
# packages listed below, and for nothing else. Run inside the finished rootfs
# after the last package change.
set -euo pipefail
rpm_bin="${TILO_RPM_BIN:-rpm}"
root="${TILO_ROOTFS:-/}"
root="${root%/}"

# Python programs shipped outside any RPM, by path, then why.
shipped_exceptions='/usr/libexec/tilo-umu-run umu-launcher, the Proton runtime launcher (a Python zipapp); no compiled equivalent exists
/usr/libexec/umu_run.py umu-launcher, the module tilo-umu-run imports'

# Distribution packages that require Python, then why. An entry is a
# replacement still owed or a reviewed distribution dependency, not a licence
# for Tilo code: python3 must not become a dependency of anything else.
package_exceptions='python3 the interpreter, for umu-launcher
python3-libs the interpreter, for umu-launcher
python-unversioned-command the interpreter, for umu-launcher
firewalld the system firewall: firewalld, firewall-cmd and firewall-offline-cmd are Python
crypto-policies-scripts update-crypto-policies is Python
policycoreutils-python-utils semanage, audit2allow and chcat (SELinux policy management) are Python
power-profiles-daemon its powerprofilesctl client is Python
at-spi2-core declares python(abi) for the Python bindings it carries
flatpak its flatpak-bisect and flatpak-coredumpctl tools are Python
libwacom its libwacom-update-db tool is Python
gstreamer1 its gst-plugins-doc-cache-generator helper is Python
samba-common-tools its samba-log-parser tool is Python
sos the support-report tool the base ships, kept on purpose (os/Containerfile), is Python
libcap-ng-python3 Python bindings required by the packages above
systemd-ukify ukify, the UKI builder, is Python'

fail=0
requirements="$(mktemp)"
owned="$(mktemp)"
present="$(mktemp)"
trap 'rm -f "$requirements" "$owned" "$present"' EXIT

# `=NAME` repeats the scalar package name inside REQUIRENAME's array iterator.
"$rpm_bin" -qa --qf '[%{=NAME}\t%{REQUIRENAME}\n]' >"$requirements"
consumers="$(awk -F '\t' '$2 ~ /^(python\(abi\)|python3(\.[0-9]+)?(dist\(.*\))?|python3-libs|\/usr\/bin\/python3?(\.[0-9]+)?)( |$)/ { print $1 }' \
  "$requirements" | sort -u)"
# Python libraries (python3-*) are only ever there as the dependencies of a
# consumer; the consumers are what needs reviewing.
consumers="$(grep -Ev '^python3-' <<<"$consumers" || true)"
allowed_packages="$(awk 'NF { print $1 }' <<<"$package_exceptions")"
unexpected_packages="$(grep -Fvx -f <(printf '%s\n' "$allowed_packages") <<<"$consumers" || true)"
if [ -n "$unexpected_packages" ]; then
  echo 'packages outside the reviewed list depend on Python:' >&2
  printf '%s\n' "$unexpected_packages" >&2
  fail=1
fi

# Every file RPM does not own is Tilo's (or the build's) own.
"$rpm_bin" -qal | sed -n 's|^/|/|p' | LC_ALL=C sort -u >"$owned"
# /usr/local holds the build's own tools, this script among them; the
# image build removes them after its checks.
( cd "$root/" && find usr etc -xdev -path usr/local -prune -o -type f -printf '/%p\n' 2>/dev/null ) \
  | LC_ALL=C sort -u >"$present"
allowed_files="$(awk 'NF { print $1 }' <<<"$shipped_exceptions")"
python_files=()
while IFS= read -r path; do
  [ -n "$path" ] || continue
  grep -Fqx -- "$path" <<<"$allowed_files" && continue
  file="$root$path"
  case "$path" in
    *.pyc) continue ;;
    *.py) python_files+=("$path: a Python source file"); continue ;;
  esac
  first="$(head -c 256 -- "$file" 2>/dev/null | head -n 1 | tr -d '\0' || true)"
  if [[ $first =~ ^#![[:space:]]*([^[:space:]]*/)?(env[[:space:]]+(-[^[:space:]]*[[:space:]]+)*)?python ]]; then
    python_files+=("$path: a Python program")
    continue
  fi
  # A shell script that runs Python: any line, comments aside, naming the
  # interpreter as a command word.
  if [[ $first == '#!'* ]] && grep -Eq '^[^#]*(^|[[:space:];|&(`$]|/)python(3(\.[0-9]+)?)?([[:space:];|&)`]|$)' -- "$file"; then
    python_files+=("$path: a script that runs Python")
  fi
done < <(LC_ALL=C comm -23 "$present" "$owned")
if [ "${#python_files[@]}" -gt 0 ]; then
  echo 'files outside any RPM ship or run Python:' >&2
  printf '%s\n' "${python_files[@]}" >&2
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  exit 1
fi
echo 'no Python on the runtime path beyond umu-launcher and the reviewed packages'
