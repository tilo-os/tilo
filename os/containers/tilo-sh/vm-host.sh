#!/usr/bin/env bash
# A VM host of the local tilo.sh stack: the image tilo.sh's VM hosts run
# (os/containers/tilo-vm-host), privileged and with a state disk of its own,
# and the guest image this checkout built instead of a release's. Its member
# device is a server of the person, which takes a workspace's Linux turns. It is what a hosted workspace needs
# under it locally: a workspace's Linux programs and its Browser run in its VM
# here, and its browser session joins the workspace at the server beside it.
#
# usage: vm-host.sh up|down [--wipe]|status|env
#
#   up       make the state disk, certify this host's member device under the
#            server's root, ready the guest image, start vm-host-0
#   down     stop it; the state disk and the host's identity stay, so the
#            next up is the same member device with its VMs and the
#            workspaces' files it keeps. --wipe removes them too
#   status   what is up, and the VMs the host holds
#   env      the variables the server reads to run its workspaces' Linux
#            work here (nothing while the host is down)
#
# What a caller passes (each has the default shown):
#
#   TILO_SH_STATE=/var/lib/tilo-sh      this host's own state directory
#   TILO_SH_IMAGE_VM_HOST               the vm-host image: its OCI archive, or
#                                       a reference podman already holds
#   TILO_SH_HOSTED_GUEST=<dir>          the image-hosted-guest check's outputs:
#                                       an image.oci.tar with the digest beside
#                                       it
#   TILO_VM_TICKET                      the ticket this host joins the person's
#                                       devices with, printed by `tilo device
#                                       add` on an owner device (spent once,
#                                       on this host's disk)
#   TILO_SH_HOSTED_VM_SIZE=default      the size a workspace's VM runs at, as
#                                       the plans file names it (`small`,
#                                       `default`, `large`, `xlarge`): the
#                                       size production places, unless a
#                                       machine with less room places smaller
#   TILO_SH_VM_HOST_MEMORY_MIB          what the host holds awake at once: by
#                                       default the size's own memory (8 GiB
#                                       at `default`), the reserve it keeps
#                                       free (2 GiB) and room for a clone of
#                                       the size's warm template (1 GiB), so
#                                       the machine's room is what bounds it,
#                                       not this stack's default
#   TILO_SH_SERVER_SECRETS=<state>/secrets
#                                       where the server's secrets are on this
#                                       machine: the plans file is written
#                                       there, for the server to read
#   TILO_SH_SERVER_SECRETS_VIEW=/run/secrets
#                                       where that server reads them (the image
#                                       mounts them at /run/secrets; the
#                                       native stack's server reads them in the
#                                       stack's own state)
#   TILO_SH_SERVER_WORKSPACES_VIEW=/run/tilo-workspaces
#                                       where that server reads its own state
#                                       (the image's volume, or natively the
#                                       stack's state)
#   TILO_SH_SERVER_STATE=<state>/workspaces/lounge/server
#                                       the server's own state, where the
#                                       device key it answers as is
#   TILO_SH_SERVER_IROH=127.0.0.1:24104  where the server's endpoint listens
#
# Root, /dev/kvm and a loop device are what a VM host needs: without them
# there is nothing to run, and `up` says so and stops.
set -euo pipefail

state=${TILO_SH_STATE:-/var/lib/tilo-sh}
vm=$state/vm
# The server's secrets: where it reads the plans file from, which may be a
# stack's directory rather than this host's.
secrets=${TILO_SH_SERVER_SECRETS:-$state/secrets}
pod=vm-host-0
host_container=tilo-sh-$pod
server_iroh=${TILO_SH_SERVER_IROH:-127.0.0.1:24104}
server_state=${TILO_SH_SERVER_STATE:-$state/workspaces/lounge/server}
server_cli=${TILO_SH_TILO_SERVER:-tilo-server}
# What the host holds awake at once: the size's warm template (a template is
# counted at its size's whole memory, since warming it fills its guest), a
# clone of that template awake beside it (a third of a gigabyte, measured), and
# the host's own reserve (its service keeps 2 GiB free). A host that admits the
# template and then refuses the VM it is for wakes that workspace cold, which is
# the boot the template exists to save, so the size's own memory alone is not
# enough here: 8 GiB VMs need more than 10 GiB.
case ${TILO_SH_HOSTED_VM_SIZE:-default} in
  small) size_memory_mib=4096 ;;
  large) size_memory_mib=16384 ;;
  xlarge) size_memory_mib=32768 ;;
  *) size_memory_mib=8192 ;;
esac
memory_mib=${TILO_SH_VM_HOST_MEMORY_MIB:-$((size_memory_mib + 3072))}
# The server's own view of what this host writes for it: the image's secrets
# volume and state volume, or, for the stack that runs the server natively, the
# paths of its state (TILO_SH_SERVER_SECRETS_VIEW, TILO_SH_SERVER_WORKSPACES_VIEW).
server_secrets=${TILO_SH_SERVER_SECRETS_VIEW:-/run/secrets}
server_workspaces=${TILO_SH_SERVER_WORKSPACES_VIEW:-/run/tilo-workspaces}
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

# The server's endpoint id: an iroh endpoint is keyed apart from the device it
# signs its runs as, and a VM's Browser session reaches the server there.
# `tilo-server node-id` derives it from the device key the server keeps.
server_endpoint() {
  [ -f "$server_state/host-device.key" ] \
    || fail "$server_state/host-device.key: the server has not started yet: start the stack's server first"
  "$server_cli" node-id --key-file "$server_state/host-device.key"
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

# This host's member device, as the server reads it: its device key, which the
# ticket's login left on the host's own state disk. The start joins the person's
# devices once, so the key is read back from the running host, never made here.
member_device() {
  local device started=$SECONDS
  while ((SECONDS - started < 180)); do
    device=$(podman exec "$host_container" setpriv --reuid=10001 --regid=10001 --init-groups \
      --inh-caps=-all -- env HOME=/var/lib/tilo-hosted/member/home \
      XDG_STATE_HOME=/var/lib/tilo-hosted/member/profile/state \
      TILO_IDENTITY_ROOT=/var/lib/tilo-hosted/member/identity \
      tilo device key 2>/dev/null |
      json_field device)
    if [ -n "$device" ]; then printf '%s' "$device"; return 0; fi
    [ "$(podman inspect --format '{{.State.Running}}' "$host_container" 2>/dev/null)" = true ] \
      || fail "the host stopped before it joined: $(podman logs --tail 3 "$host_container" 2>&1)"
    sleep 2
  done
  fail "the host joined no identity in 180 s: set TILO_VM_TICKET to a ticket \`tilo device add\` printed (podman logs $host_container says why)"
}

# The plans file the server and the host admit by (docs/hosted-browser.md,
# "Metering"): five free Browser page actions, so the card prompt comes after
# them, and the stack's account with a card on file for the Linux VMs and for
# what follows the card — the account the server places its workspaces' VMs for,
# so a proof run inside one names the account that owns it.
# TILO_SH_BROWSER_FREE_ACTIONS sets another allowance: a check that counts page
# actions gives itself few of them. The file lives in
# the secrets the stack mounts for the server, which reads it again for every
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
    "acct-tilo-sh": {
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

up() {
  need_root
  [ -e /dev/kvm ] || fail '/dev/kvm: the local stack runs the VMs the tilo.sh VM hosts run, which need KVM'
  [ -n "${TILO_SH_IMAGE_VM_HOST:-}" ] || fail 'TILO_SH_IMAGE_VM_HOST: the vm-host image is not named'
  [ -n "${TILO_SH_HOSTED_GUEST:-}" ] || fail 'TILO_SH_HOSTED_GUEST: the guest image is not named'
  [ -n "${TILO_VM_TICKET:-}" ] || [ "${TILO_VM_FOUND:-0}" = 1 ] \
    || fail 'TILO_VM_TICKET: the ticket `tilo device add` printed on an owner device'
  local device loop guest_ref image=$TILO_SH_IMAGE_VM_HOST
  if [ -f "$image" ]; then
    image=$(podman load -q -i "$image" | sed -n 's/^Loaded image[s]*: *//p' | tail -n 1)
    [ -n "$image" ] || fail "$TILO_SH_IMAGE_VM_HOST: podman loaded no image from it"
  fi
  plans
  state_disk
  loop=$(state_loop)
  [ -n "$loop" ] || fail 'the state disk is not attached'
  guest_ref=$(cat "$vm/guest-ref")

  # The host itself: privileged for KVM, the cgroup tree, user namespaces and
  # the jail helper's mounts, with its state disk as a raw block device, as the
  # cloud's StatefulSet runs it. It joins the person's devices with the ticket
  # on first start, and TILO_VM_SERVER_ENDPOINT is where a workspace's browser
  # session reaches the server, the one address besides the internet a VM may
  # reach.
  podman rm -f "$host_container" >/dev/null 2>&1 || true
  podman run -d --name "$host_container" --hostname "$pod" --network host --privileged \
    --log-driver k8s-file \
    --device "$loop:$state_device" \
    -v "$plans_file:/etc/tilo/plans/plans.json:ro" \
    -e TILO_VM_STATE_DEVICE="$state_device" \
    -e TILO_VM_PLANS=/etc/tilo/plans/plans.json \
    -e TILO_VM_TICKET="${TILO_VM_TICKET:-}" \
    -e TILO_VM_FOUND="${TILO_VM_FOUND:-0}" \
    -e TILO_VM_MEMORY_MIB="$memory_mib" \
    -e TILO_VM_GUEST_REF="$guest_ref" \
    -e TILO_VM_SERVER_ENDPOINT="$server_iroh" \
    -e TILO_DEVICE_PORT="$member_port" \
    "$image" >/dev/null
  # The member's device key is the ticket's login's, made on the host's own
  # disk: read back once it joined, as the server's person's server.
  device=$(member_device)
  [ -n "$device" ] || fail "the host's member device"
  say "$pod is up as $device: workspaces' VMs run here, joining the server at $server_iroh"
}

down() {
  need_root
  podman rm -f "$host_container" >/dev/null 2>&1 || true
  if mountpoint -q "$vm/mnt"; then umount "$vm/mnt"; fi
  local loop
  loop=$(state_loop)
  if [ -n "$loop" ]; then losetup --detach "$loop"; fi
  if [ "${1:-}" = --wipe ]; then
    rm -rf "$vm"
    say 'the VM host is down, and its state disk and identity are gone'
  else
    say "the VM host is down; its state disk and identity stay in $vm (down --wipe removes them)"
  fi
}

status() {
  local container loop
  for container in "$host_container"; do
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
      --inh-caps=-all tilo-linux-vm hosted status --run /run/tilo-hosted || true
  fi
}

# What the server reads to run its workspaces' Linux work on the devices that
# take it, and to answer Browser's page actions: nothing while the host is down.
server_env() {
  podman container exists "$host_container" 2>/dev/null || return 0
  local endpoint
  endpoint=$(server_endpoint)
  cat <<ENV
TILO_HOSTED_ACCOUNT=acct-tilo-sh
TILO_HOSTED_VM_SIZE=${TILO_SH_HOSTED_VM_SIZE:-default}
TILO_HOSTED_PLANS=$server_secrets/vm-host-plans.json
TILO_SERVER_BROWSER_ACTIONS=$server_workspaces/lounge/server/browser-actions.jsonl
TILO_HOSTED_BROWSER_SERVER=$endpoint@$server_iroh
ENV
}

case ${1:-} in
  up) up ;;
  down) down "${2:-}" ;;
  status) status ;;
  env) server_env ;;
  *) fail 'usage: vm-host.sh up|down [--wipe]|status|env' ;;
esac
