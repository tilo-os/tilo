# Driving a slot

There is one driving story: `tilo-control`. `tilo-dev control` runs the VM's
copy against your slot's session; it never reimplements it.

```sh
tilo-dev control actions list                 # what this desktop can do
tilo-dev control actions invoke PROVIDER ID   # do it, no coordinates
tilo-dev control state-dump --pretty          # what it believes is on screen
tilo-dev control list-targets                 # named rects
tilo-dev control click NAME                   # also click-at, drag, key, type
tilo-dev shot -o target/slot.png              # PNG of the live scene
tilo-dev logs -n 200                          # the session's journal
```

Prefer the semantic half (`actions`, `state-dump`): it is the surface real
users and agents depend on.

## Real input devices (headful slots)

A headful slot has its own seat, and `tilo-dev input` sends through a
keyboard and a pointer plugged into that seat (uinput devices with
`phys=seat-<slot>`), so libinput, key maps and hit-testing run for real:

```sh
tilo-dev input key ctrl+alt+t        # combos, pressed in order
tilo-dev input type 'hello world'    # US layout
tilo-dev input move 400 300          # output pixels
tilo-dev input click left --at 400 300
```

Headless slots have no seat and refuse `input`; use `tilo-dev control
key|type|click` there. While a slot holds seat0, its input goes to seat0.

## seat0

```sh
tilo-dev seat claim      # your session moves onto the real display (VT 1)
tilo-dev seat release    # back to your own seat; greetd returns
```

Only one slot holds seat0. A claim on a held seat fails and names the holder;
`--steal` takes it anyway and moves the holder back to its own seat.

## When SSH is down

The hypervisor can still show you the VM: `tilo-dev vm shot`,
`tilo-dev vm serial`, `tilo-dev vm reset`, and `tilo-dev vm control ...`
(virsh, the QEMU monitor, utmctl or PowerShell). `tilo-dev doctor` lists what
your adapter supports.
