#!/bin/sh
# What a hosted workspace's VM must be for the Browser in it, printed as
# `field<TAB>value` lines for integration/vm-proofs.py to hold to:
#
#   - every renderer of the engine host is sandboxed: /proc/<pid>/status says
#     Seccomp: 2 (a filter) and NoNewPrivs: 1, and its network, pid and user
#     namespaces are its own, not the browser process's;
#   - no process runs the engine with `--no-sandbox`;
#   - the session and the engine run as the guest's unprivileged browser user,
#     not as the tenant and not as root;
#   - the device key the session took is a file only that user reads;
#   - the image's launcher entitlement lists name Browser, so its JIT and its
#     renderers' own sandbox are granted by the image and not by a manifest;
#   - what the session and its Chromium processes hold in memory, and the
#     VM's own.
#
# It runs in the guest as the tenant (`tilo-legacy hosted run -- /bin/sh -c`),
# and takes root there with sudo where it has it: reading another user's
# /proc/<pid>/ns asks for it.
set -u

browser_user=${BROWSER_USER:-tilo-browser}
field() { printf '%s\t%s\n' "$1" "$2"; }
have_root=no
if [ "$(id -u)" = 0 ]; then have_root=yes
elif command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then have_root=sudo
fi
as_root() {
  case $have_root in
    yes) "$@" ;;
    sudo) sudo -n "$@" ;;
    *) return 1 ;;
  esac
}
field root "$have_root"

ps -eo pid=,ppid=,user=,args= > /tmp/vm-proofs.ps 2>/dev/null || ps -eo pid,ppid,user,args > /tmp/vm-proofs.ps

# The session the host started, and the browser (engine host) it runs.
session=$(awk '$4 ~ /tilo-session/ && /--backend headless/ {print $1; exit}' /tmp/vm-proofs.ps)
engine=$(awk '$4 ~ /tilo-chromium-host/ {print $1; exit}' /tmp/vm-proofs.ps)
field session_pid "${session:-none}"
field engine_pid "${engine:-none}"
if [ -n "${session:-}" ]; then field session_user "$(ps -o user= -p "$session" | tr -d ' ')"; fi
if [ -n "${engine:-}" ]; then
  field engine_user "$(ps -o user= -p "$engine" | tr -d ' ')"
  field engine_ns_net "$(readlink "/proc/$engine/ns/net" 2>/dev/null || echo unreadable)"
  field engine_ns_pid "$(readlink "/proc/$engine/ns/pid" 2>/dev/null || echo unreadable)"
  field engine_ns_user "$(readlink "/proc/$engine/ns/user" 2>/dev/null || echo unreadable)"
fi

# Nothing in the VM may run the engine with its sandbox off.
for pid in $(awk '{print $1}' /tmp/vm-proofs.ps); do
  args=$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null)
  case $args in
    *"--no-sandbox"*) field no_sandbox_pid "$pid $args" ;;
  esac
done

# Every Chromium process, wherever it hangs: the renderers are the ones
# running as `--type=renderer`.
count=0
renderers=0
for pid in $(awk '$4 ~ /chrome|chromium/ {print $1}' /tmp/vm-proofs.ps); do
  args=$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null)
  [ -n "$args" ] || continue
  count=$((count + 1))
  user=$(ps -o user= -p "$pid" 2>/dev/null | tr -d ' ')
  case $args in
    *"--type=renderer"*)
      renderers=$((renderers + 1))
      status=/proc/$pid/status
      seccomp=$(awk '/^Seccomp:/ {print $2}' "$status" 2>/dev/null)
      nonewprivs=$(awk '/^NoNewPrivs:/ {print $2}' "$status" 2>/dev/null)
      net=$(readlink "/proc/$pid/ns/net" 2>/dev/null || echo unreadable)
      pidns=$(readlink "/proc/$pid/ns/pid" 2>/dev/null || echo unreadable)
      userns=$(readlink "/proc/$pid/ns/user" 2>/dev/null || echo unreadable)
      field renderer "$pid $user ${seccomp:-unknown} ${nonewprivs:-unknown} $net $pidns $userns"
      ;;
  esac
done
field chromium_processes "$count"
field renderers "$renderers"

# The whole of what the engine holds, over every Chromium process.
rss=0
pss=0
for pid in $(awk '$4 ~ /chrome|chromium|tilo-session/ {print $1}' /tmp/vm-proofs.ps); do
  value=$(awk '/^VmRSS:/ {print $2}' "/proc/$pid/status" 2>/dev/null)
  rss=$((rss + ${value:-0}))
  value=$(awk '/^Pss:/ {print $2}' "/proc/$pid/smaps_rollup" 2>/dev/null)
  pss=$((pss + ${value:-0}))
done
field browser_rss_kib "$rss"
field browser_pss_kib "$pss"
field vm_mem_total_kib "$(awk '/^MemTotal:/ {print $2}' /proc/meminfo 2>/dev/null)"
field vm_mem_available_kib "$(awk '/^MemAvailable:/ {print $2}' /proc/meminfo 2>/dev/null)"

# The device key the session took: only the browser user may read it.
key=$(as_root find /home/"$browser_user" /var/lib/tilo -name keypair.bin -path '*device-private*' 2>/dev/null | head -n 1)
if [ -z "${key:-}" ]; then
  key=$(as_root find / -xdev -name keypair.bin -path '*device-private*' 2>/dev/null | head -n 1)
fi
field device_key "${key:-none}"
if [ -n "${key:-}" ]; then
  field device_key_mode "$(as_root stat -c '%a' "$key" 2>/dev/null || echo unknown)"
  field device_key_owner "$(as_root stat -c '%U' "$key" 2>/dev/null || echo unknown)"
fi

# What the image lets Browser do: generate code, and make its own sandboxes.
for list in /usr/lib/tilo/jit-entitled.conf /usr/lib/tilo/web-engine-entitled.conf; do
  named=no
  [ -f "$list" ] || { field entitlement_missing "$list"; continue; }
  while IFS= read -r line; do
    case $line in
      'so.tilo.browser'|'so.tilo.browser '*) named=yes ;;
    esac
  done < "$list"
  field "entitlement_$(basename "$list" .conf)" "$named"
done

rm -f /tmp/vm-proofs.ps
