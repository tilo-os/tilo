# Development devices

Every Tilo device, a laptop, a board or a VM, gets new code one way: it follows a
signed OS image and takes it through its own update path. Stable releases, main's
newest build and your branch differ only in which signed reference the device
follows. There is no second path: never copy binaries or images onto a device with
SSH, SCP or rsync.

## The update path

`bootc-fetch-apply-updates.timer` runs `/usr/libexec/tilo-verified-update`, as the
**Check for updates** button in Settings and `tilo system update` do:

1. `tilo-update-channel next` names the reference to follow: the channel's, or a
   build pinned in its place (below).
2. bootc pulls it by digest. The container policy checks its signature: the release
   key for releases, the development key (`keys/cosign-dev.pub`) for dev images.
3. bootc stages it, `tilo-extension live:system` applies what changed in `/usr`
   while the device runs, and the desktop restarts in place when you are away
   (`tilo-live-update-session`). A change to the kernel, firmware or initramfs
   waits for a restart.
4. The staged boot entry gets boot counters. A boot that does not reach a greeter
   that takes input (`tilo-boot-health`, required by `boot-complete.target`) is
   tried three times, then the boot loader returns to the previous image by itself.
   `/var` is the same in both, so your data stays.

## Channels

| Channel | Follows | Signed with | Published |
| --- | --- | --- | --- |
| `stable` | `registry.tilo.so/tilo/tilo:latest`; a board's `code.tilo.so/tilo/tilo:<board>-latest` | release key | manual releases (`docs/release-pipeline.md`) |
| `dev` | `registry.tilo.so/tilo/tilo-vm:<target>-ci` | development key, from t's trusted runs only | main's newest head, whenever t has room |

`<target>` is the board (`/usr/lib/tilo/hardware-profile.json`: `orangepi-3b-v2-1`,
`lenovo-83ed`) or else the architecture (`x86_64`, `aarch64`).

Choose the channel in **Settings → Updates → Update channel**, or as root with
`tilo-update-channel`. A dev image offers both channels; a release image follows
`stable` only, because dev images carry development-only code (an SSH server,
`dev-overrides` binaries) and a release image's policy refuses them. Choosing
`stable` on a dev device installs the release image, which then stays on `stable`.

```sh
tilo-update-channel current          # dev
tilo-update-channel ref              # registry.tilo.so/tilo/tilo-vm:orangepi-3b-v2-1-ci
```

### How main reaches the dev channel

`publish-os-dev-<arch>` and `publish-os-board-<board>` run on main's newest head
when t has room (and on `t dispatch ... --ref main`). An image is published once:

- it builds (`os-image-<arch>`, `os-dev-image-<arch>`, `os-board-<board>`; each
  layer cached by its inputs, so a change to an app or the shell rebuilds only the
  compile and image layers);
- a board image fits its size budget (`os-board-size-<board>`);
- `os-dev-gate-<arch>` took it as a device does: the dev disk published last booted
  under KVM, staged the image and applied it live, restarted into it to a greeter that
  takes input with its boot assessment passed, and staged and live-applied a newer
  image from there.

That is all the dev channel waits for. A device falls back by itself from an image
that does not boot, so a bad dev image costs a reboot, not data, as long as each
image boots and takes the update that fixes it: exactly what the gate proves. The
heavy acceptance suite (`os-acceptance-<arch>`: paired devices, the desktop's
journeys, the splash) gates stable releases and runs on main when t has room,
telling the commit that broke it; the dev disk `tilo-vm vm create` takes
(`publish-os-dev-disk-<arch>`) is published once it passed on that disk.

## Your own build on your device

Register the device once (`tilo-vm` is also `tilo device`):

```sh
tilo-vm vm add opi --ssh OWNER@ADDRESS --key /path/to/owner-key
tilo-vm vm use opi
```

Registration grants administrative access: use only a device you may administer.
Then:

```sh
tilo-vm vm upgrade opi --ref my-branch        # a pushed branch's head
tilo-vm vm upgrade opi --from sha:1a2b3c4     # main's build of a commit
tilo-vm vm upgrade opi                        # back to its channel's newest
```

`--ref` runs `t dispatch publish-os-ref-<target> --ref my-branch --publish`: t
builds the branch's head for the device's target, reusing every layer it has built
before, and publishes it signed in the branches' own repository as
`registry.tilo.so/tilo/tilo-vm-ref:ref-<branch>-<sha7>-<target>`; a branch can never
push to or sign for the repositories devices follow channels from. Then the
device takes it through the update path above: `tilo-update-channel pin` puts it in
the channel's place and `tilo-verified-update` stages it; tilo-vm reboots it into
the new image (`--no-reboot` leaves that to you). The device follows the pinned
build until its channel publishes a newer image, then returns to the channel on its
next update. `tilo-vm vm rollback-image` boots the previous image.

Until t gives runs of branches the identity tokens to sign and publish there, `--ref`
stops and says so. Until then the temporary path is `tilo-vm vm upgrade --from local` (or `tilo-vm image
build` and `vm upgrade --image <layout>`, `tilo device guide images`): it builds the
image locally and sends it unsigned over SSH, outside the update path above, and
goes away once branch publishing works.

## Test beside the owner's session

A private slot runs your component next to the device's normal session, without
an image:

```sh
tilo-vm slot create --headless --identity
tilo-vm swap shell
tilo-vm shot -o target/board.png
tilo-vm rollback
tilo-vm slot rm
```

`tilo device guide swapping` covers builders (a small board builds on a remote
builder) and `tilo device guide devices` the `device` slot, `seat claim` and
waking a device. A slot is a test session; what a device runs comes from its image.

## Boards and kernels

Use real hardware for drivers, firmware, audio, suspend and physical output; a VM
cannot prove that a board boots. [Hardware profiles](../../os/hardware/README.md)
define the supported boards, their firmware and the image they need: a generic
aarch64 image is no substitute. `t test os-board-<board>` builds a board image of
your tree without publishing it; `tilo device guide kernel` iterates on a kernel.
After a board or kernel change, check cold boot, the affected devices and the fall
back to the previous image on the hardware, and record the image digest, profile
and kernel with the result.

## When a new image does not boot

The device falls back on its own; find out why without rebuilding the whole image.
Boot the published image as a throwaway VM and follow its serial log:

```sh
tilo device vm create try1 --image /absolute/path/to/disk.raw
tilo device vm start try1
tilo device vm serial try1
```

Stop at the first stage that fails, and fix it in the layer that owns it:

| Last visible stage | Check | Owning layer |
| --- | --- | --- |
| Firmware, no serial output | `os/dev/verify-installed-development-disk.py --preflight-disk` on the disk | loader install |
| Stub, no kernel message | the UKI's sections and architecture | UKI |
| Kernel panic or missing driver | the serial log; the UKI's `.linux`, `.dtb`, `.initrd` | kernel, initramfs |
| Root discovery or composefs mismatch | `os/dev/composefs/guest-check.py`; the UKI `composefs=` value | initramfs; the root and UKI are sealed together |
| A unit fails, emergency mode | `systemctl --failed`, `journalctl -b -u UNIT` | the unit's layer |
| No greeter, or login falls back to it | `journalctl -b -u greetd`, the session journal | greeter or session layer |

On a disposable copy of the disk a helper under `/var/lib/tilo/debug/` with a
`/etc/systemd/system/<unit>.d/` override can prove a fix; the fix itself goes into
its source layer and reaches devices as the next image.
