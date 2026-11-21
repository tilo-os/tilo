#!/usr/bin/env bash
# The units that have bootc open its image store keep a sandbox bootc can run in.
#
# tilo-os-image-gc.service runs `bootc internals composefs-gc --prune-repo`
# through `tilo os clean --only os-images`, and tilo-deployment-update.service
# runs `bootc switch` / `bootc upgrade` through tilo-deployment, so both inherit
# their unit's sandbox. One directive there used to make them impossible:
# RestrictSUIDSGID= makes systemd deny a set of syscalls outright, openat2()
# among them (seccomp cannot inspect its arguments, so the whole call goes),
# and bootc's containers/image resolves every path through openat2() and does
# not fall back. The collection then failed at every run with
#
#   Pruning: Listing images: Loading global authfile: Opening runtime auth
#   file: Function not implemented (os error 38)
#
# even when it had nothing to remove, and the update failed the same way before
# it reached the registry, so a computer never fetched a newer image by itself.
#
# This test reads each unit's own sandbox directives, puts a process under
# exactly those properties, and requires that openat2() still works; for the
# collection also that its command exits 0. It needs a systemd manager; without
# one it still holds the units to the directives bootc needs.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
units="$repo/os/usr/lib/systemd/system"
unit="$units/tilo-os-image-gc.service"
update_unit="$units/tilo-deployment-update.service"
fail=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

# The units must be shipped whole: a directive the test cannot see is a
# directive it cannot hold to anything.
for shipped in "$unit" "$update_unit"; do
  [ -r "$shipped" ] || { bad "missing $shipped"; exit 1; }
done

# RestrictSUIDSGID= is the one directive a process that calls openat2() cannot
# live with, and every bootc fetch and composefs collection does. It must not be
# set (unset, or explicitly no) rather than merely absent, so that a later edit
# either keeps the reason visible or has to re-read this test.
for shipped in "$unit" "$update_unit"; do
  name="$(basename "$shipped")"
  if grep -Eq '^RestrictSUIDSGID=(yes|true)$' "$shipped"; then
    bad "$name sets RestrictSUIDSGID=; systemd's filter for it denies openat2(), and bootc dies with ENOSYS"
  else
    ok "$name is not put under RestrictSUIDSGID="
  fi
done
# The rest of the sandbox stays: it is what keeps a root cleanup bounded.
for required in \
  'ProtectHome=yes' \
  'PrivateTmp=yes' \
  'PrivateNetwork=yes' \
  'NoNewPrivileges=yes' \
  'ProtectKernelModules=yes' \
  'ProtectKernelLogs=yes' \
  'ProtectControlGroups=yes' \
  'ProtectClock=yes' \
  'ProtectHostname=yes' \
  'LockPersonality=yes' \
  'MemoryDenyWriteExecute=yes' \
  'RestrictRealtime=yes' \
  'SystemCallArchitectures=native'; do
  grep -Fqx "$required" "$unit" || bad "the collection sandbox lost $required"
done
[ "$fail" -ne 0 ] || ok "the rest of the collection sandbox is intact"

# A systemd manager to run the properties under, system or user. Both apply
# the same sandbox directives; neither changes the machine.
runner=()
if systemd-run --quiet --wait --collect /usr/bin/true >/dev/null 2>&1; then
  runner=(systemd-run --quiet --wait --collect --pipe)
elif systemd-run --user --quiet --wait --collect /usr/bin/true >/dev/null 2>&1; then
  runner=(systemd-run --user --quiet --wait --collect --pipe)
fi
if [ "${#runner[@]}" -eq 0 ]; then
  printf 'SKIP  no usable systemd manager to run the unit sandbox\n'
  [ "$fail" -eq 0 ] || exit 1
  exit 0
fi

# The unit's sandbox directives, as properties. Everything in the [Service]
# section that changes what the process may do; scheduling, timeouts and paths
# do not, and a StateDirectory= would need the manager to own it.
sandbox_of() {  # $1 = unit file; fills `properties`
  properties=()
  while IFS= read -r line; do
    case "$line" in
      ProtectHome=*|PrivateTmp=*|PrivateNetwork=*|NoNewPrivileges=*|ProtectKernelModules=*|\
      ProtectKernelLogs=*|ProtectControlGroups=*|ProtectClock=*|ProtectHostname=*|\
      LockPersonality=*|MemoryDenyWriteExecute=*|RestrictRealtime=*|RestrictSUIDSGID=*|\
      RestrictNamespaces=*|RestrictAddressFamilies=*|SystemCallArchitectures=*|\
      SystemCallFilter=*|SystemCallErrorNumber=*|CapabilityBoundingSet=*|AmbientCapabilities=*)
        properties+=("--property=$line") ;;
    esac
  done < <(sed -n '/^\[Service\]/,$p' "$1" | grep -Ev '^#|^\[|^$')
}

# openat2() is what the collection needs and what RestrictSUIDSGID= removed.
# A tiny probe with no build step: the one syscall, through ctypes.
probe='
import ctypes, os, sys
class OpenHow(ctypes.Structure):
    _fields_ = [("flags", ctypes.c_uint64), ("mode", ctypes.c_uint64), ("resolve", ctypes.c_uint64)]
libc = ctypes.CDLL("libc.so.6", use_errno=True)
libc.syscall.restype = ctypes.c_long
how = OpenHow(0, 0, 2)  # O_RDONLY, RESOLVE_NO_MAGICLINKS
r = libc.syscall(ctypes.c_long(437), ctypes.c_int(-100), b"/etc/hostname",
                 ctypes.byref(how), ctypes.c_size_t(ctypes.sizeof(how)))
if r < 0:
    errno = ctypes.get_errno()
    print(os.strerror(errno), file=sys.stderr)
    sys.exit(1 if errno == 38 else 0)  # ENOSYS is the failure this test is about
'
if command -v python3 >/dev/null 2>&1; then
  for shipped in "$unit" "$update_unit"; do
    name="$(basename "$shipped")"
    sandbox_of "$shipped"
    if "${runner[@]}" "${properties[@]}" /usr/bin/python3 -c "$probe" >/dev/null 2>&1; then
      ok "openat2() survives the sandbox of $name"
    else
      bad "openat2() is denied inside the sandbox of $name; bootc fails there with ENOSYS"
    fi
  done
else
  printf 'SKIP  python3 is absent, so the openat2() probe cannot run\n'
fi
sandbox_of "$unit"

# And the collection itself, on a machine that has one: the unit's command
# must exit 0, including when it has nothing to remove. /sysroot is root-only,
# so a machine without root cannot tell whether there is a repository to
# collect, and the full run is a root's.
if [ "$(id -u)" -eq 0 ] && [ -d /sysroot/composefs ] && [ -x /usr/bin/tilo ]; then
  if out="$("${runner[@]}" "${properties[@]}" /usr/bin/tilo os clean --only os-images 2>&1)"; then
    ok "the collection exits 0 ($(printf '%s' "$out" | tr '\n' ' ' | sed 's/  */ /g' | cut -c1-120))"
  else
    bad "the collection failed under its own sandbox: $(printf '%s' "$out" | tail -1)"
  fi
else
  printf 'SKIP  no composefs repository readable here (the full collection needs root), so it cannot run\n'
fi

[ "$fail" -eq 0 ] || exit 1
exit 0
