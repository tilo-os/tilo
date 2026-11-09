# Tilo VM host

One per VM-host node of a Tilo host that serves other people's workspaces
(tilo.sh): `tilo-legacy hosted serve`, which runs each workspace
environment's Linux programs in a jailed Firecracker VM
([`docs/app-model/environments.md`](../../../docs/app-model/environments.md),
"Hosted workspace VMs"). `entrypoint.sh` says what the start does.

## What the pod needs

- **Privileged**, on a node with `/dev/kvm` and `/dev/net/tun`: the start
  writes `kernel.apparmor_restrict_unprivileged_userns` (Ubuntu) and arranges
  the cgroup tree, and the jail helper mounts, chroots and changes uids.
- **A raw block volume** at `TILO_VM_STATE_DEVICE` (default
  `/dev/tilo-vm-state`) that outlives the node: every VM's disk, memory and
  snapshot, the jails and the guest image. It is made XFS with reflink the
  first time.
- **The eviction notice** (optional): `TILO_VM_EVICT_NOTICE` names a file
  whatever watches the cloud writes when the node is being taken back; every
  VM is then suspended onto the volume, and the next host the volume follows
  restores each on its next command.
- **A memory budget** (optional): `TILO_VM_MEMORY_MIB`, the most the VMs
  awake at once may take together by their sizes, or `auto`: the node's
  memory less `TILO_VM_MEMORY_RESERVE_MIB` (3.5 GiB), in whole GiB. A wake
  past it waits for room.
- **A placer's view** (optional): `TILO_VM_STATUS_LISTEN`, a TCP address
  that answers the service's Status alone, for `tilo-legacy hosted place`.
- **Plans** (optional): `TILO_VM_PLANS`, the account plans file the service
  admits wakes against.
- **The guest**: the digest this build pins (`os/hosted-guest-snapshot.env`),
  or `TILO_VM_GUEST_REF`; either way only an image the release key signed.

| | |
| --- | --- |
| service | uid/gid 10001 (`tilo-hosted`), socket `/run/tilo-hosted/hosted.sock` |
| VM slots | 16, uids 200000–200031 |
| VM network | pasta per VM, DNS 1.1.1.1, 200 Mbit/s each way |
| Firecracker | 1.17.0, with its jailer |
