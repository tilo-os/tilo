# Concepts

**VM.** A Tilo dev image (production image plus `tilo-vmd`, sshd, toolchain)
running under the host's native hypervisor. Humans watch it in UTM, Hyper-V
Manager or virt-manager. One VM is shared by several agents.

**Slot.** Your private piece of a VM: Linux user `tds-<slot>`, session unit
`tilo-vm-session@<slot>`, and an ordered list of layers. A slot's layers
overlay `/usr` for its session and its user services only; other slots and
the VM itself do not see them.

- `headless`: the compositor renders offscreen. Drive it with `control`,
  take pictures with `shot`. Costs no GPU.
- `headful`: gets its own virtual display (a vkms card), its own logind seat
  and, on first `tilo-vm input`, its own keyboard and pointer, so the real
  DRM and libinput paths run. The card is re-created after a VM reboot.

**Layer.** An immutable, content-addressed directory (`usr/...`) holding one
component's build, with `provenance.json` saying how it was built. The same
bytes are one layer, whoever built them.

**Generation.** A numbered snapshot of a slot: which layer each component
uses, bottom first (later layers win). `swap` writes a new generation;
`rollback` switches back. Generation 0 is the bare VM image.

**The `vm` slot.** Components with VM-wide scope (`fs`, greeter, system
services) are swapped into the reserved slot `vm`. Its layers merge into the
VM's own `/usr`, so they affect every slot.

**Seat.** `seat0` is the VM's real display, the one the hypervisor window
shows. It runs greetd until a slot claims it (`tilo-vm seat claim`); the
claim is a lease that records who holds it.

**Authority.** Layers you build (in the VM or on the host) are
`authority = "local"`. They exist to test; they are never promoted to an
image. Layers taken from a published CI image (`--from ci:<tag>`) are
`authority = "ci"`.
