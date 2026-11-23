# Devices: boards and other real machines

A **device** is a real machine that already runs a Tilo dev image, such as an
Orange Pi installed from its board dev disk or a Lenovo Yoga Slim 7x installed
from its board dev image. tilo-vm reaches it through its device daemon
instead of a hypervisor. Everything else is the VM loop: the same accounts,
layers, generations, `swap`, `rollback`, `shot`, `control` and provenance
checks. Never hot-fix a device by hand (`git archive | ssh`,
`bootc usr-overlay`, `install` into `/usr/bin`): nothing can roll that back,
and the next boot loses it.

## How tilo-vm reaches a device

Every machine and VM tilo-vm drives is one of your devices. tilo-vm sends its
steps (`status`, `swap`, `build`, `exec`, `upgrade`, `reboot`, ...) as action
frames to the device daemon's endpoint (`tilo-actions/1`, UDP port 7703 on a
dev image). Each request is signed with this computer's device key from
`tilo login`, and the device runs it only when your workspace grants this
computer `devices/<device key>/vm`. Bytes travel as content the device does
not have yet: a layer is one blob, a worktree a manifest plus the files that
changed. Nothing reaches a device over SSH; `vm ssh` is the one interactive
shell, for people.

**This computer needs a Tilo login.** On a device of yours run
`tilo device add`, then here `tilo login <the ticket it prints>`. On a
computer with no person of its own (CI, a fresh builder) tilo-vm makes a
throwaway one for its VMs under its state directory the first time it starts
one (`tilo login --new-identity`); `tilo-vm doctor` says which it uses.

**A VM tilo-vm starts joins by itself.** Before the VM's first boot tilo-vm
runs `tilo device add`, which prints a one-use ticket, and hands the ticket to
the VM as the systemd credential `tilo.ticket` (an SMBIOS OEM string, or the
kernel command line for a direct kernel boot), next to `tilo.port=7703`, the
port its daemon binds. The VM spends the ticket as it boots and joins as one of
your devices; tilo-vm records its device key in `vms.toml` and drops the
ticket. libvirt, QEMU and WHPX have that channel; a UTM or Hyper-V VM is
registered like a real machine (below).

## Register a real machine

```sh
tilo device list                                   # on any device of yours: the machine's device key
tilo-vm vm add opi --device 3f2a...e1 --address 10.0.0.108
tilo-vm vm add yoga --device 9c41...07 --ssh me@10.0.0.223 --key ~/.ssh/yoga_ed25519   # vm ssh opens a shell there
tilo-vm vm status opi                              # its daemon's version and architecture
tilo-vm vm use opi                                 # this worktree (writes .tilo-vm); --global for every worktree
```

The machine must already run a board dev image and be one of your devices
(`tilo device add` on it, or `tilo login` there). `vm add` asks its daemon
as this computer and prints each check with its fix:

| Check | Needed for | Fix when it fails |
|---|---|---|
| `device` | everything | the device key, the address (`--address <ip>[:<port>]`), the grant on `devices/<key>/vm` |
| `toolchain` | `swap` from your worktree on the device (cargo, python3) | a current board dev image, or CI builds (`--from sha:`) |
| `bootc` | `vm upgrade`, `vm rollback-image` | a bootc dev image |

It registers the device only once `device` passes; rerun it after a fix (it
is idempotent). `--ssh` and `--key` only set where `vm ssh` opens its shell.

## A laptop: the owner's own desktop is the `device` account

A laptop's image logs its owner in through greetd. While the owner is logged
in to Tilo on the built-in display, that login is the **`device` account**, and
a swap updates the desktop the person at the machine is using, without
logging them out:

```sh
tilo-vm status --account device                          # whose login it is, generation, components
tilo-vm swap --account device tilo-session --from sha:1a2b3c4   # CI's build of a main commit (or build it, below)
tilo-vm account exec --account device -- sha256sum /usr/bin/tilo-session   # the binary the desktop runs now
tilo-vm account show device                              # every file and its sha256 and layer
tilo-vm logs device
tilo-vm control --account device state-dump --pretty     # dev images: the owner's compositor
tilo-vm rollback --account device                        # the generation before
tilo-vm rollback --account device --to 0                 # the image's own session again
```

What the person at the machine sees on a swap or a rollback: the screen goes
dark for a moment and the desktop comes back, still logged in. The
compositor restarts in place (`tilo-session` restarts it on SIGUSR1); the
login, its user manager and its services keep running. The machine's own apps
(a `.desktop` entry) run behind app keepers and stay open with their windows;
Tilo's apps close and reopen through continuity. A component
with user services (`ush`, the model services) restarts those services too.
Unsaved work in an app that does not keep its state is lost, so on a machine
someone is using, say before you swap.

greetd runs the login in the machine's own mount namespace, so the account's
layers merge into the machine's `/usr` with `systemd-sysext`, together with
the `vm` account's: every process that starts afterwards sees them, the greeter
and other accounts included, and they stay merged across reboots until a
rollback removes them. `vm ssh -- sha256sum /usr/bin/tilo-session` shows the same
file as `account exec`.

If the new desktop does not come back, the swap fails with the journal and
the owner is back at the login screen; the account outlives the login, so
`tilo-vm rollback --account device` works from there and the next login runs
the previous build. While nobody is logged in, swaps and rollbacks still
apply and reach the next login; `status` says `logged out`, and `account exec`,
`control` and `shot` wait for a login. With no Tilo login on the display
(and nothing swapped into the account before), there is no `device` account.

Never update the owner's desktop any other way (`bootc usr-overlay`, copying
into `/usr/bin`): nothing rolls that back. Your own account still works beside
the owner's session, for a throwaway desktop the owner never sees:

```sh
tilo-vm account create --headless --identity   # your session beside the owner's, with a throwaway identity
tilo-vm swap tilo-session                          # your layers reach only your account
tilo-vm shot -o target/yoga.png             # your compositor's own frame
tilo-vm seat claim                          # your session takes the built-in display (ends the owner's login)
tilo-vm seat release                        # greetd's login screen comes back
tilo-vm account rm                             # done: your account's session and user go
```

What reaches the person at the machine, so ask before each on a machine
someone is using:

- A `device` account swap or rollback restarts their desktop (above).
- `seat claim` stops greetd, which ends whoever is logged in on the display.
  The claim is kept across reboots (`tilo-vm-restore.service` starts your
  session again, still on seat0), so `seat release` when you are done.
- `vm restart`, `vm upgrade` and `vm rollback-image` reboot the machine
  (an upgrade reboots by itself; no `vm restart` after it), which ends every
  session on it. A disk that asks for a passphrase at boot waits at the
  machine's keyboard each time.
- VM-wide swaps (`--account vm`, `tilo-vm guide swapping`) change `/usr` for
  everyone and restart the owner's desktop too; the greeter's restarts
  greetd, which ends the owner's login.

When `vms.toml` names a remote builder for the laptop's architecture, swaps
build there by default (Builds, below). Building on the device
(`--builder vm`) works too: release layers link without LTO
(`tilo-vm guide swapping`), and a laptop has the memory. Accounts live in `/var`
and survive image upgrades, layers included (`tilo-vm guide images`).

## A board: the device's own session

A board image runs its account's session without a greeter
(`tilo-board-headless-session@<account>`): headless, or on the display
once one is connected. That session is the **`device` account**:

```sh
tilo-vm swap --account device tilo-session --from sha:1a2b3c4   # CI's aarch64 build of a main commit; only the layer travels
tilo-vm swap --account device tilo-session                      # your worktree (see Builds below)
tilo-vm status --account device                          # generation, session, components
tilo-vm account show device                              # every file and its sha256 and layer
tilo-vm account exec --account device -- sha256sum /usr/bin/tilo-session
tilo-vm logs device
tilo-vm control --account device state-dump --pretty
tilo-vm rollback --account device                        # the generation before
tilo-vm rollback --account device --to 0                 # the image's own session again
```

A swap merges the layers into the machine's `/usr` with `systemd-sysext`,
together with the `vm` account's, and restarts the session the normal way: stop
it, restart the account's user manager, start it. The merge is system-wide
because the seat's supervisor starts the compositor as a system transient
unit in the host's own mount namespace; that unit takes no drop-in and no
view reaches it, so a per-session view could never start the new session.
The `systemd-sysext` links stay in `/var/lib/extensions`, so a fresh boot
merges them again, and `tilo-vm-restore.service` restarts the session if it
started first; an image without the device steps boots its own session. Its account
and identity stay as they are; the device account never creates or removes a
user. It cannot be created, removed or moved to another seat: it is the
machine's session.

Set `account = "device"` in the worktree's `.tilo-vm` (or `TILO_VM_ACCOUNT=device`)
to make it the default. Your own accounts (`tilo-vm account create --headless`)
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
  job builds the newest main commit only, so check that a commit has arm64
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
  aarch64 = ["my-arm-cargo"]   # any cargo drop-in that runs on an aarch64 machine
  ```

  ```sh
  tilo-vm swap --account device tilo-session                    # builds on the aarch64 builder, streams to the device
  tilo-vm swap --account device apps+runtime             # the whole app collection and runtime, the same way
  tilo-vm swap --account device tilo-session --builder remote   # the same, said explicitly
  ```

  Do not build for a device with `--builder host` and a `TILO_VM_CARGO`
  drop-in that brings the binaries back: every swap copies them (a release
  session binary is large) into `target/`, and that has filled this host's disk
  before. The layer's target and rustc are read from the binary on the
  builder, and the device refuses any other architecture.
- **Your worktree on the device** (`--builder vm`, the default when the
  device's image can build): the account's `release` profile with the
  manifest's features, so the build is the real one. `control` and `shot`
  drive the control socket the deleted shell bound, so a session swapped onto
  the device cannot be driven that way yet. A board with little memory (the
  Orange Pi) builds only small components or `--profile dev` there: build the
  session on a remote builder (above), or take CI's.

Each layer records its provenance (`authority = "ci"` for CI builds, `local`
for everything built from a worktree), which `account show --json` and the
device's `/var/lib/tilo-vm/layers/<hash>/provenance.json` show.

## The device's image

```sh
tilo-vm vm status opi                    # its daemon and architecture
tilo-vm vm ssh opi -- bootc status       # the image it runs and follows
tilo-vm vm upgrade opi                   # back to its channel's newest image, then a reboot
tilo-vm vm upgrade opi --from sha:1a2b3c4   # that commit's build of this board's image
tilo-vm vm rollback-image opi
tilo-vm vm restart opi                   # reboot through its daemon; wait for the new boot
```

A board's image is sealed, so an upgrade takes a published image through the
device's own update path and reboots, never a
local build. `--from sha:<commit>` reads the board the device's
image was built for (`/usr/lib/tilo/hardware-profile.json`) and take that
board's image of the build, `tilo-vm:main-<sha7>-<board>`: the generic dev image has no board
firmware or device tree and would not boot it. The board follows
`tilo-vm:<board>-ci`, its dev channel, otherwise.
Board dev images are public today; when the registry refuses the device's
own pull for want of a login (a private image), tilo-vm pulls the image here with this host's login
(`podman login registry.tilo.so`), sends only the layers the device lacks
and switches to that copy; the credentials never reach the device. A device
whose disk asks for a passphrase at boot waits for it at its own keyboard
after every reboot. tilo-vm cannot power a device on: `vm start` only waits
for its daemon, `vm restart` reboots it, and after `vm stop` someone has to
power it on (`vm wake` wakes one that sleeps, below). `vm create`, `vm reset`, `vm serial`, `vm shot` and
`vm control` need a hypervisor and say so. `vm rm opi` forgets the device;
the device is untouched.

## Asleep, awake, and on another address

**It stays awake while you work on it.** Every tilo-vm command that asks its
daemon renews a lease: a logind `sleep` lock (`tilo-vm-awake.service`,
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
it again while it waits, up to 150 seconds, for its daemon, whose first
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
another office, is reached the same way: the daemon's endpoint is dialled by
its device key, directly when an address answers and through a relay
otherwise, and the connection follows a roam or a lost link by itself.
Everything works over it, builds and `vm upgrade` included; only `vm ssh`
needs an address SSH can reach.

## When something is off

- `this computer has no Tilo login`: on a device of yours run
  `tilo device add`, then here `tilo login <ticket>`.
- `has not answered as a Tilo device yet`: a VM joins at its next
  `tilo-vm vm start`; a real machine is registered with `vm add --device`.
- `does not let this machine act on devices/<key>/vm`: grant it from a device
  that may: `tilo grant devices/<key>/vm <this computer's device key>`
  (`tilo-vm doctor` names the key).
- `runs an image that predates the <step> step`: `tilo-vm vm upgrade <name>`.
- `asleep or gone`: it answers neither at its address nor by name for a
  minute. `tilo-vm vm wake <name>`, or wake it at the machine; then
  `tilo-vm vm awake <name> --for 2h` keeps it from sleeping again.
- `moved from <old> to <new>`: it was found by name on another address and
  that is recorded now; nothing to do.
- `no device account`: the image runs no session of its own and nobody is
  logged in to Tilo on its display (a laptop at its login screen, a VM); the
  owner logs in at the machine, or use your own account.
- `nobody is logged in`: the laptop's `device` account waits for its owner to
  log in for `account exec`, `control` and `shot`; swaps and rollbacks work.
- The swap says the desktop did not come back: the owner is at the login
  screen; `tilo-vm rollback --account device` brings the previous build back.
- The swap says the session did not start: `tilo-vm logs device` names the
  cause; `tilo-vm rollback --account device` brings the previous build back.
- A layer is refused with `target`: it was built for another architecture.

## The machine's GPU (`--gl`)

`vm create <name> --gl` renders the VM's desktop on the machine's GPU through virgl.
NVIDIA's EGL stack initialises no context virglrenderer can render on
(`EGL_NOT_INITIALIZED`), and libvirt otherwise picks the node the firmware
initialised, which on a laptop with a discrete NVIDIA card is that card. So
tilo-vm picks the first render node whose driver is not NVIDIA, keeps it on
the VM and says so when it creates it:

```
GL renders on /dev/dri/renderD128 (the first render node whose driver is not NVIDIA; amdgpu serves virglrenderer where NVIDIA's EGL stack does not)
```

Name another with `--render-node /dev/dri/renderD128` when more than one is
usable. A `--gl` VM's scanout lives in a GL context outside the guest, so `tilo-vm vm shot`
cannot capture it (`no surface`); use `tilo-vm shot` for the compositor's own
frame, or a VM without `--gl` when a frame from outside the guest is what a test needs
(`tilo-vm guide driving`).

Enable a supported active connection's magic-packet wake without reconnecting:

```sh
tilo-vm vm network-wake yoga --profile <NetworkManager-UUID> --enabled true
tilo-vm vm network-wake yoga --profile <NetworkManager-UUID>
```

This is a step like any other, asked under the same grant on `devices/<key>/vm`.
It persists the connection property and asks NetworkManager to reapply it;
readback distinguishes saved configuration, driver support and current driver
state. A saved setting with a reapply error needs attention and reports failure;
it does not disconnect the connection. Settings exposes **Wake over network**
for saved Wi-Fi profiles. Neither setting nor driver support verifies physical
wake, and a reachable awake device is required to configure it.
