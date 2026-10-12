# Read by os/dev/test-nearby-pairing.sh on the paired machine as `sh -s`:
# exits 1 and lists every process that would mean pairing involved an
# administrator (a pkexec, a polkit agent's authentication helper, the
# home-peer control helper), exits 0 when there is none, and exits 2 when it
# cannot look.
#
# A process is identified by its own name (its comm, which the kernel cuts to
# 15 bytes) or the program its argv[0] names, never by a pattern anywhere in
# its command line. The gate used `ssh host '! pgrep -f polkit-agent-helper-1'`:
# procps leaves out only pgrep itself, not the shell that started it, so that
# found the remote shell whose command line carried the pattern and reported
# a helper that never ran. os/tests/nearby_privileged_processes_test.sh runs
# this the way the gate does.
set -u

[ -r /proc/self/comm ] || {
  echo 'no /proc: pending privileged processes cannot be observed' >&2
  exit 2
}

privileged() {
  case $1 in
    pkexec | polkit-agent-helper-1 | polkit-agent-he | tilo-home-peer-control | tilo-home-peer-) return 0 ;;
  esac
  return 1
}

found=0
for dir in /proc/[0-9]*; do
  # A process may exit between the listing and the reads.
  comm=$(cat "$dir/comm" 2>/dev/null) || continue
  argv0=$(tr '\0' '\n' < "$dir/cmdline" 2>/dev/null | head -n 1)
  if privileged "$comm" || privileged "${argv0##*/}"; then
    printf 'pending: %s %s\n' "${dir#/proc/}" "$(tr '\0' ' ' < "$dir/cmdline" 2>/dev/null)" >&2
    found=1
  fi
done
exit "$found"
