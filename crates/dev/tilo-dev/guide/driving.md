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
users and agents depend on. Use input commands when the thing under test is
the pointer, hit-testing or painting.

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
