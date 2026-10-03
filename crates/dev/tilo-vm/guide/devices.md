# Devices: boards and other real machines

A **device** is a real machine that already runs a Tilo dev image, such as an
Orange Pi installed from its board dev disk. tilo-vm reaches it over SSH
instead of a hypervisor. Everything else is the VM loop: the same slots,
layers, generations, `swap`, `rollback` and provenance checks.

## Register it

```sh
tilo-vm vm add opi --ssh root@10.0.0.108   # makes keys/opi_ed25519, prints the key install
ssh -t -o StrictHostKeyChecking=accept-new -p 22 quickuser@10.0.0.108 'sudo install ...'   # the printed command, once
tilo-vm vm status opi                      # reachable, tilo-vmd version and architecture
tilo-vm vm use opi                         # this worktree (writes .tilo-vm); --global for every worktree
```

`vm add` makes the device's own root key beside `vms.toml` and asks the
device for tilo-vmd. A new device does not know the key yet, so `vm add`
prints the one command that installs it for root, with the dev account
`quickuser`'s password (`tilo`, from `os/account.env`; SSH asks for it, then
sudo). After that every connection is root with that key and the device's
own `known_hosts`, never a password. Without a terminal to type the
password into, an agent asks a human to run the printed command.

The device must already run a board dev image (`tilo-vm:<board>-ci`, which
has tilo-vmd): `vm status` says when it does not, and `vm upgrade` moves it
onto one (below).

## Your build on the device's own session

A board image runs its account's session without a greeter
(`tilo-board-headless-session@<account>`): headless, or on the display
once one is connected. That session is the **`device` slot**:

```sh
tilo-vm swap --slot device shell --from sha:1a2b3c4   # CI's aarch64 build of a main commit
tilo-vm swap --slot device shell                      # your worktree (see Builds below)
tilo-vm status --slot device                          # generation, session, components
tilo-vm slot show device                              # every file and its sha256 and layer
tilo-vm slot exec --slot device -- sha256sum /usr/bin/shell
tilo-vm logs device
tilo-vm control --slot device state-dump --pretty
tilo-vm rollback --slot device                        # the generation before
tilo-vm rollback --slot device --to 0                 # the image's own shell again
```

A swap writes the layers as drop-ins (`ExtensionDirectories=`) for that
session's unit, the account's user manager and the unit the session runs its
compositor in on a display, then restarts the session the normal way: stop
it, restart the user manager, start it. Its account and identity stay as
they are; the device slot never creates or removes a user. It cannot be
created, removed or moved to another seat: it is the machine's session.

Set `slot = "device"` in the worktree's `.tilo-vm` (or `TILO_VM_SLOT=device`)
to make it the default. Your own slots (`tilo-vm slot create --headless`)
work on a device too, beside the device's session.

## Builds

- **CI** (`--from sha:<commit>`, `--from ci:<tag>`): the device pulls the
  build of its own architecture out of CI's dev image of that commit,
  `registry.tilo.so/tilo/tilo-vm:main-<sha7>-ci`. The aarch64 lane builds the
  newest main commit only, so check that a commit has arm64 first
  (`skopeo inspect --raw docker://registry.tilo.so/tilo/tilo-vm:main-<sha7>-ci`).
  The pull needs room for the whole image on the device; `tilo-vm status`
  shows its free space.
- **Your worktree on a builder of the device's architecture**
  (`--builder host` with `TILO_VM_CARGO`): any cargo drop-in that builds for
  aarch64 and leaves the outputs in this worktree's target dir. The layer's
  target and rustc are read from the binary it built, and the device refuses
  any other architecture.
- **Your worktree on the device** (`--builder vm`, the default when the
  device's image can build): it works, but a board compiles a release shell
  for hours. Use it on a fast aarch64 machine, or for small components.

Each layer records its provenance (`authority = "ci"` for CI builds, `local`
for everything built from a worktree), which `slot show --json` and the
device's `/var/lib/tilo-vm/layers/<hash>/provenance.json` show.

## The device's image

```sh
tilo-vm vm status opi                    # tilo-vmd, protocol, architecture
tilo-vm vm ssh opi -- bootc status       # the image it runs and follows
tilo-vm vm upgrade opi                   # the newest build of the image it follows, then a reboot
tilo-vm vm upgrade opi --image registry.tilo.so/tilo/tilo-vm:orangepi-3b-v2-1-ci
tilo-vm vm rollback-image opi
tilo-vm vm restart opi                   # reboot over SSH; wait for the new boot and tilo-vmd
```

A board's image is sealed, so an upgrade is a bootc switch and a reboot,
never a local build. tilo-vm cannot power a device on: `vm start` only waits
for SSH, `vm restart` reboots over SSH, and after `vm stop` someone has to
power it on. `vm create`, `vm reset`, `vm serial`, `vm shot` and
`vm control` need a hypervisor and say so. `vm rm opi` forgets the device
and its key; the device is untouched.

## When something is off

- `vm add` or `vm status` says the key was refused: run the install command
  it prints (once per device and key).
- `no device slot`: the image runs no session of its own (a VM, or a board
  image without one); use your own slot.
- The swap says the session did not start: `tilo-vm logs device` names the
  cause; `tilo-vm rollback --slot device` brings the previous build back.
- A layer is refused with `target`: it was built for another architecture.
