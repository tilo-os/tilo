# Tilo VM host

One per VM-host node of a Tilo host that serves other people's workspaces
(tilo.sh): `tilo-legacy hosted serve`, which runs each
workspace's Linux programs and its Browser in one jailed Firecracker VM
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
- **The node's memory** (optional): `TILO_VM_MEMORY_MIB`, the node's memory
  its VMs are packed into, or `auto` for all of it. VMs are admitted by what
  they really take, which the service reads from each VM's processes every
  second (their proportional set sizes: a clone of the warm template with
  Browser open is about 325 MiB, whatever size it was configured at), never by
  the size they may grow into. `TILO_VM_MEMORY_RESERVE_MIB` (2 GiB) stays free
  for this pod, each VM's helper processes and the room a balloon needs to
  give memory back, and `TILO_VM_MEMORY_HEADROOM_MIB` (128 MiB) is added to
  what a VM is counted at until the kernel has measured it. A wake that does
  not fit waits; when the node's free memory falls under the reserve, the VMs'
  balloons are asked for memory back first, and a VM is never killed for room.
- **A placer's view** (optional): `TILO_VM_STATUS_LISTEN`, a TCP address
  that answers the service's Status alone, for `tilo-legacy hosted place`.
- **Plans** (optional): `TILO_VM_PLANS`, the account plans file the service
  admits wakes against.
- **The guest**: the digest this build pins (`os/hosted-guest-snapshot.env`),
  or `TILO_VM_GUEST_REF`; either way only an image the release key signed.
- **Browser** (optional): `TILO_VM_RELAY_ENDPOINT` (`ADDR:PORT`) has the
  host serve each workspace's Browser in its VM: the browser session there
  joins its workspace through the relay at that address, the relay's address
  as a VM reaches it and the only address besides the internet a VM may
  reach. A relay on the host's own loopback (a local stack) is reached at
  `10.0.2.4`, which `pasta` maps there. A session ends after ten minutes
  without page activity, or two hours alive; the VM then sleeps like any.

| | |
| --- | --- |
| service | uid/gid 10001 (`tilo-hosted`), socket `/run/tilo-hosted/hosted.sock` |
| VM slots | 1024 (as many as any node's memory holds), uids 200000–202047 |
| VM network | pasta per VM, DNS 1.1.1.1, 200 Mbit/s each way |
| Firecracker | 1.17.0, with its jailer |
