# Concepts

**VM.** A Tilo dev image (production image plus `tilo-vmd`, sshd, toolchain)
running under the host's native hypervisor. Humans watch it in UTM, Hyper-V
Manager or virt-manager (libvirt's QEMU/KVM user session). One VM is shared
by several agents.

**Branch.** A new VM that starts as an exact copy of another one and then
goes its own way: `tilo-vm vm branch <source> <new>...`. Set one box up once
(git and SSH keys, the `t` token, model API keys, logins), then branch as
many VMs off it as you need; each starts fully configured. The source must be
off (`--stop` shuts it down cleanly first). On libvirt and WHPX tilo-vm
freezes the source's disk as a read-only **base** in `bases/` beside it and
records it in `vms.toml`; the source and every branch then get a
copy-on-write child of that base, so branching a 50 GB box takes seconds and
copies nothing, and no VM ever sees another's writes. Branching a branch
freezes its disk into a base over the first. A base is deleted only when no
VM (and no other base) uses it, so `vm rm` of the source or of any branch
never breaks the others; `tilo-vm vm list` shows what each VM was branched
from and which VMs use each base. On UTM each branch is a UTM clone of the
source, which UTM makes copy-on-write (an APFS clone of the VM bundle), so no
base is kept. Hyper-V and the qemu adapter refuse to branch.

What carries over is the whole disk: home, keys, vaults, settings, installed
apps, and the root SSH key tilo-vm uses (each branch has its own copy of the
key file). What is new for each branch is its name, its hypervisor identity
(a libvirt domain UUID, which is also the SMBIOS system UUID the guest reads
from `/sys/class/dmi/id/product_uuid`, or UTM's new clone UUID, passed to the
guest the same way; a MAC; on WHPX QEMU's `-uuid`), and its SSH port. The
UUID is unique per VM and the same at every boot of it. On first boot the
guest sees that `product_uuid` differs from the one recorded beside its
device key, so it knows it was branched and gives itself a new device
identity, keeping its secrets; the source keeps its UUID and so its identity.

**Slot.** Your private piece of a VM: Linux user `tds-<slot>`, session unit
`tilo-vm-session@<slot>`, and an ordered list of layers. Your default slot
is named after your branch; `TILO_VM_SLOT` or `slot = "<name>"` in the
worktree's `.tilo-vm` picks another. A slot's layers
overlay `/usr` for its session and its user services only; other slots and
the VM itself do not see them.

- `headless`: the compositor renders offscreen. Drive it with `control`,
  take pictures with `shot`. Costs no GPU.
- `headful`: gets its own virtual display (a vkms card), its own logind seat
  and, on first `tilo-vm input`, its own keyboard and pointer, so the real
  DRM and libinput paths run. The card is re-created after a VM reboot.
  `--outputs 2560x1600@2,1920x1080` gives it several monitors with their
  own modes and scales (`tilo-vm guide driving`).
  The hypervisor window does not show it; `shot` does, and `seat claim`
  moves the session onto seat0, which the window does show.

**Layer.** An immutable, content-addressed directory (`usr/...`) holding one
component's build, with `provenance.json` saying how it was built. The same
bytes are one layer, whoever built them.

**Generation.** A numbered snapshot of a slot: which layer each component
uses, bottom first (later layers win). `swap` writes a new generation;
`rollback` switches back, `rollback --forward` or `--to <n>` forward again.
Generation 0 is the bare VM image.

**The `vm` slot.** Components with VM-wide scope (`fs`, greeter, system
services) are swapped into the reserved slot `vm`. Its layers merge into the
VM's own `/usr`, so they affect every slot.

**Device and the `device` slot.** A real machine that already runs a Tilo
dev image, such as a board, registered with `tilo-vm vm add` and reached over
SSH instead of a hypervisor, as root or as its owner's account through
`sudo -n`. An always-on board image runs its account's own session without a
greeter; the reserved slot `device` is that session, so its layers reach the
board's real session and a swap restarts it. On a laptop the `device` slot is
the owner's Tilo login through greetd on the built-in display: its layers
merge into the machine's `/usr` (with the `vm` slot's) and a swap restarts
the owner's desktop in place, without logging them out (`tilo-vm guide
devices`).

**Seat.** `seat0` is the VM's real display, the one the hypervisor window
shows. It runs greetd until a slot claims it (`tilo-vm seat claim`); the
claim is a lease that records who holds it.

**Authority.** Layers you build (in the VM or on the host) are
`authority = "local"`. They exist to test; they are never promoted to an
image. Layers taken from a published CI image (`--from ci:<tag>`) are
`authority = "ci"`.
