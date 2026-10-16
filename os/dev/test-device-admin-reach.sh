#!/usr/bin/env bash
# Two booted Tilo dev disks on separate networks, paired to one identity,
# reach each other only through a relay: the device-admin route
# (docs/device-reach.md, "Reaching it by name").
#
# Every VM sits on its own QEMU user-mode network (no shared segment, no
# route between them). A third VM runs the relay (`tilo-device-admin relay`),
# which the other two reach only through a port this host forwards to it, so
# nothing depends on the public relays. A ("board") is the device; B
# ("laptop") is a developer's machine that runs tilo-vm on itself. The
# identity's root key exists on B only while it certifies, as in
# os/dev/test-joined-device-startup.sh.
#
# The run proves, in order:
#   1. A's tilo-device-admin.service serves the route through the relay, and
#      `tilo-device-admin id` reports what tilo-vm records.
#   2. B enrols as an administrator (`tilo-vm vm enrol-admin`, the owner's
#      `tilo-enroll certify --capability admin`), finds board neither at its
#      recorded address nor by name, and reaches it by name over the relay:
#      `tilo-vm vm status board` says so, and a slot is created and listed.
#   3. A refuses another identity's administrator, a certificate that does
#      not verify, and one of the owner's devices without the grant, each
#      with its reason, and its journal logs every session and refusal.
#   4. With A's link cut, A does not answer; once restored, the session that
#      was open when it was cut answers again, and tilo-vm reaches it anew.
#
# Usage: os/dev/test-device-admin-reach.sh DEV_DISK_QCOW2 [--out DIR] [--display gl|2d] [--bootstrap-cli PATH]
#
# DEV_DISK_QCOW2 (a dev disk: tilo-vmd, tilo-device-admin, tilo-vm) is never
# booted or modified; each VM boots its own overlay.
set -Eeuo pipefail

dev_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
vm="$dev_dir/vm.sh"
base="${1:?usage: test-device-admin-reach.sh DEV_DISK_QCOW2 [--out DIR] [--display gl|2d] [--bootstrap-cli PATH]}"
shift
out="$PWD/device-admin-reach-artifacts"
display="${TILO_VM_DISPLAY:-gl}"
bootstrap_cli=""
bootstrap_state=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --out) out="${2:?--out needs a directory}"; shift 2 ;;
    --display) display="${2:?--display needs gl or 2d}"; shift 2 ;;
    --bootstrap-cli) bootstrap_cli="${2:?--bootstrap-cli needs the host tilo-vm executable}"; shift 2 ;;
    *) printf 'unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
done
case "$display" in gl|2d) ;; *) echo '--display must be gl or 2d' >&2; exit 2 ;; esac
[ -z "$bootstrap_cli" ] || [ -x "$bootstrap_cli" ] || {
  echo '--bootstrap-cli must name an executable host tilo-vm' >&2; exit 2;
}
[ -f "$base" ] || { printf 'disk not found: %s\n' "$base" >&2; exit 1; }
base="$(cd "$(dirname "$base")" && pwd)/$(basename "$base")"
command -v jq >/dev/null || { echo 'jq is required' >&2; exit 1; }

export TILO_USER="${TILO_USER:-tester}"
export TILO_PASS="${TILO_PASS:-tilo}"
arch="${TILO_VM_ARCH:-$(uname -m)}"
# Short: QEMU's monitor and console sockets live here (108-byte AF_UNIX limit).
run_a="${TILO_REACH_RUN_A:-/var/tmp/tilo-reach-a-$arch}"
run_b="${TILO_REACH_RUN_B:-/var/tmp/tilo-reach-b-$arch}"
run_c="${TILO_REACH_RUN_C:-/var/tmp/tilo-reach-c-$arch}"
relay_port="${TILO_REACH_RELAY_PORT:-23941}"
# 10.0.2.2 is this host as every user-mode network sees it.
relay_url="http://10.0.2.2:$relay_port"
# Where board was "last seen": an address no VM can reach, and a name
# nothing resolves, so tilo-vm finds it beyond the LAN.
board_address=198.51.100.10
identity_dir='$HOME/.config/tilo/identity'

mkdir -p "$out"
rm -rf -- "$run_a" "$run_b" "$run_c"
install -d -m 0700 "$run_a" "$run_b" "$run_c"

die() { printf 'device-admin reach: %s\n' "$*" >&2; exit 1; }
note() { printf 'device-admin reach: %s\n' "$*" >&2; }

vm_a() { TILO_RUN="$run_a" TILO_SSH_PORT="${TILO_REACH_SSH_A:-2241}" "$vm" "$@"; }
vm_b() { TILO_RUN="$run_b" TILO_SSH_PORT="${TILO_REACH_SSH_B:-2242}" "$vm" "$@"; }
vm_c() {
  TILO_RUN="$run_c" TILO_SSH_PORT="${TILO_REACH_SSH_C:-2243}" TILO_MEM="${TILO_REACH_RELAY_MEM:-2048}" \
    TILO_VM_HOSTFWD="hostfwd=tcp:127.0.0.1:$relay_port-:3340" "$vm" "$@"
}
# Root on a guest for test setup only. The account is in wheel; sudo takes
# the password's line from stdin; the script is a shell argument.
as_root() {
  local each=$1 script=$2
  # The password must never become the first script statement when sudo
  # has cached authentication or a fixture account has a NOPASSWD grant.
  script="'${script//\'/\'\\\'\'}'"
  printf '%s\n' "$TILO_PASS" | "$each" ssh "sudo -S -p '' sh -c $script"
}

collect() {
  local label=$1 run=$2 each=$3
  [ ! -f "$run/serial.log" ] || cp -f "$run/serial.log" "$out/$label-serial.log"
  [ ! -f "$run/qemu.log" ] || cp -f "$run/qemu.log" "$out/$label-qemu.log"
  as_root "$each" 'journalctl -b --no-pager -o short-monotonic -u tilo-device-admin -u tilo-reach-relay' \
    > "$out/$label-device-admin-journal.txt" 2>&1 || true
  "$each" ssh 'ip -br addr; ip route' > "$out/$label-network.txt" 2>&1 || true
}
cleanup() {
  local rc=$?
  trap - EXIT
  vm_a cmd "set_link ${nic_a:-virtio-net-pci.0} on" >/dev/null 2>&1 || true
  collect a "$run_a" vm_a
  collect b "$run_b" vm_b
  collect c "$run_c" vm_c
  vm_b ssh 'cat ~/reach/session.out ~/reach/session.err' > "$out/b-session.txt" 2>&1 || true
  vm_a stop >/dev/null 2>&1 || true
  vm_b stop >/dev/null 2>&1 || true
  vm_c stop >/dev/null 2>&1 || true
  [ -z "$bootstrap_state" ] || rm -rf -- "$bootstrap_state"
  rm -f -- "$run_a/disk.qcow2" "$run_b/disk.qcow2" "$run_c/disk.qcow2"
  printf '%s\n' "$rc" > "$out/exit-status.txt"
  exit "$rc"
}
trap cleanup EXIT
trap 'printf "device-admin reach: failed at scenario line %s\n" "$LINENO" >&2' ERR

# --- three machines, three networks --------------------------------------------
for run in "$run_a" "$run_b" "$run_c"; do
  "$vm" tool qemu-img create -f qcow2 -F qcow2 -b "$base" "$run/disk.qcow2" >/dev/null
done
vm_c boot --2d --disk "$run_c/disk.qcow2"
vm_a boot "--$display" --disk "$run_a/disk.qcow2"
vm_b boot "--$display" --disk "$run_b/disk.qcow2"
wait_ssh() {
  local each=$1
  for attempt in $(seq 1 90); do
    "$each" ssh true >/dev/null 2>&1 && return 0
    sleep 2
  done
  die "$each: no SSH after boot"
}
wait_ssh vm_c
wait_ssh vm_a
wait_ssh vm_b
for each in vm_a vm_b vm_c; do
  "$each" ssh 'command -v tilo-device-admin tilo-vmd tilo-enroll' >/dev/null \
    || die "$each: not a dev disk with tilo-device-admin, tilo-vmd and tilo-enroll"
done

# Older gated dev disks have the daemon and peer service but no developer
# CLI. Optional bootstrap uses a real, provenanced VM-wide component swap;
# only this disposable laptop receives a fresh fixture SSH key/sudo grant.
# No binaries are copied over the scenario's setup SSH connection.
if [ -n "$bootstrap_cli" ]; then
  bootstrap_state="$(mktemp -d "${TMPDIR:-/var/tmp}/tilo-reach-bootstrap.XXXXXXXX")"
  chmod 0700 "$bootstrap_state"
  ssh-keygen -q -t ed25519 -N '' -f "$bootstrap_state/key"
  fixture_key="$(cat "$bootstrap_state/key.pub")"
  vm_b ssh "umask 077; mkdir -p ~/.ssh; printf '%s\n' '$fixture_key' >> ~/.ssh/authorized_keys"
  as_root vm_b "set -eu
    printf '%s\n' '$TILO_USER ALL=(root) NOPASSWD: ALL' > /etc/sudoers.d/tilo-reach-bootstrap
    chmod 0440 /etc/sudoers.d/tilo-reach-bootstrap
    visudo -c -f /etc/sudoers.d/tilo-reach-bootstrap" > "$out/bootstrap-sudo.txt"
  # An explicit private registry and VM selection keep the host's developer
  # registry, identity and unrelated running VMs out of the scenario.
  TILO_VM_CONFIG="$bootstrap_state/vms.toml" "$bootstrap_cli" vm add reach-bootstrap \
    --ssh "$TILO_USER@127.0.0.1:${TILO_REACH_SSH_B:-2242}" --key "$bootstrap_state/key" \
    > "$out/bootstrap-add.txt" 2>&1
  TILO_VM_CONFIG="$bootstrap_state/vms.toml" TILO_VM=reach-bootstrap \
    "$bootstrap_cli" swap --slot vm vm-cli --builder host --profile dev \
    > "$out/bootstrap-swap.txt" 2>&1
  as_root vm_b 'rm -f /etc/sudoers.d/tilo-reach-bootstrap'
  vm_b ssh "sed -i '\|$fixture_key|d' ~/.ssh/authorized_keys"
fi
vm_b ssh 'command -v tilo-vm' >/dev/null || die 'VM B has no developer CLI; use --bootstrap-cli with a host build'

# The relay, on C only. Its desktop firewall zone accepts this port; the rule
# is added anyway, so a stricter zone does not fail the run for the wrong reason.
as_root vm_c "set -eu
  if command -v firewall-cmd >/dev/null && firewall-cmd --state >/dev/null 2>&1; then
    firewall-cmd --add-port=3340/tcp >/dev/null
  fi
  systemd-run --unit=tilo-reach-relay /usr/bin/tilo-device-admin relay --listen 0.0.0.0:3340 >/dev/null" \
  || die "VM C could not start the relay"
for each in vm_a vm_b; do
  for attempt in $(seq 1 60); do
    code="$("$each" ssh "curl -s -o /dev/null -m 3 -w '%{http_code}' $relay_url/" 2>/dev/null || true)"
    [ -n "$code" ] && [ "$code" != 000 ] && break
    [ "$attempt" -lt 60 ] || die "$each cannot reach the relay at $relay_url"
    sleep 2
  done
done
# Separate networks: both guests are 10.0.2.15 on networks of their own;
# neither has an address or route the other could be reached at.
net_a="$(vm_a ssh 'ip -4 -o addr show scope global | awk "{print \$4}"')"
net_b="$(vm_b ssh 'ip -4 -o addr show scope global | awk "{print \$4}"')"
printf 'A: %s\nB: %s\n' "$net_a" "$net_b" > "$out/networks.txt"
[ "$net_a" = 10.0.2.15/24 ] && [ "$net_b" = 10.0.2.15/24 ] \
  || die "A and B are not each alone on a user-mode network: A=$net_a B=$net_b"

# --- one identity: A and B are the owner's devices ------------------------------
vm_b ssh 'umask 077; rm -rf ~/reach; mkdir -p ~/reach && head -c 32 /dev/urandom > ~/reach/root.key && head -c 32 /dev/urandom > ~/reach/stranger.key'
device_key() { "$1" ssh 'tilo-enroll device-key' | jq -r .device_public_key; }
join() {
  local each=$1 label=$2 device cert
  device="$(device_key "$each")"
  [[ $device =~ ^[0-9a-f]{64}$ ]] || die "$each printed no device key"
  cert="$(vm_b ssh "tilo-enroll certify --root-key ~/reach/root.key --device $device \
    --label $label --capability own --out ~/reach/$label.cert && base64 -w0 ~/reach/$label.cert")"
  "$each" ssh "install -d -m 0700 \$HOME/.config/tilo $identity_dir && base64 -d > /tmp/member.cert" <<<"$cert"
  printf '%s\n' "$TILO_PASS" \
    | "$each" ssh "sudo -S -p '' tilo-enroll install-cert --identity-dir $identity_dir --cert /tmp/member.cert" >/dev/null \
    || die "$each could not install its certificate"
  printf '%s\n' "$device"
}
device_a="$(join vm_a board)"
device_b="$(join vm_b laptop)"
identity="$(vm_a ssh 'od -An -v -tx1 /var/lib/tilo/user-identity/pubkey.bin' | tr -d ' \n')"
[[ $identity =~ ^[0-9a-f]{64}$ ]] || die "A belongs to no identity"
[ "$(vm_b ssh 'od -An -v -tx1 /var/lib/tilo/user-identity/pubkey.bin' | tr -d ' \n')" = "$identity" ] \
  || die "B joined another identity than A"
note "A ($device_a) and B ($device_b) belong to identity $identity"

# --- A serves the route through the relay ---------------------------------------
as_root vm_a "set -eu
  printf 'TILO_DEVICE_ADMIN_RELAY=%s\n' '$relay_url' > /etc/tilo/device-admin.env
  systemctl restart tilo-device-admin" || die "A's tilo-device-admin did not restart"
for attempt in $(seq 1 30); do
  id_a="$(as_root vm_a 'tilo-device-admin id' 2>/dev/null || true)"
  [ "$(jq -r '.serving // false' <<<"$id_a" 2>/dev/null)" = true ] && break
  [ "$attempt" -lt 30 ] || die "A's route is not served: ${id_a:-no answer}"
  sleep 2
done
printf '%s\n' "$id_a" > "$out/a-id.json"
[ "$(jq -r .device <<<"$id_a")" = "$device_a" ] || die "A's route names another device: $id_a"
[ "$(jq -r .identity <<<"$id_a")" = "$identity" ] || die "A's route names another identity: $id_a"
[ "$(jq -r .relay <<<"$id_a")" = "$relay_url/" ] || [ "$(jq -r .relay <<<"$id_a")" = "$relay_url" ] \
  || die "A does not use the harness relay: $id_a"
endpoint_a="$(jq -r .endpoint <<<"$id_a")"

# --- B: a developer's machine, enrolled, reaches board by name ------------------
# Each tilo-vm configuration below is one machine's view: its registry and,
# beside it, the identity it administers with.
board_entry() {
  cat <<TOML
default = "board"

[vms.board]
hypervisor = "device"
ssh = "root@$board_address:22"
hostname = "tilo-reach-board"

[vms.board.peer]
device = "$device_a"
endpoint = "$endpoint_a"
identity = "$identity"
relay = "$relay_url"
TOML
}
tilo_vm() {
  local config=$1
  shift
  vm_b ssh "TILO_VM_CONFIG=~/reach/$config/vms.toml tilo-vm $*"
}
setup_config() {
  local config=$1
  board_entry | vm_b ssh "mkdir -p ~/reach/$config && cat > ~/reach/$config/vms.toml"
}
setup_config admin
unenrolled="$(tilo_vm admin vm enrol-admin --json)"
printf '%s\n' "$unenrolled" > "$out/b-enrol-before.json"
[ "$(jq -r .enrolled <<<"$unenrolled")" = false ] || die "a fresh machine claims to be enrolled: $unenrolled"
admin_device="$(jq -r .device <<<"$unenrolled")"
[[ $admin_device =~ ^[0-9a-f]{64}$ ]] || die "vm enrol-admin made no device key: $unenrolled"
jq -e '.next | any(test("enrol-admin --via"))' <<<"$unenrolled" >/dev/null \
  || die "vm enrol-admin does not say how to enrol: $unenrolled"
# The owner certifies the machine's key, as on the device that holds the identity.
vm_b ssh "tilo-enroll certify --root-key ~/reach/root.key --device $admin_device \
  --label 'laptop (tilo-vm)' --capability admin --out ~/reach/admin.cert"
# Keep the tilde literal until the guest shell receives the certificate path.
enrolled="$(tilo_vm admin vm enrol-admin --cert '~/reach/admin.cert' --json)" \
  || die "VM B could not install its administrator certificate: $enrolled"
printf '%s\n' "$enrolled" > "$out/b-enrol-after.json"
[ "$(jq -r .enrolled <<<"$enrolled")" = true ] && [ "$(jq -r .identity <<<"$enrolled")" = "$identity" ] \
  || die "vm enrol-admin --cert did not enrol the machine: $enrolled"

status_text="$(tilo_vm admin vm status board 2>&1)" || die "tilo-vm vm status board failed: $status_text"
printf '%s\n' "$status_text" > "$out/b-status.txt"
grep -q 'reached over the peer network: relay' <<<"$status_text" \
  || die "vm status does not name the route it took: $status_text"
status_json="$(tilo_vm admin vm status board --json)"
printf '%s\n' "$status_json" > "$out/b-status.json"
[ "$(jq -r .state <<<"$status_json")" = ready ] || die "board is not ready over the relay: $status_json"
[ "$(jq -r .route.peer.route <<<"$status_json")" = relay ] \
  || die "board was not reached through the relay: $status_json"
[ "$(jq -r .route.peer.device <<<"$status_json")" = "$device_a" ] \
  || die "another device answered for board: $status_json"
jq -e --arg relay "$relay_url" '.route.peer.via | startswith($relay)' <<<"$status_json" >/dev/null \
  || die "the route went through another relay than the harness's: $status_json"
created="$(tilo_vm admin slot create reach --headless --json)"
printf '%s\n' "$created" > "$out/b-slot-create.json"
[ "$(jq -r .slot.name <<<"$created")" = reach ] || die "slot create over the relay failed: $created"
listed="$(tilo_vm admin slot list --json)"
printf '%s\n' "$listed" > "$out/b-slot-list.json"
jq -e 'map(.name) | index("reach")' <<<"$listed" >/dev/null || die "the new slot is not listed: $listed"
note "B reached board through the relay and created a slot on it"

# --- A refuses everyone else ------------------------------------------------------
# Another identity's administrator.
setup_config stranger
stranger_device="$(tilo_vm stranger vm enrol-admin --json | jq -r .device)"
vm_b ssh "tilo-enroll certify --root-key ~/reach/stranger.key --device $stranger_device \
  --label stranger --capability admin --out ~/reach/stranger.cert"
tilo_vm stranger vm enrol-admin --cert '~/reach/stranger.cert' --json >/dev/null
# A certificate that claims the owner's identity but that the owner never
# signed: the stranger's, with the owner's public key written over its own.
setup_config forged
forged_device="$(tilo_vm forged vm enrol-admin --json | jq -r .device)"
vm_b ssh "tilo-enroll certify --root-key ~/reach/stranger.key --device $forged_device \
  --label forged --capability admin --out ~/reach/forged.cert
  python3 -c 'import sys; c = open(sys.argv[1], \"rb\").read(); open(sys.argv[2], \"wb\").write(bytes.fromhex(sys.argv[3]) + c[32:])' \
    ~/reach/forged.cert ~/reach/forged/admin-identity/device/cert.bin $identity"
refusal() {
  local config=$1 reason=$2 status
  status="$(tilo_vm "$config" vm status board --json || true)"
  printf '%s\n' "$status" > "$out/b-refused-$config.json"
  [ "$(jq -r .state <<<"$status")" = peer-refused ] || die "$config was not refused: $status"
  jq -e --arg reason "$reason" '.error | contains($reason)' <<<"$status" >/dev/null \
    || die "$config was refused without the reason $reason: $status"
  jq -e '.hint | contains("enrol-admin")' <<<"$status" >/dev/null \
    || die "the refusal of $config does not say how to fix it: $status"
}
refusal stranger OtherIdentity
refusal forged NotCertified
# One of the owner's devices without the grant: B itself, as its own device
# (nobody is logged in to B, so its session does not hold its device key's
# endpoint).
protocol="$(jq -r .daemon.protocol <<<"$status_json")"
hello="$(jq -cn --argjson protocol "$protocol" \
  '{jsonrpc: "2.0", id: 1, method: "hello", params: {protocol: $protocol, client_version: "reach"}}')"
member="$(as_root vm_b "printf '%s\n' '$hello' | tilo-device-admin dial --identity-root /var/lib/tilo \
  --endpoint $endpoint_a --device $device_a --relay $relay_url --wait 60 2>&1; echo \"exit=\$?\"" || true)"
printf '%s\n' "$member" > "$out/b-refused-member.txt"
grep -qx 'exit=3' <<<"$member" && grep -q NotAdministrator <<<"$member" \
  || die "a device of the owner without the grant was not refused as NotAdministrator: $member"

journal_a="$(as_root vm_a 'journalctl -b --no-pager -o cat -u tilo-device-admin')"
printf '%s\n' "$journal_a" > "$out/a-journal-after-refusals.txt"
for reason in OtherIdentity NotCertified NotAdministrator; do
  grep -q "refused an admin session .*: $reason:" <<<"$journal_a" \
    || die "A's journal does not log the $reason refusal"
done
grep -q "admin session [0-9]* opened by \"laptop (tilo-vm)\" .* over relay $relay_url" <<<"$journal_a" \
  || die "A's journal does not log the administrator's session and its route"
grep -q 'admin session [0-9]*: slot.create' <<<"$journal_a" || die "A's journal does not log the requests"
note "A refused another identity, a forged certificate and a device without the grant"

# --- the link is cut and restored -------------------------------------------------
nic_a="$(vm_a cmd 'info network' | tr -d '\r' | grep -o '[A-Za-z0-9._-]*: index=[0-9]*,type=nic' | head -n 1 | cut -d: -f1)"
[ -n "$nic_a" ] || die "A's network card was not found in QEMU's monitor"
dial_admin="tilo-device-admin dial --identity-root ~/reach/admin/admin-identity --endpoint $endpoint_a --device $device_a --relay $relay_url --wait 120"
status_line() { printf '{"jsonrpc":"2.0","id":%s,"method":"status","params":{}}' "$1"; }
# A session that stays open across the cut: hello and a status before it,
# a status after the link is back.
vm_b ssh "cat > ~/reach/session.sh" <<EOF
set -u
cd ~/reach && rm -f in out session.out session.err ready restored done
mkfifo in
$dial_admin < in > session.out 2> session.err &
dial=\$!
exec 3> in
printf '%s\n' '$hello' >&3
printf '%s\n' '$(status_line 2)' >&3
for _ in \$(seq 1 120); do [ "\$(wc -l < session.out)" -ge 3 ] && break; sleep 1; done
touch ready
for _ in \$(seq 1 600); do [ -e restored ] && break; sleep 1; done
printf '%s\n' '$(status_line 3)' >&3
for _ in \$(seq 1 180); do [ "\$(wc -l < session.out)" -ge 4 ] && break; sleep 1; done
exec 3>&-
wait \$dial
echo "\$?" > done
EOF
vm_b ssh '(setsid bash ~/reach/session.sh < /dev/null > /dev/null 2>&1 &)'
for attempt in $(seq 1 120); do
  vm_b ssh 'test -e ~/reach/ready' && break
  [ "$attempt" -lt 120 ] || die "the session before the cut never answered: $(vm_b ssh 'cat ~/reach/session.out ~/reach/session.err')"
  sleep 1
done
vm_b ssh 'test "$(wc -l < ~/reach/session.out)" -ge 3' \
  || die "the session before the cut did not answer hello and status: $(vm_b ssh 'cat ~/reach/session.out ~/reach/session.err')"
vm_a cmd "set_link $nic_a off" >/dev/null
note "A's link ($nic_a) is cut"
if cut="$(vm_b ssh "printf '%s\n' '$hello' | timeout 60 tilo-device-admin dial --identity-root ~/reach/admin/admin-identity --endpoint $endpoint_a --device $device_a --relay $relay_url --wait 10" 2>&1)"; then
  cut_status=0
else
  cut_status=$?
fi
printf 'exit %s\n%s\n' "$cut_status" "$cut" > "$out/b-dial-while-cut.txt"
[ "$cut_status" = 2 ] || die "A answered with its link cut (exit $cut_status): $cut"
# Long enough for the open connection to time out, so the session has to
# dial again rather than ride on QUIC retransmission.
sleep 45
vm_a cmd "set_link $nic_a on" >/dev/null
note "A's link is restored"
wait_ssh vm_a
vm_b ssh 'touch ~/reach/restored'
for attempt in $(seq 1 240); do
  vm_b ssh 'test -e ~/reach/done' && break
  [ "$attempt" -lt 240 ] || die "the session after the cut never ended: $(vm_b ssh 'cat ~/reach/session.out ~/reach/session.err')"
  sleep 1
done
session="$(vm_b ssh 'cat ~/reach/session.out; echo "exit $(cat ~/reach/done)"; cat ~/reach/session.err')"
printf '%s\n' "$session" > "$out/b-session.txt"
vm_b ssh 'sed -n 4p ~/reach/session.out' | jq -e '.id == 3 and .result != null' >/dev/null \
  || die "the open session did not answer after the link came back: $session"
[ "$(vm_b ssh 'cat ~/reach/done')" = 0 ] || die "the open session ended with an error: $session"
after="$(tilo_vm admin vm status board --json)"
printf '%s\n' "$after" > "$out/b-status-after-cut.json"
[ "$(jq -r .state <<<"$after")" = ready ] && [ "$(jq -r .route.peer.route <<<"$after")" = relay ] \
  || die "tilo-vm does not reach board through the relay after the cut: $after"
note "the open session and tilo-vm reach board again after the cut"

printf 'PASS\n' > "$out/verdict.txt"
