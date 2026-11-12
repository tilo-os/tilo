#!/usr/bin/env bash
# A VM host of the local tilo.sh stack: the image tilo.sh's VM hosts run
# (os/containers/tilo-vm-host), privileged and with a state disk of its own,
# the placer that places workspaces' VMs on it, and the guest image this
# checkout built instead of a release's. It is what a hosted workspace needs
# under it locally: a workspace's Linux programs and its Browser run in its VM
# here, and its browser session joins the workspace at the relay beside it.
#
# usage: vm-host.sh up|down|status|env
#
#   up       make the state disk, certify this host's member device under the
#            relay's root, ready the guest image, start the placer and
#            vm-host-0
#   down     stop both, and remove the state disk and the host's identities
#   status   what is up, and the VMs the host holds
#   env      the variables the relay reads to place here (nothing while the
#            host is down)
#
# What a caller passes (each has the default shown):
#
#   TILO_SH_STATE=/var/lib/tilo-sh      this host's own state directory
#   TILO_SH_IMAGE_VM_HOST               the vm-host image: its OCI archive, or
#                                       a reference podman already holds
#   TILO_SH_HOSTED_GUEST=<dir>          the image-hosted-guest check's outputs:
#                                       an image.oci.tar with the digest beside
#                                       it
#   TILO_SH_TILO_ENROLL                 the tilo-enroll this host's member
#                                       device is made with, as a release
#                                       ships it (default: on PATH)
#   TILO_SH_HOSTED_VM_SIZE=default      the size a workspace's VM runs at, as
#                                       the plans file names it (`small`,
#                                       `default`, `large`, `xlarge`): the
#                                       size production places, unless a
#                                       machine with less room places smaller
#   TILO_SH_VM_HOST_MEMORY_MIB          what the host holds awake at once: by
#                                       default the size's own memory (8 GiB
#                                       at `default`) beside the host's own,
#                                       so the machine's room is what bounds
#                                       it, not this stack's default
#   TILO_SH_RELAY_SECRETS=<state>/secrets
#                                       where the relay's secrets are on this
#                                       machine: the hosts' member devices and
#                                       the plans file are written there, for
#                                       the relay to read
#   TILO_SH_RELAY_ROOT=<state>/secrets/room-root-lounge
#                                       the relay's owner root, 64 hex
#                                       characters: what certifies the member
#   TILO_SH_RELAY_STATE=<state>/rooms/lounge/relay
#                                       the relay's own state, where the
#                                       device key it answers as is
#   TILO_SH_RELAY_IROH=127.0.0.1:24104  where the relay's endpoint listens
#
# Root, /dev/kvm and a loop device are what a VM host needs: without them
# there is nothing to run, and `up` says so and stops.
set -euo pipefail

state=${TILO_SH_STATE:-/var/lib/tilo-sh}
vm=$state/vm
# The relay's secrets: where it reads the member devices and the plans file
# from, which may be a stack's directory rather than this host's.
secrets=${TILO_SH_RELAY_SECRETS:-$state/secrets}
pod=vm-host-0
placer_container=tilo-sh-vm-placer
host_container=tilo-sh-$pod
place_port=${TILO_SH_VM_HOST_PLACER_PORT:-7702}
status_port=${TILO_SH_VM_HOST_STATUS_PORT:-7701}
member_port=${TILO_SH_VM_HOST_MEMBER_PORT:-7703}
relay_iroh=${TILO_SH_RELAY_IROH:-127.0.0.1:24104}
relay_root=${TILO_SH_RELAY_ROOT:-$secrets/room-root-lounge}
relay_state=${TILO_SH_RELAY_STATE:-$state/rooms/lounge/relay}
enroll=${TILO_SH_TILO_ENROLL:-tilo-enroll}
# What the host holds awake at once: the memory of the size it places, as the
# size table gives it (crates/runtime/legacy-vm/src/hosted/size.rs: small
# 4 GiB, default 8 GiB, large 16 GiB, xlarge 32 GiB), beside the host's own
# few hundred megabytes. A VM woken past a short budget waits for room rather
# than running, so a host that places 8 GiB VMs needs more than 8 GiB here.
case ${TILO_SH_HOSTED_VM_SIZE:-default} in
  small) size_memory_mib=4096 ;;
  large) size_memory_mib=16384 ;;
  xlarge) size_memory_mib=32768 ;;
  *) size_memory_mib=8192 ;;
esac
memory_mib=${TILO_SH_VM_HOST_MEMORY_MIB:-$((size_memory_mib + 2048))}
# The relay's own view of what this host writes for it: the stack mounts its
# secrets at /run/secrets and the relay's state at /run/tilo-rooms.
relay_secrets=/run/secrets
relay_rooms=/run/tilo-rooms
# A VM host's state disk is 512 GiB on the cloud: a sparse file that size
# holds the same, since only what is written is stored.
state_size=${TILO_SH_VM_HOST_STATE_SIZE:-512G}
state_device=/dev/tilo-vm-state
image_guest=registry.tilo.so/tilo/hosted-guest

say() { printf 'tilo-sh vm-host: %s\n' "$*" >&2; }
fail() { say "$*"; exit 1; }
json_field() { sed -n "s/.*\"$1\":\"\([0-9a-f]*\)\".*/\1/p"; }

need_root() {
  [ "$(id -u)" = 0 ] || fail 'a VM host runs root containers, loop devices and KVM: run this as root'
}

# The loop device the state disk is attached at, when it is.
state_loop() {
  [ -f "$vm/state.img" ] || return 0
  losetup --noheadings --output NAME --associated "$vm/state.img" 2>/dev/null | head -n 1
}

# The reference the guest is pinned by: its digest beside it, in the guest's
# own repository, which is the only reference a host fetches.
guest_reference() { # dir -> reference
  [ -f "$1/digest" ] || fail "$1 holds no digest: name the check's outputs (image.oci.tar with digest)"
  [ -f "$1/image.oci.tar" ] || fail "$1 holds no image.oci.tar: name the check's outputs"
  printf '%s@%s' "$image_guest" "$(tr -d '[:space:]' < "$1/digest")"
}

# Lay an image's kernel and root filesystem where `fetch-guest` keeps a
# fetched guest, under its digest. A start that finds them there fetches
# nothing, and never needs the registry or the release key.
guest_ready() { # dir images -> reference
  local dir=$1 images=$2 reference digest layer into
  reference=$(guest_reference "$dir")
  digest=${reference#*@sha256:}
  into=$images/hosted-$digest
  if [ ! -f "$into/vmlinux" ] || [ ! -f "$into/rootfs.ext4" ]; then
    # The manifest the digest names, and its one layer: the image holds a
    # single layer, which is the kernel and the root filesystem. The layer is
    # written out before it is read, since a compressed one is only told apart
    # by looking at its first bytes, which a pipe does not allow.
    layer=$(tar -xOf "$dir/image.oci.tar" "blobs/sha256/$digest" | tr -d '[:space:]' |
      sed -n 's/.*"layers":\[{[^}]*"digest":"sha256:\([0-9a-f]\{64\}\)".*/\1/p')
    [ -n "$layer" ] || fail "$dir/image.oci.tar: no layer in its manifest"
    tar -xOf "$dir/image.oci.tar" "blobs/sha256/$layer" > "$images/.layer-$digest"
    rm -rf "$into.new"
    mkdir -p "$into.new"
    tar -xf "$images/.layer-$digest" -C "$into.new" vmlinux rootfs.ext4
    rm -f "$images/.layer-$digest"
    rm -rf "$into"
    mv "$into.new" "$into"
  fi
  printf '%s' "$reference"
}

# The relay's own device, as the member must know it: the key it certified in
# the relay's state, its public key, and the endpoint id it answers on (a
# relay's endpoint is keyed apart from its device). `tilo-enroll device-key`
# derives both from the key.
relay_device() {
  local work=$vm/relay-device
  [ -f "$relay_state/host-device.key" ] \
    || fail "$relay_state/host-device.key: the relay has not started yet: start the stack's relay first"
  rm -rf "$work"
  mkdir -p "$work/device-private"
  chmod 0700 "$work/device-private"
  install -m 0600 "$relay_state/host-device.key" "$work/device-private/keypair.bin"
  "$enroll" device-key --identity-root "$work"
}

# The state disk: a sparse file loop-attached and made XFS with reflink the
# first time, as the cloud's volume is, mounted to lay out the guest before
# the host starts.
state_disk() {
  local loop mount=$vm/mnt
  mkdir -p "$vm"
  if [ ! -f "$vm/state.img" ]; then
    say "making a $state_size state disk at $vm/state.img"
    truncate -s "$state_size" "$vm/state.img"
  fi
  loop=$(state_loop)
  if [ -z "$loop" ]; then
    loop=$(losetup --find --show "$vm/state.img")
    say "attached the state disk at $loop"
  fi
  mkdir -p "$mount"
  if ! blkid -p "$loop" >/dev/null 2>&1; then
    mkfs.xfs -q -m reflink=1 -L tilovmstate "$loop"
  fi
  mountpoint -q "$mount" || mount -t xfs -o noatime "$loop" "$mount"
  mkdir -p "$mount/state" "$mount/images" "$mount/jails"
  guest_ready "$TILO_SH_HOSTED_GUEST" "$mount/images" > "$vm/guest-ref"
  chown -R 10001:10001 "$mount/state" "$mount/images"
  chown root:root "$mount/jails"
  chmod 0755 "$mount/jails"
  umount "$mount"
}

# The operator root as a signing key: the relay's root file is 64 hex
# characters, and `tilo-enroll certify --root-key` reads 32 bytes.
root_key() { # work
  local hex
  hex=$(tr -d '[:space:]' < "$relay_root")
  [ ${#hex} = 64 ] || fail "$relay_root: the relay's root is 64 hex characters"
  case $hex in
    *[!0-9a-fA-F]*) fail "$relay_root: the relay's root is 64 hex characters" ;;
  esac
  printf "$(printf '%s' "$hex" | sed 's/../\\x&/g')" > "$1/root-key.bin"
  chmod 0600 "$1/root-key.bin"
}

# This host's member device, as the tar the start installs on its state disk
# (TILO_VM_MEMBER_IDENTITIES): its key, the relay's root certifying it, and
# the person's record. It prints the device's public key.
member_identity() {
  local work=$vm/member identities=$vm/member-identities device
  [ -f "$relay_root" ] || fail "$relay_root: the relay's root is not there yet"
  if [ ! -f "$identities/$pod.tar" ]; then
    rm -rf "$work"
    mkdir -p "$work/root" "$work/person"
    chmod 0700 "$work/root"
    device=$("$enroll" device-key --identity-root "$work/root" | json_field device_public_key)
    [ -n "$device" ] || fail "the host's device key was not made"
    root_key "$work"
    "$enroll" certify --root-key "$work/root-key.bin" --identity-root "$work/root" \
      --device "$device" --label "$pod of the local tilo.sh stack" \
      --capability own --capability host --out "$work/cert.bin" >&2
    "$enroll" install-cert --identity-root "$work/root" --identity-dir "$work/person" \
      --cert "$work/cert.bin" >&2
    # The start installs `root` as the host's identity and `person` as the
    # person's record (member/config/tilo/identity), both as the service.
    mkdir -p "$identities"
    chmod 0755 "$identities"
    tar -C "$work" -cf "$identities/$pod.tar" --owner=0 --group=0 root person
    printf '%s' "$device" > "$identities/$pod.device"
    rm -f "$work/root-key.bin"
  fi
  cat "$identities/$pod.device"
}

# The plans file the relay and the host admit by (docs/hosted-browser.md,
# "Metering"): five free Browser page actions, so the card prompt comes after
# them, and one account with a card on file for the Linux VMs and for what
# follows the card. TILO_SH_BROWSER_FREE_ACTIONS sets another allowance: a
# check that counts page actions gives itself few of them. The file lives in
# the secrets the stack mounts for the relay, which reads it again for every
# page action: an account added to it mid-run is served from the next one.
plans_file=$secrets/vm-host-plans.json
free_actions=${TILO_SH_BROWSER_FREE_ACTIONS:-5}
plans() {
  local file=$plans_file
  [ -f "$file" ] && return 0
  mkdir -p "$(dirname "$file")"
  cat > "$file" <<JSON
{
  "sells": {
    "host": "Tilo local",
    "plans": [
      {
        "id": "local",
        "name": "Local",
        "price": "free",
        "limits": { "max_running": 2, "max_vms": 5, "sizes": ["small", "default"] }
      }
    ],
    "browser_free_actions": $free_actions,
    "add_payment_method": "http://localhost:24110/billing/card?account={account}",
    "manage_billing": "http://localhost:24110/billing?account={account}",
    "raise_limit": "http://localhost:24110/billing/plan?account={account}"
  },
  "accounts": {
    "acct-with-card": {
      "plan": "local",
      "max_running": 2,
      "max_vms": 5,
      "sizes": ["small", "default"],
      "card": { "brand": "visa", "last4": "4242" }
    }
  }
}
JSON
}

# The VM hosts' member devices as the relay reads them: this stack's one host,
# by the name the placer answers and where its endpoint listens.
members() { # device
  mkdir -p "$secrets"
  printf '{\n  "%s": {\n    "device": "%s",\n    "address": "127.0.0.1:%s"\n  }\n}\n' \
    "$pod" "$1" "$member_port" > "$secrets/vm-host-members.json"
}

up() {
  need_root
  [ -e /dev/kvm ] || fail '/dev/kvm: the local stack runs the VMs the tilo.sh VM hosts run, which need KVM'
  [ -n "${TILO_SH_IMAGE_VM_HOST:-}" ] || fail 'TILO_SH_IMAGE_VM_HOST: the vm-host image is not named'
  [ -n "${TILO_SH_HOSTED_GUEST:-}" ] || fail 'TILO_SH_HOSTED_GUEST: the guest image is not named'
  command -v "$enroll" >/dev/null || fail "$enroll: not found: name the tilo-enroll a release ships (TILO_SH_TILO_ENROLL)"
  local relay relay_key relay_endpoint device loop guest_ref image=$TILO_SH_IMAGE_VM_HOST
  if [ -f "$image" ]; then
    image=$(podman load -q -i "$image" | sed -n 's/^Loaded image[s]*: *//p' | tail -n 1)
    [ -n "$image" ] || fail "$TILO_SH_IMAGE_VM_HOST: podman loaded no image from it"
  fi
  relay=$(relay_device)
  relay_key=$(printf '%s' "$relay" | json_field device_public_key)
  relay_endpoint=$(printf '%s' "$relay" | json_field relay_service_node_id)
  [ -n "$relay_key" ] && [ -n "$relay_endpoint" ] || fail "the relay's own device: $relay"
  device=$(member_identity)
  [ -n "$device" ] || fail "the host's member device"
  members "$device"
  plans
  state_disk
  loop=$(state_loop)
  [ -n "$loop" ] || fail 'the state disk is not attached'
  guest_ref=$(cat "$vm/guest-ref")
  mkdir -p "$vm/placer"
  chown 10001:10001 "$vm/placer"

  # The placer: the same image and command the cloud's placer runs, naming
  # this stack's one host, with no cluster to start hosts on.
  podman rm -f "$placer_container" >/dev/null 2>&1 || true
  podman run -d --name "$placer_container" --network host --log-driver k8s-file \
    --user 10001:10001 --read-only --tmpfs /tmp \
    --security-opt no-new-privileges --cap-drop ALL \
    -v "$vm/placer:/var/lib/tilo-placer" \
    --entrypoint tilo-legacy "$image" \
    hosted place --listen "127.0.0.1:$place_port" \
    --record /var/lib/tilo-placer/placements.json \
    --hosts "$pod=127.0.0.1:$status_port" >/dev/null

  # The host itself: privileged for KVM, the cgroup tree, user namespaces and
  # the jail helper's mounts, with its state disk as a raw block device and
  # its member identity on a read-only mount, as the cloud's StatefulSet runs
  # it. TILO_VM_RELAY_ENDPOINT is where a workspace's browser session reaches
  # the relay, the one address besides the internet a VM may reach.
  podman rm -f "$host_container" >/dev/null 2>&1 || true
  podman run -d --name "$host_container" --hostname "$pod" --network host --privileged \
    --log-driver k8s-file \
    --device "$loop:$state_device" \
    -v "$vm/member-identities:/etc/tilo/member:ro" \
    -v "$plans_file:/etc/tilo/plans/plans.json:ro" \
    -e TILO_VM_STATE_DEVICE="$state_device" \
    -e TILO_VM_PLANS=/etc/tilo/plans/plans.json \
    -e TILO_VM_MEMBER_IDENTITIES=/etc/tilo/member \
    -e TILO_VM_MEMORY_MIB="$memory_mib" \
    -e TILO_VM_STATUS_LISTEN="127.0.0.1:$status_port" \
    -e TILO_VM_GUEST_REF="$guest_ref" \
    -e TILO_VM_RELAY_ENDPOINT="$relay_iroh" \
    -e TILO_TAKE_WORK_RELAY="$relay_iroh" \
    -e TILO_TAKE_WORK_DEVICE="$relay_key" \
    -e TILO_TAKE_WORK_ENDPOINT="$relay_endpoint" \
    "$image" >/dev/null
  say "$pod and the placer are up: workspaces' VMs run here, joining the relay at $relay_iroh"
}

down() {
  need_root
  podman rm -f "$host_container" "$placer_container" >/dev/null 2>&1 || true
  if mountpoint -q "$vm/mnt"; then umount "$vm/mnt"; fi
  local loop
  loop=$(state_loop)
  if [ -n "$loop" ]; then losetup --detach "$loop"; fi
  rm -rf "$vm"
  say 'the VM host is down'
}

status() {
  local container loop
  for container in "$placer_container" "$host_container"; do
    if podman container exists "$container" 2>/dev/null; then
      printf '%-24s %s\n' "$container" "$(podman inspect --format '{{.State.Status}}' "$container")"
    else
      printf '%-24s %s\n' "$container" 'not up'
    fi
  done
  loop=$(state_loop)
  printf '%-24s %s\n' 'state disk' "${loop:-not attached}"
  if podman container exists "$host_container" 2>/dev/null; then
    # Every VM the host holds, with its phase and its memory budget.
    podman exec "$host_container" setpriv --reuid=10001 --regid=10001 --init-groups \
      --inh-caps=-all tilo-legacy hosted status --run /run/tilo-hosted || true
  fi
}

# What the relay reads to place here, and to answer Browser's page actions:
# nothing while the host is down.
relay_env() {
  podman container exists "$host_container" 2>/dev/null || return 0
  local relay_endpoint
  relay_endpoint=$(relay_device | json_field relay_service_node_id)
  cat <<ENV
TILO_HOSTED_PLACER=127.0.0.1:$place_port
TILO_HOSTED_MEMBERS=$relay_secrets/vm-host-members.json
TILO_HOSTED_ACCOUNT=acct-tilo-sh
TILO_HOSTED_VM_SIZE=${TILO_SH_HOSTED_VM_SIZE:-default}
TILO_HOSTED_PLANS=$relay_secrets/vm-host-plans.json
TILO_HOSTED_BROWSER_ACTIONS=$relay_rooms/lounge/relay/browser-actions.jsonl
TILO_HOSTED_BROWSER_RELAY=$relay_endpoint@$relay_iroh
ENV
}

case ${1:-} in
  up) up ;;
  down) down ;;
  status) status ;;
  env) relay_env ;;
  *) fail 'usage: vm-host.sh up|down|status|env' ;;
esac
