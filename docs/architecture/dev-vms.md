# Dev VMs, slots and dev devices

How `tilo-vm` lets people and agents test Tilo OS builds in shared VMs and on
real boards. The how-to lives in [`crates/dev/tilo-vm/guide/`](../../crates/dev/tilo-vm/guide/);
this page records the design and the reasons behind it.

## Outcome

One to a few shared dev VMs serve many developers and agents on macOS, Windows
or Linux hosts. Each gets a **slot** inside a VM: its own Linux user
(`tds-<slot>`), its own `/usr` layer stack and its own session. Switching a
component to another build is one command and never needs a new VM or a
reboot. Humans watch VMs in the host's native hypervisor UI (UTM, Hyper-V
Manager, virt-manager/Boxes); `tilo-vm` has thin adapters, not a VM manager.

## Principle: no dev/prod drift

Dev tooling reuses production mechanisms. Where a dev-only piece is
unavoidable, it is small, sits in front of the production path rather than
beside it, and a contract check catches divergence.

- A session starts via PAM and then `/usr/libexec/tilo-session`, exactly as
  greetd starts it. Everything after the PAM session is production code.
- A component swap is a **normal session restart** (stop the session unit,
  restart `user@<uid>`, start the session unit). There is no hot-swap path.
- One component manifest, `os/components.toml`, drives both the production
  Containerfile and the dev layers. A policy check fails if the image has Tilo
  binaries the manifest does not name, or the manifest names paths the image
  lacks.
- Environment overrides for testing sit behind the `dev-overrides` cargo
  feature. Dev binaries are release builds with it; a contract check keeps it
  out of production images.

## Session topology

| Kind | Display | Seat | Entered via |
|---|---|---|---|
| Seat 0 | the VM's real GPU, shown in the hypervisor UI | `seat0` + VT | greetd, the greeter, then `tilo-session --backend drm` |
| Headful slot | a `vkms` card created through configfs; costs no GPU | its own logind seat (`ID_SEAT` udev tag) | `tilo-vm-session@<slot>.service` (`PAMName=greetd`), then `tilo-session --backend drm` |
| Headless slot | offscreen | none | the same unit, then `tilo-session --backend headless` |

- The DRM backend scans out through CPU-written dumb buffers, so vkms cards
  need no multi-GPU or PRIME work. Rendering uses the shared render node, or
  llvmpipe when there is none.
- A headful slot drives the real libinput path through per-seat `uinput`
  devices, held by a transient `tilo-vm-input-<slot>` unit because a uinput
  device dies with its creator. vkms connectors have a fixed count; hotplug
  toggles connector status.
- `tilo-vm seat claim <slot>` stops greetd and restarts that slot's session on
  `seat0`/vt1; `release` hands the seat back. Both are normal session restarts.
- The image's kernel must provide vkms with its configfs interface and
  uinput; `os/scripts/check-dev-kernel.sh` fails the dev image build otherwise.

## Slots, layers and swaps

- A **layer** is an immutable, content-addressed sysext directory under
  `/var/lib/tilo-vm/layers/<sha256>/` with `provenance.json`: component, source
  commit and dirty-tree hash, builder image digest, target triple, Cargo.lock
  hash and toolchain. Layers are SELinux-labelled at import.
- A **slot** is a user, a kind (`headful`, `headless`) and an ordered list of
  layers with numbered generations. Its layers apply through
  `ExtensionDirectories=` to both its session unit and `user@<uid>.service`, so
  the compositor and its user services see one build. Other slots and the VM do
  not see them.
- VM-wide components (`fs`, the local model, the greeter) live in the
  pseudo-slot `vm`, merged into the VM's `/usr` with `systemd-sysext refresh`.
- `tilo device swap <component> --from worktree|sha:<commit>|layer:<hash>` resolves
  the source to a layer, writes a new generation and applies it. Session
  components restart the session; apps alone install as self-signed `.tapp`
  packages into the slot user's store, which production already shadows, so
  they are live at the next launch; VM-wide components refresh sysext and
  restart the affected unit. `tilo-vm rollback` re-applies the previous
  generation the same way.
- Boot-level components (kernel, UKI, bootloader, initramfs, installer) are not
  swappable; `swap` points to the `os/dev/` scenario drivers instead.

## Builders and images

Every builder produces the same layer format:

- `vm` (default): the worktree syncs into the VM and builds with the dev
  image's toolchain, the same builder as the base image.
- `host`: builds on the host or any cargo drop-in; the layer's target and rustc
  are read from the binary it produced, so the ABI check sees what was built.
- `ci`: takes the component's manifest paths out of CI's dev image of a commit.

An ABI check refuses a layer whose builder differs from the base image's, and
apps whose runtime does not match `libruntime_dylib.so`. Local layers are never
promoted; CI rebuilds from source.

`os/Containerfile.dev` builds from the production image of the same commit and
adds `tilo-vmd`, sshd and the toolchain. Its `/usr` is unsealed so sysext can
merge into it. Upgrades are `bootc switch`; slots survive because they live in
`/var`. `tilo-vm` reaches the guest daemon `tilo-vmd` by JSON-RPC over SSH stdio,
the same transport on every hypervisor. Every command takes `--json`, is
idempotent, and long or state-changing commands write a receipt.

## Real boards

A board dev image is the dev image plus the board layer
(`os/Containerfile.board`), so a board gets the same swap and rollback as a VM
through two generalisations rather than a parallel path:

- **A target without a hypervisor.** The `device` adapter registers a running
  board by SSH address. It cannot create, power on, reset or screenshot, so
  `vm start` waits for SSH and `vm restart` reboots over SSH.
- **A slot whose session already exists.** A board runs its account's session
  without a greeter. That session is the reserved pseudo-slot `device`: its
  user is the existing account and is never created or removed, and the
  greetd-parity check is skipped because this session is the production one.
  It cannot be created, removed or moved between seats.

For `--from sha:`, the host pulls CI's dev image for the board's architecture,
extracts the component and uploads only that layer, with the provenance a pull
on the board would record; extracted layers stay cached on the host by image
digest. Building on the board itself works but is slow.

## Host artifact cleanup (proposed)

`tilo device gc` collects guest layers and build targets; installer scenarios also
leave downloads, converted disks, overlays, firmware variables and proofs on
the host. The proposed design: every scenario writes an `artifacts.json`
ownership record (canonical path, file identity, run and step, kind,
allocated bytes, backing references, retention state) before allocating a
large file. External targets, physical sources and registered VM disks are
never owned. `tilo device scenario gc --dry-run` lists exactly what a run could
remove and why other files stay; `--apply <plan-id>` revalidates the plan's
hash under the registry lock, reconciles running QEMU processes, mounted
images and qcow2 backing edges, removes children before backing files and
refuses symlinks or unknown backing graphs. Failed runs keep their scratch
files as evidence until explicitly selected.

## Later and out of scope

Cloud dev VMs stay cheap to add as long as the hypervisor adapter stays narrow
(create, start, stop, destroy, SSH endpoint) and the `vm` builder can fetch
sources by commit. They would need a provider adapter, an image import and a
way to see `seat0` remotely. Frame streaming, remote display and hot reload of
a running compositor are out of scope.
