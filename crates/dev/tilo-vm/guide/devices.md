# Devices: boards and other real machines

A **device** is a real machine that already runs a Tilo dev image, such as an
Orange Pi installed from its board dev disk or a Lenovo Yoga Slim 7x installed
from its board dev image. tilo-vm reaches it over SSH instead of a
hypervisor. Everything else is the VM loop: the same slots, layers,
generations, `swap`, `rollback`, `shot`, `control` and provenance checks.
Never hot-fix a device by hand (`git archive | ssh`, `bootc usr-overlay`,
`install` into `/usr/bin`): nothing can roll that back, and the next boot
loses it.

## Register it

```sh
tilo-vm vm add opi --ssh root@10.0.0.108   # makes keys/opi_ed25519, prints the key install
ssh -t -o StrictHostKeyChecking=accept-new -p 22 tester@10.0.0.108 'sudo install ...'   # the printed command, once
tilo-vm vm status opi                      # reachable, tilo-vmd version and architecture
tilo-vm vm use opi                         # this worktree (writes .tilo-vm); --global for every worktree
```

`vm add` makes the device's own root key beside `vms.toml` and asks the
device for tilo-vmd. A new device does not know the key yet, so `vm add`
prints the one command that installs it for root, with the dev account
`tester`'s password (`tilo`, from `os/account.env`; SSH asks for it, then
sudo). After that every connection is root with that key and the device's
own `known_hosts`, never a password. Without a terminal to type the
password into, an agent asks a human to run the printed command.

The device must already run a board dev image (`tilo-vm:<board>-ci`, which
has tilo-vmd): `vm status` says when it does not, and `vm upgrade` moves it
onto one (below).

## As your own account (a laptop)

A machine installed with an owner SSH key (the installer's dev SSH setup) lets
only that account in: `PermitRootLogin no`, `AllowUsers <owner>`. There
tilo-vm logs in as the owner with the owner's key and runs every remote
command as root through `sudo -n`:

```sh
tilo-vm vm add yoga --ssh me@10.0.0.223 --key ~/.ssh/yoga_ed25519
tilo-vm vm add yoga --ssh me@10.0.0.223 --key ~/.ssh/yoga_ed25519 --grant-sudo   # if the root check failed
tilo-vm vm use yoga
```

`vm add` checks, as that account, everything the other commands rely on and
prints each with its fix:

| Check | Needed for | Fix when it fails |
|---|---|---|
| `ssh` | everything | the key (`--key`), the address, the device's sshd |
| `root` | everything: tilo-vmd, the VM lock, bootc run as root | `--grant-sudo`, or `--ssh root@` |
| `tilo-vmd` | slots, swaps, `shot`, `control` | a dev image; an older protocol registers anyway, so `vm upgrade` can move it on |
| `toolchain` | `swap` from your worktree on the device (cargo, rsync, python3) | a current board dev image, or CI builds (`--from sha:`) |
| `bootc` | `vm upgrade`, `vm rollback-image` | a bootc dev image |

It registers the device only once `ssh`, `root` and `tilo-vmd` pass; rerun
it after a fix (it is idempotent). `--grant-sudo` writes
`/etc/sudoers.d/tilo-vm-<user>` (`<user> ALL=(ALL) NOPASSWD: ALL`, checked
by `visudo` before it is moved in) over an interactive SSH session: sudo asks
for that account's own login password (the owner's, not the dev account's)
once, on your terminal, and it is never stored. From then on whoever holds
that key is root on the machine without a password, as the root key of a VM
is. An agent without a terminal asks a human to run it. When the machine
stops being a dev target, revoke it (a write into `/etc`, hence `--raw`):
`tilo-vm vm ssh yoga --raw -- rm /etc/sudoers.d/tilo-vm-<user>`.

## A laptop's display: your slot on seat0

A laptop's image logs its owner in through greetd; it has no `device` slot.
Use your own slot, as on a VM, and move it onto the built-in display when you
want to see it there:

```sh
tilo-vm slot create --headless --identity   # your session beside the owner's, with a throwaway identity
tilo-vm swap shell                          # build on the device (or --from sha:<commit>), restart your session
tilo-vm slot exec -- sha256sum /usr/bin/shell   # the binary your session runs
tilo-vm shot -o target/yoga.png             # your compositor's own frame; no screen grab
tilo-vm control actions list                # the typed actions, as on any slot
tilo-vm seat claim                          # your session (headless or headful) takes the built-in display and input (VT 1)
tilo-vm shot -o target/panel.png            # the same compositor's frame, now the one on the panel
tilo-vm seat release                        # greetd's login screen comes back
tilo-vm rollback                            # undo the swap (--to 0: the image's own shell)
tilo-vm slot rm                             # done: your slot's session and user go
```

The owner's own session is never touched by a swap: your layers reach only
your slot. Three things do reach the person at the machine, so ask before
each on a machine someone is using:

- `seat claim` stops greetd, which ends whoever is logged in on the display.
  The claim is kept across reboots (`tilo-vm-restore.service` starts your
  session again, still on seat0), so `seat release` when you are done.
- `vm restart`, `vm upgrade` and `vm rollback-image` reboot the machine
  (an upgrade reboots by itself; no `vm restart` after it), which ends every
  session on it. A disk that asks for a passphrase at boot waits at the
  machine's keyboard each time.
- VM-wide swaps (`--slot vm`, `tilo-vm guide swapping`) change `/usr` for
  everyone; the greeter's restarts greetd and with it the owner's session.

On a laptop, building on the device is fine: release layers link without LTO
(`tilo-vm guide swapping`), and a laptop has the memory. Your slot, its
layers and generations live in `/var` and survive image upgrades.

## Your build on the device's own session

A board image runs its account's session without a greeter
(`tilo-board-headless-session@<account>`): headless, or on the display
once one is connected. That session is the **`device` slot**:

```sh
tilo-vm swap --slot device shell --from sha:1a2b3c4   # CI's aarch64 build of a main commit; only the layer travels
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

- **CI** (`--from sha:<commit>`, `--from ci:<tag>`): this host (it needs
  podman) pulls CI's dev image of that commit for the device's architecture,
  `registry.tilo.so/tilo/tilo-vm:main-<sha7>-ci`, copies out only the
  component's files, removes the image again, and uploads just that layer
  with its provenance (`authority = "ci"`, the image's digest and the commit
  CI built). The board never downloads the multi-gigabyte image. The layer
  stays in this host's cache (`<cache>/tilo-vm/ci-layers`, the six most
  recently used) under the image's digest, so swapping the same build again
  costs one registry lookup (`skopeo inspect --raw`) and no pull, and no
  upload either when the device still holds that layer (after a rollback, say). The aarch64
  lane builds the newest main commit only, so check that a commit has arm64
  first (`skopeo inspect --raw docker://registry.tilo.so/tilo/tilo-vm:main-<sha7>-ci`).
- **Your worktree on a builder of the device's architecture**
  (`--builder host` with `TILO_VM_CARGO`): any cargo drop-in that builds for
  aarch64 and leaves the binaries in the target dir `cargo metadata` names
  (set `CARGO_TARGET_DIR` to where the drop-in puts them). The layer's target
  and rustc are read from the binary it built, and the device refuses any
  other architecture. With a drop-in `$CARGO_AARCH64` that runs cargo on an
  aarch64 machine and brings the binaries back:

  ```sh
  CARGO_TARGET_DIR=$PWD/target/aarch64 TILO_VM_CARGO=$CARGO_AARCH64 \
    tilo-vm swap --slot device shell --builder host
  ```
- **Your worktree on the device** (`--builder vm`, the default when the
  device's image can build): the slot's `release` profile with the
  manifest's features and `dev-overrides`, so `control` and `shot` work. A
  board with little memory (the Orange Pi) builds only small components or
  `--profile dev` there; build the shell with the drop-in above, or take
  CI's. A laptop builds it on the device.

Each layer records its provenance (`authority = "ci"` for CI builds, `local`
for everything built from a worktree), which `slot show --json` and the
device's `/var/lib/tilo-vm/layers/<hash>/provenance.json` show.

## The device's image

```sh
tilo-vm vm status opi                    # tilo-vmd, protocol, architecture
tilo-vm vm ssh opi -- bootc status       # the image it runs and follows
tilo-vm vm upgrade opi                   # the newest build of the image it follows, then a reboot
tilo-vm vm upgrade opi --from sha:1a2b3c4   # that commit's build of this board's image
tilo-vm vm upgrade opi --image registry.tilo.so/tilo/tilo-vm:orangepi-3b-v2-1-ci
tilo-vm vm rollback-image opi
tilo-vm vm restart opi                   # reboot over SSH; wait for the new boot and tilo-vmd
```

A board's image is sealed, so an upgrade is a bootc switch and a reboot,
never a local build. `--from sha:<commit>` (and `ci:<tag>`) reads the board
the device's image was built for (`/usr/lib/tilo/hardware-profile.json`) and
takes that board's image of the build, `tilo-vm:<ref>-<sha7>-<board>` (a
commit that is not on main is found through its pull request's tags): the
generic `-ci` dev image has no board firmware or device tree and would not
boot it. CI builds a board image in every Build / OS dev run with an aarch64
lane; `skopeo list-tags docker://registry.tilo.so/tilo/tilo-vm` lists them.
Board dev images are public today; when the registry refuses the device's
own pull for want of a login (a private image), tilo-vm pulls the image here with this host's login
(`podman login registry.tilo.so`), sends only the layers the device lacks
and switches to that copy; the credentials never reach the device. A device
whose disk asks for a passphrase at boot waits for it at its own keyboard
after every reboot. tilo-vm cannot power a device on: `vm start` only waits
for SSH, `vm restart` reboots over SSH, and after `vm stop` someone has to
power it on. `vm create`, `vm reset`, `vm serial`, `vm shot` and
`vm control` need a hypervisor and say so. `vm rm opi` forgets the device
and its key; the device is untouched.

## When something is off

- `vm add` or `vm status` says the key was refused: run the install command
  it prints (once per device and key).
- `cannot run commands as root without a password`: the `--ssh` account has
  no passwordless sudo; rerun `vm add` with the same `--ssh` and `--key` and
  `--grant-sudo` (each `vm add` records what it is given).
- `no device slot`: the image runs no session of its own (a VM, or a board
  image without one); use your own slot.
- The swap says the session did not start: `tilo-vm logs device` names the
  cause; `tilo-vm rollback --slot device` brings the previous build back.
- A layer is refused with `target`: it was built for another architecture.
