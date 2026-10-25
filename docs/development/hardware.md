# Work on a board or kernel

Use real hardware to establish driver, firmware, audio, suspend and physical
output behaviour. A VM can test installation or generic kernel paths, but cannot
prove that a board boots or its peripherals work.

## Choose the right surface

| Change                                        | Smallest useful route                        |
| --------------------------------------------- | -------------------------------------------- |
| Shell or service on an installed dev device   | A private slot, below                        |
| Owner's actual display/session                | The `device` slot, with the owner's consent  |
| Generic kernel iteration                      | `tilo device guide kernel`                   |
| Initramfs, boot units or cold-boot behaviour  | `tilo device guide scenarios`                |
| Board firmware, device tree or image packages | Board image workflow and physical acceptance |

[Hardware profiles](../../os/hardware/README.md) define the supported boards,
firmware inputs and signed-image requirements. In particular, Lenovo Yoga Slim
7x (`lenovo-83ed`) and Orange Pi 3B v2.1 (`orangepi-3b-v2-1`) need their own
profiles; a generic aarch64 image is not a substitute.

## Connect to an existing development device

The machine must already run a dev image with SSH and `tilo-vmd`. Registration
grants administrative access: remote operations run as root, directly or through
the owner's passwordless sudo. Use only a device you are authorized to
administer.

```sh
tilo-vm vm add board --ssh OWNER@ADDRESS --key /path/to/owner-key
tilo-vm vm use board
tilo-vm vm status board
```

Substitute the account, address and key. The command checks SSH, root access,
the daemon, toolchain and bootc, and reports missing prerequisites. Do not add
`--grant-sudo` casually: it writes an unrestricted passwordless sudo grant for
that account. The canonical
[`tilo device guide devices`](../../crates/dev/tilo-vm/guide/devices.md)
explains approval, key setup and revocation.

## Test without replacing the owner's desktop

A private slot lets you inspect your build beside the device's normal session:

```sh
tilo-vm slot create --headless --identity
tilo-vm swap shell
tilo-vm shot -o target/board.png
tilo-vm rollback
tilo-vm slot rm
```

For a small board, use the remote-builder setup in `tilo-vm guide swapping` or
an available CI component layer rather than compiling a release shell on the
board.

A snapshot renders the slot's scene; it does not photograph the panel.
`seat claim` takes the physical display and ends the owner's login; the claim
survives reboot until `seat release`. Ask before using it. The reserved `device`
slot updates the machine's own session instead of a private test session. Its
swaps/rollbacks can restart the owner's desktop and close apps; VM-wide
components affect other sessions too.

## Change the board image

Use the board-specific procedures in
[Hardware profiles](../../os/hardware/README.md) and the current inputs of
[t's board-image checks](../../t.toml). Keep development images distinct from
signed release images. Never flash a generic VM disk onto a board merely because
its architecture matches.

`os-board-<board>` builds a board image from your working tree without
publishing it. `publish-os-board-<board>` publishes main's newest head when t
has room, after the board-image, size and dev-image gate (`os-dev-gate-aarch64`)
checks pass. Use the matching board image, not a generic
VM disk. These development images are separate from signed releases.

On an existing dev target, `tilo-vm guide images` and `guide devices` describe
image upgrade and rollback. An upgrade/restart reboots the whole machine;
arrange local recovery access and any boot passphrase entry first. Use an
explicit published board dev-image reference with `vm upgrade --image` when a
download-only operation is required. Do not copy files into `/usr` or use a
writable overlay to bypass generation tracking.

After a board or kernel change, physically check cold boot, affected devices and
image rollback. Preserve identity and data, and record the image digest,
hardware profile and kernel with the result. A successful image build or
disk-install check is not that evidence.
