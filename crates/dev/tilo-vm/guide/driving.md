# Driving a slot

There is one driving story: `tilo-control`. `tilo-vm control` runs the VM's
copy against your slot's session; it never reimplements it.

```sh
tilo-vm control actions list                 # what this desktop can do
tilo-vm control actions invoke PROVIDER ID   # do it, no coordinates
tilo-vm control state-dump --pretty          # what it believes is on screen
tilo-vm control list-targets                 # named rects
tilo-vm control click NAME                   # also click-at, drag, key, type
tilo-vm shot -o target/slot.png              # the slot compositor's own frame
tilo-vm logs -n 200                          # the session's journal
```

Prefer the semantic half (`actions`, `state-dump`): it is the surface real
users and agents depend on. On the host, without a VM, the same story is
`tilo-control` and `shots` (AGENTS.md, "Driving a running desktop").

## Screenshots: `shot` and `vm shot`

- `tilo-vm shot` asks the slot's compositor to render its current scene into
  a PNG (`tilo-control snapshot`). Headless slot, headful slot on its vkms
  seat, or a slot holding seat0: the same call, the compositor's own frame.
  It never reads a display, so it cannot show what the kernel scanned out.
- `tilo-vm vm shot` is the hypervisor's screenshot of the VM's real display,
  seat0: greetd's greeter, or the slot that claimed it. Headless and vkms
  slots never appear there. It needs no SSH.

## Real input devices (headful slots)

A headful slot has its own seat, and `tilo-vm input` sends through a
keyboard and a pointer plugged into that seat (uinput devices with
`phys=seat-<slot>`), so libinput, key maps and hit-testing run for real:

```sh
tilo-vm input key ctrl+alt+t        # combos, pressed in order
tilo-vm input type 'hello world'    # US layout
tilo-vm input move 400 300          # desktop pixels (see below)
tilo-vm input click left --at 400 300
```

Headless slots have no seat and refuse `input`; use `tilo-vm control
key|type|click` there. While a slot holds seat0, its input goes to seat0.

## Several monitors, mixed scale

```sh
tilo-vm slot create dual --headful --outputs 2560x1600@2,1920x1080@1
tilo-vm shot -o target/dual.png --slot dual
tilo-vm input --slot dual click left --at 1500 400   # on the second monitor
```

Each `--outputs` entry is one vkms connector (`Virtual-1`, `Virtual-2`, ...)
with its own CRTC, laid out left to right, top-aligned, the first one
primary. vkms offers the standard VESA (DMT) 60 Hz modes, like a monitor
without an EDID. The mode and scale are not injected: tilo-vmd stages them
as a display change in the slot user's settings, the compositor applies it,
and tilo-vmd confirms it, exactly as the Settings app does, so the
compositor remembers the layout for that set of monitors. Outputs are fixed
when the slot is created; running the same `slot create` again re-applies
the layout, and a different list needs `tilo-vm slot rm` first.

With `--outputs`, `input move` and `click --at` take logical desktop
pixels: `2560x1600@2,1920x1080` is 1280 + 1920 wide, and x 1280 is the left
edge of the second monitor.

## seat0

```sh
tilo-vm seat claim                   # your session moves onto the real display (VT 1)
tilo-vm vm shot -o target/seat0.png  # what the hypervisor window shows now
tilo-vm seat release                 # back to your own seat; greetd returns
```

Only one slot holds seat0. A claim on a held seat fails and names the holder;
`--steal` takes it anyway and moves the holder back to its own seat.

## When SSH is down

The hypervisor can still show you the VM: `tilo-vm vm shot`,
`tilo-vm vm serial`, `tilo-vm vm reset`, and `tilo-vm vm control ...`
(virsh, the QEMU monitor, utmctl or PowerShell). `tilo-vm doctor` lists what
your adapter supports.
