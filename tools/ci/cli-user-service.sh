#!/usr/bin/env bash
# `tilo login --new-identity` on stock Linux installs this computer's daemon as the person's systemd
# user unit and systemd starts it; a fresh manager starts the enabled unit as after a reboot. A job's
# sandbox has no writable cgroups, so the check runs in a VM and starts a user manager here, as a
# login's is (`dist/session/tests/private-user-manager-handoff.sh` does on a host).
set -euo pipefail
cli=${1:?the directory the built tilo is in}; work=$(mktemp -d); manager=
trap 'systemctl --user stop tilo-peer.service >/dev/null 2>&1 || true; kill ${manager:-} 2>/dev/null || true; rm -rf "$work"' EXIT
export HOME=$work/home XDG_RUNTIME_DIR=$work/run XDG_CONFIG_HOME=$work/config XDG_STATE_HOME=$work/state XDG_DATA_HOME=$work/data DBUS_SESSION_BUS_ADDRESS=unix:path=$work/run/bus
mkdir -p "$HOME" "$XDG_CONFIG_HOME"; install -d -m 0700 "$XDG_RUNTIME_DIR"
mkdir -p /sys/fs/cgroup /run/systemd/system; mountpoint -q /sys/fs/cgroup || mount -t cgroup2 none /sys/fs/cgroup
fail() { echo "FAIL: $*"; exit 1; }
unit=$XDG_CONFIG_HOME/systemd/user/tilo-peer.service
stamp() { stat -c '%y' "$unit"; sha256sum "$unit"; }
active() { for _ in $(seq 1 300); do systemctl --user is-active --quiet tilo-peer.service && return; sleep 0.1; done; fail 'tilo-peer.service is not active'; }
start_manager() {
  /usr/lib/systemd/systemd --user --unit="$1" >>"$work/manager.log" 2>&1 & manager=$!
  for _ in $(seq 1 200); do [[ -S $XDG_RUNTIME_DIR/systemd/private ]] && return; sleep 0.05; done
  fail "the login's user manager did not start"
}
start_manager basic.target
"$cli/tilo" login --new-identity >"$work/first" 2>&1 || fail "tilo login --new-identity failed: $(tail -3 "$work/first")"
[[ -f $unit ]] || fail "the login installed no $unit"
systemctl --user is-enabled --quiet tilo-peer.service || fail 'the unit is not enabled'
active; echo 'ok: the login installed and started the daemon as their user unit'
"$cli/tilo" status >"$work/status" 2>&1 || fail "tilo status failed: $(tail -3 "$work/status")"
grep -q . "$work/status" || fail 'tilo status answered nothing'; echo 'ok: tilo status answers'
before=$(stamp)
"$cli/tilo" login --new-identity >"$work/second" 2>&1 || fail "the second login failed: $(tail -3 "$work/second")"
[[ $(stamp) == "$before" ]] || fail 'the second login changed the unit'
active; echo 'ok: a second login reinstalled nothing'
systemctl --user stop tilo-peer.service
kill "$manager"; wait "$manager" 2>/dev/null || true
rm -rf "$XDG_RUNTIME_DIR/tilo"
start_manager default.target
active; [[ $(stamp) == "$before" ]] || fail 'the unit changed as a fresh manager started it'
"$cli/tilo" status >/dev/null 2>&1 || fail 'the restarted daemon does not answer'
echo 'ok: a fresh manager starts the enabled unit again, nothing reinstalled'
