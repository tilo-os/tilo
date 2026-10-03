# Dev devices: the slot loop on real boards

## Outcome

Developers and agents put a build of a component onto a physical board (first the Orange
Pi 3B) with the same command and the same mechanism as onto a dev VM: `tilo-vm swap`
writes a new generation of content-addressed sysext layers, restarts the session the
normal way, and `tilo-vm rollback` steps back. Provenance and integrity checks are the
VM's. Nothing is copied into the device's state by hand, and nothing in the board's
session differs from production.

## Principle

A board dev image is the generic dev image plus the board layer (`os/Containerfile.board`),
so it already has tilo-vmd, sshd, sysext and the session template. The extension is two
small pieces, each a generalisation of an existing concept, not a parallel path:

1. **A target without a hypervisor.** The `device` adapter in tilo-vm.
2. **A slot whose session already exists.** The `device` slot in tilo-vmd.

## Target: the `device` adapter

`tilo-vm vm add <name> --ssh root@<host>[:port]` registers a running device in `vms.toml`
(`hypervisor = "device"`, its address, its own key under `keys/`). It is reached exactly as
a VM is: `ssh root@host tilo-vmd rpc` with that key and its own `known_hosts`. The key is
installed once through the dev account, the command `vm add` prints (the UTM and Hyper-V
flow). Every VM command then works; the adapter cannot create, power on, reset or screenshot,
so `vm start` only waits for SSH, `vm restart` reboots over SSH (an adapter capability,
`powers_on`), and `vm create --hypervisor device` points at `vm add`. `vm upgrade` and
`vm rollback-image` are bootc on the device, as on a VM.

## Session: the `device` slot

A board image runs its account's session without a greeter:
`tilo-board-headless-session@<account>` runs `tilo-seat-session`, which runs the compositor
headless in the account's user manager, or on seat0 as the transient unit
`tilo-board-drm-session` while a display is connected. That session is the reserved slot
`device` (like `vm`, a pseudo-slot; kind `device`, protocol 6):

- tilo-vmd finds it from the board layer's marker (`/usr/share/tilo/board-headless-session`)
  and the instance of the template; a unit test ties the names to the board layer's files.
  An image without it answers `no device slot`.
- Activation is the slot path with three differences, all carried by one `Session` value
  (unit, user, units, control socket, stand-in or not): the user is the existing account and
  is never created, changed or removed; the layers drop-in goes to the user manager, the
  board unit and `tilo-board-drm-session` (drop-ins apply to transient units); and the
  greetd-parity check is skipped, since this session is the production one rather than a
  stand-in for it.
- A swap restarts it as it restarts a slot: stop the session unit, restart the user manager,
  start the session unit; readiness waits up to 120 s, because the board unit first waits for
  its user manager, the network and the clock.
- It cannot be created, removed or moved between seats. `rollback --slot device --to 0` is
  the image's own build again.
- `logs`, `slot show`, `slot exec`, `control` and `shot` use the board unit, the account's
  journal and the control socket `tilo-seat-session` sets.

## Architecture-aware sources

- `--from sha:`/`ci:` takes the component out of CI's dev image of the build
  (`registry.tilo.so/tilo/tilo-vm:<tag>-ci`, one index over both architectures). For a
  device the host pulls it for the device's `--arch`, copies out the component's files,
  removes the image and uploads only that layer (adapter capability
  `ci_layers_from_host`); its provenance is the one a pull on the device would write
  (`Provenance::from_ci`: authority, image digest, the commit from the image's revision
  label). A VM keeps pulling in the guest.
- `--builder host` with a `TILO_VM_CARGO` drop-in builds anywhere (the ARM builder); the
  layer's target and rustc are read from the binary it produced, so the device's ABI check
  sees what was really built.
- `--builder vm` builds on the device itself: correct, but slow on a board.

## Not in scope

Seat claims by other slots on a board (the board session owns seat0), power control, and
building board images locally (they are sealed: `vm upgrade` switches to CI's).
