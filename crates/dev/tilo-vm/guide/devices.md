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

## A laptop: the owner's own desktop is the `device` slot

A laptop's image logs its owner in through greetd. While the owner is logged
in to Tilo on the built-in display, that login is the **`device` slot**, and
a swap updates the desktop the person at the machine is using, without
logging them out:

```sh
tilo-vm status --slot device                          # whose login it is, generation, components
tilo-vm swap --slot device shell --from sha:1a2b3c4   # CI's build of a main commit (or build it, below)
tilo-vm slot exec --slot device -- sha256sum /usr/bin/shell   # the binary the desktop runs now
tilo-vm slot show device                              # every file and its sha256 and layer
tilo-vm logs device
tilo-vm control --slot device state-dump --pretty     # dev images: the owner's compositor
tilo-vm rollback --slot device                        # the generation before
tilo-vm rollback --slot device --to 0                 # the image's own shell again
```

What the person at the machine sees on a swap or a rollback: the screen goes
dark for a moment and the desktop comes back, still logged in. The
compositor restarts in place (`tilo-session` restarts it on SIGUSR1); the
login, its user manager and its services keep running, and open apps close
and reopen through continuity, as after any compositor restart. A component
with user services (`ush`, the model services) restarts those services too.
Unsaved work in an app that does not keep its state is lost, so on a machine
someone is using, say before you swap.

greetd runs the login in the machine's own mount namespace, so the slot's
layers merge into the machine's `/usr` as signed layers, together with the
`vm` slot's, through the system extension activation that also merges a
production device's extensions: every process that starts afterwards sees
them, the greeter and other accounts included, and they stay merged across
reboots until a rollback removes them. The machine merges only layers signed
by a key its owner enrolled: once per machine, `tilo-vm --target yoga trust
enroll` (`tilo-vm guide trust`). `vm ssh -- sha256sum /usr/bin/shell` shows the same
file as `slot exec`.

If the new desktop does not come back, the swap fails with the journal and
the owner is back at the login screen; the slot outlives the login, so
`tilo-vm rollback --slot device` works from there and the next login runs
the previous build. While nobody is logged in, swaps and rollbacks still
apply and reach the next login; `status` says `logged out`, and `slot exec`,
`control` and `shot` wait for a login. With no Tilo login on the display
(and nothing swapped into the slot before), there is no `device` slot.

Never update the owner's desktop any other way (`bootc usr-overlay`, copying
into `/usr/bin`): nothing rolls that back. Your own slot still works beside
the owner's session, for a throwaway desktop the owner never sees:

```sh
tilo-vm slot create --headless --identity   # your session beside the owner's, with a throwaway identity
tilo-vm swap shell                          # your layers reach only your slot
tilo-vm shot -o target/yoga.png             # your compositor's own frame
tilo-vm seat claim                          # your session takes the built-in display (ends the owner's login)
tilo-vm seat release                        # greetd's login screen comes back
tilo-vm slot rm                             # done: your slot's session and user go
```

What reaches the person at the machine, so ask before each on a machine
someone is using:

- A `device` slot swap or rollback restarts their desktop (above).
- `seat claim` stops greetd, which ends whoever is logged in on the display.
  The claim is kept across reboots (`tilo-vm-restore.service` starts your
  session again, still on seat0), so `seat release` when you are done.
- `vm restart`, `vm upgrade` and `vm rollback-image` reboot the machine
  (an upgrade reboots by itself; no `vm restart` after it), which ends every
  session on it. A disk that asks for a passphrase at boot waits at the
  machine's keyboard each time.
- VM-wide swaps (`--slot vm`, `tilo-vm guide swapping`) change `/usr` for
  everyone and restart the owner's desktop too; the greeter's restarts
  greetd, which ends the owner's login.

When `vms.toml` names a remote builder for the laptop's architecture, swaps
build there by default (Builds, below). Building on the device
(`--builder vm`) works too: release layers link without LTO
(`tilo-vm guide swapping`), and a laptop has the memory. Slots live in `/var`
and survive image upgrades, layers included (`tilo-vm guide images`).

## A board: the device's own session

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
- **Your worktree on a remote builder of the device's architecture**
  (`--builder remote`, the default for a device whose architecture has a
  `[remote_builders]` entry in `vms.toml`): the builder builds and lays out
  the layer, and it streams through this host into the device's upload.
  Neither the binaries nor the layer are written to this host's disk. Set it
  up once with any cargo drop-in whose `run` runs the program on an aarch64
  machine in this worktree (`tilo-vm guide swapping`):

  ```toml
  [remote_builders]
  aarch64 = ["dist/dev/t-builder.sh", "aarch64"]   # t's aarch64 nodes build it
  ```

  ```sh
  tilo-vm swap --slot device shell                    # builds on the aarch64 builder, streams to the device
  tilo-vm swap --slot device apps+runtime             # the whole app collection and runtime, the same way
  tilo-vm swap --slot device shell --builder remote   # the same, said explicitly
  ```

  Do not build for a device with `--builder host` and a `TILO_VM_CARGO`
  drop-in that brings the binaries back: every swap copies them (a release
  shell is about 165 MB) into `target/`, and that has filled this host's disk
  before. The layer's target and rustc are read from the binary on the
  builder, and the device refuses any other architecture.
- **Your worktree on the device** (`--builder vm`, the default when the
  device's image can build): the slot's `release` profile with the
  manifest's features and `dev-overrides`, so `control` and `shot` work. A
  board with little memory (the Orange Pi) builds only small components or
  `--profile dev` there: build the shell on a remote builder (above), or take
  CI's.

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
power it on (`vm wake` wakes one that sleeps, below). `vm create`, `vm reset`, `vm serial`, `vm shot` and
`vm control` need a hypervisor and say so. `vm rm opi` forgets the device
and its key; the device is untouched.

## Asleep, awake, and on another address

**It stays awake while you work on it.** Every tilo-vm command that talks to
tilo-vmd renews a lease: a logind `sleep` lock (`tilo-vm-awake.service`,
listed by `systemd-inhibit --list` on the device) until 30 minutes after the
last request. A lost connection does not end it, and it ends by itself when
you stop. The lock blocks suspend only: the screen still blanks and locks,
and closing the lid or a critical battery still suspends. `vm status` shows
until when it holds.

```sh
tilo-vm vm awake yoga --for 2h   # a long build or test with no tilo-vm requests in between
tilo-vm vm awake yoga            # until when it is held
tilo-vm vm awake yoga --off      # done: it may suspend once idle (the next request holds it again)
```

Hold it with `--for` before anything that runs long without tilo-vm
requests: waking a device is not reliable (below), keeping it awake is.

**Its address is only a hint.** `vm add` and `vm status` record the device's
hostname, its MAC on the network tilo-vm reached it on, and what can wake it.
When the recorded address gives no route or no answer, every command looks
the device up as `<hostname>.local` (mDNS, through `getent hosts` or
`avahi-resolve`), records the new address in `vms.toml` and says so. It
keeps trying the address and the name for a minute, so a Wi-Fi roam does
not fail a command. After that it reports `asleep or gone` and offers
`vm wake`.

**Waking it.** `tilo-vm vm wake yoga` sends the device's Wake-on-LAN magic
packet from this machine (UDP port 9, to 255.255.255.255 and the device's
subnet broadcast), so this machine has to be on the device's LAN. It sends
it again while it waits, up to 150 seconds, for tilo-vmd, whose first
request holds the device awake again. Whether the packet wakes it depends on
the hardware, and `vm status` shows what the device reported:

| `wake:` says | Means |
|---|---|
| Wake-on-LAN magic packet on `<if>` (enabled) | Ethernet WoL: `vm wake` wakes it |
| Wake-on-LAN ... supported but disabled | the image did not enable it; the printed `ethtool -s <if> wol g` does, until the next boot |
| Wi-Fi WoWLAN ... not known to wake this machine | the Wi-Fi chip lists magic-packet wake, but the machine around it may not wake on it (support and resume reliability must be checked on the booted kernel): count on someone at the machine |
| cannot wake on a magic packet | someone has to wake it at the machine |
| unknown | the image has no `ethtool` or `iw` to ask |

When it cannot be counted on, `vm wake` says so before it sends the packet;
keep such a device awake with `vm awake --for` instead. A device with no MAC
recorded (registered before tilo-vm recorded one) is woken by hand once,
then `vm status` records it. A libvirt VM is woken through the hypervisor
(`virsh dompmwakeup`); `vm start` does the same for one that is suspended.

## Beyond the LAN

A device that is not on this network, a laptop at home or a board in
another office, is reached over the peer network: hole-punched QUIC between
the two machines, through a relay when no direct path exists, which follows
a roam or a lost link by itself. Its dev image serves tilo-vmd's request
protocol there as `tilo-device-admin/1` (`tilo-device-admin.service`, also
while it sits at its login screen), never a shell.

```sh
tilo-vm vm enrol-admin --via yoga   # once: yoga holds your identity and certifies this machine
tilo-vm vm status board             # on board's LAN once: records its route over the peer network
tilo-vm vm status board             # anywhere: "reached over the peer network: relay https://..."
```

**This machine enrols as one of the owner's devices.** The device admits
only a device of its owner's identity whose root-signed certificate grants
administration, and that the owner has not removed. `vm enrol-admin` makes
this machine's device key (beside `vms.toml`, in `admin-identity/`) and has
the owner's root certify it with `tilo-enroll certify --capability admin`.
With `--via <device>` tilo-vm runs that over SSH on the device where the
owner approves new devices, as the account it logs in with, which must own
the identity (register it as the owner: `vm add --ssh <owner>@<address>`).
Without `--via` it prints the key and the command for the owner to run, and
`--cert <file>` installs what they hand back. The certificate lists this
machine among the owner's devices, so Settings shows it and removing it
there ends its access. tilo-vm needs `tilo-device-admin` beside it (or in
`TILO_DEVICE_ADMIN`): `cargo build --release -p tilo-device-admin` and copy
it next to `tilo-vm`.

**The route is recorded like the address.** `vm add` and `vm status` record
the device id and route the device reports (`tilo-device-admin id`) in
`vms.toml`; `vm status` says `beyond the LAN:` what it has. When the device
answers neither at its address nor by name, a command looks once (no minute
of retries: the peer network rides out roams itself), then dials the
recorded route and says which one it took, relay or direct. The device logs
each session and each request (`journalctl -u tilo-device-admin` on it).

**What works over it.** Everything that is a tilo-vmd request: `status`,
`vm status`, slots, swaps of CI layers (tilo-vm uploads them), rollbacks,
`logs`, `shot`, `control`, `input` and `vm awake`. What needs SSH (`slot
exec`, builds on the device, `vm ssh`, `vm upgrade`) waits until it is on
this network again.

**A relay of your own.** A device and this machine use the public relays
unless `TILO_DEVICE_ADMIN_RELAY=<url>` names one of yours: on the device in
`/etc/tilo/device-admin.env` (then `systemctl restart tilo-device-admin`),
where it is recorded with the route, so tilo-vm uses it too.

## When something is off

- `vm add` or `vm status` says the key was refused: run the install command
  it prints (once per device and key).
- `asleep or gone`: it answers neither at its address nor by name for a
  minute. `tilo-vm vm wake <name>`, or wake it at the machine; then
  `tilo-vm vm awake <name> --for 2h` keeps it from sleeping again.
- `moved from <old> to <new>`: it was found by name on another address and
  that is recorded now; nothing to do.
- `not on this network, and it did not answer over the peer network`: it is
  asleep, offline or cannot reach a relay; wake it at the machine, then
  `tilo-vm vm awake <name> --for 2h`.
- `it refused this machine`: the device said why (another identity, no
  administration grant, removed from the owner's devices);
  `tilo-vm vm enrol-admin` fixes each.
- `this machine is not enrolled`: `tilo-vm vm enrol-admin --via <device>`.
- `another device answered for it`: the recorded route is stale; `tilo-vm vm
  status <name>` on its LAN records it again.
- `cannot run commands as root without a password`: the `--ssh` account has
  no passwordless sudo; rerun `vm add` with the same `--ssh` and `--key` and
  `--grant-sudo` (each `vm add` records what it is given).
- `no device slot`: the image runs no session of its own and nobody is
  logged in to Tilo on its display (a laptop at its login screen, a VM); the
  owner logs in at the machine, or use your own slot.
- `nobody is logged in`: the laptop's `device` slot waits for its owner to
  log in for `slot exec`, `control` and `shot`; swaps and rollbacks work.
- The swap says the desktop did not come back: the owner is at the login
  screen; `tilo-vm rollback --slot device` brings the previous build back.
- The swap says the session did not start: `tilo-vm logs device` names the
  cause; `tilo-vm rollback --slot device` brings the previous build back.
- A layer is refused with `target`: it was built for another architecture.

Enable a supported active connection's magic-packet wake without reconnecting:

```sh
tilo-vm vm network-wake yoga --profile <NetworkManager-UUID> --enabled true
tilo-vm vm network-wake yoga --profile <NetworkManager-UUID>
```

This uses the same authenticated device-admin route as other device requests.
It persists the connection property and asks NetworkManager to reapply it;
readback distinguishes saved configuration, driver support and current driver
state. A saved setting with a reapply error needs attention and reports failure;
it does not disconnect the connection. Settings exposes **Wake over network**
for saved Wi-Fi profiles. Neither setting nor driver support verifies physical
wake, and a reachable awake device is required to configure it.
