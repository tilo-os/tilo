#!/usr/bin/bash
# The files daemon (fs.service, SELinux domain tilo_fs_t) writes a regular file
# in /run/tilo: the session link (`--session-link`), the address of the loopback
# endpoint through which it reaches the network by way of the session
# (crates/system/fs/src/net.rs). /run/tilo is var_run_t, where tilo_fs_t may
# make only sockets. Without a rule for files, every start of fs.service ends
# with "write the session link /run/tilo/fs-session-link: Permission denied",
# /var/workspace is never mounted, and nothing of a workspace's files is there:
# an environment's `~` cannot be its workspace's tree.
#
# The file is published atomically (a temp file in the same directory, its
# mode set, synced, renamed over the name, the directory synced), so the type
# the daemon's files take in /run/tilo is given by type, not by name, and that
# type, not var_run_t, is what tilo_fs_t may write.
set -euo pipefail
repo="$(cd "$(dirname "$0")/../.." && pwd)"
policy="$repo/os/usr/share/tilo/selinux/tilo-base.cil"
unit="$repo/os/usr/lib/systemd/system/fs.service"
fail=0
ok() { printf 'ok: %s\n' "$*"; }
bad() { printf 'FAIL: %s\n' "$*" >&2; fail=1; }
active() { sed 's/;.*//' "$policy"; }
# No -q: grep leaving at the first match would SIGPIPE sed and fail the pipeline.
rule() { active | grep -xF "$1" >/dev/null; }
# Whether the policy allows `$1` every permission in `$4…` on class `$3` of type `$2`.
allows() {
  local who="$1" type="$2" class="$3" perms perm
  shift 3
  perms="$(active | sed -n "s/^(allow $who $type ($class (\([^)]*\))))$/\1/p" | tr '\n' ' ')"
  for perm in "$@"; do
    case " $perms " in
      *" $perm "*) ;;
      *) echo "tilo_fs_t lacks $class $perm on $type" >&2; return 1 ;;
    esac
  done
}

# Where the unit has the daemon write its session link.
link="$(tr -d '\\\n' <"$unit" | grep -o -- '--session-link [^ ]*' | awk '{print $2}' || true)"
case "$link" in
  /run/tilo/*) ;;
  *) bad "fs.service writes no session link under /run/tilo ($link); this test reads the wrong unit" ;;
esac

file_type=tilo_fs_var_run_t
if rule "(type $file_type)" \
  && rule "(typetransition tilo_fs_t var_run_t file $file_type)" \
  && allows tilo_fs_t var_run_t dir getattr search open read write add_name remove_name \
  && allows tilo_fs_t "$file_type" file create open read write getattr setattr rename unlink; then
  ok "tilo_fs_t can publish ${link##*/} in /run/tilo as a file of its own type"
else
  bad "tilo-base.cil does not let tilo_fs_t write $link; fs.service fails at every start"
fi
if active | grep -Eq '^\(allow tilo_fs_t var_run_t \(file '; then
  bad "tilo-base.cil lets tilo_fs_t write every var_run_t file; only its own type"
fi

exit "$fail"
