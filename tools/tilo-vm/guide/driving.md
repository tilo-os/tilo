# Driving an account

There is one driving story: `tilo-control`. `tilo-vm control` runs the VM's
copy against your account's session; it never reimplements it.

```sh
tilo-vm control actions list                 # what this desktop can do (tilo action list)
tilo-vm account exec -- tilo do NAME         # do it, no coordinates
tilo-vm control state-dump --pretty          # what it believes is on screen
tilo-vm control list-targets                 # named rects
tilo-vm control click NAME                   # also click-at, drag, key, type
tilo-vm shot -o target/account.png              # the account compositor's own frame
tilo-vm logs -n 200                          # the session's journal
```

Prefer the semantic half (action frames, `state-dump`): it is the surface real
users and agents depend on. On the host, without a VM, the same story is
`tilo action list`, `tilo do` and `shots` (AGENTS.md, "Driving a running desktop").

## Screenshots: `shot` and `vm shot`

- `tilo-vm shot` asks the account's compositor to render its current scene into
  a PNG (`tilo-control snapshot`). Headless account, headful account on its vkms
  seat, or an account holding seat0: the same call, the compositor's own frame.
  It never reads a display, so it cannot show what the kernel scanned out.
- `tilo-vm vm shot` is the hypervisor's screenshot of the VM's real display,
  seat0: greetd's greeter, or the account that claimed it. Headless and vkms
  accounts never appear there. It needs no SSH.
  A VM created with `--gl` renders through virgl into an `egl-headless`
  display, and QEMU keeps no surface the machine can read for that scanout:
  `vm shot` fails with `no surface`. For a frame from outside the guest, boot
  the VM without `--gl` (software GL) where the scanout is readable, or
  use `tilo-vm shot` while the compositor is up; the guest's serial line and
  logs cover the moments it does not (`tilo-vm guide troubleshooting`).

## Real input devices (headful accounts)

A headful account has its own seat, and `tilo-vm input` sends through a
keyboard and a pointer plugged into that seat (uinput devices with
`phys=seat-<account>`), so libinput, key maps and hit-testing run for real:

```sh
tilo-vm input key ctrl+alt+t        # combos, pressed in order
tilo-vm input type 'hello world'    # US layout
tilo-vm input move 400 300          # desktop pixels (see below)
tilo-vm input click left --at 400 300
```

Touch, pen and game controllers work the same way. Each is a uinput device
plugged into the account's seat the first time a command needs it, so the
compositor sees a real touchscreen (libinput touch events), a real pen
(libinput tablet tool) and a real controller (evdev, opened through the
seat's device access):

```sh
tilo-vm input touch tap 400 300
tilo-vm input touch hold 400 300 --duration 1200     # long-press
tilo-vm input touch drag 400 600 400 100             # swipe up
tilo-vm input touch pinch 640 400 --from 100 --to 400
tilo-vm input pen drag 100 100 500 300 --pressure 80
tilo-vm input gamepad press a                        # printed legends
tilo-vm input gamepad press guide+rb
tilo-vm input gamepad dpad down down
tilo-vm input gamepad stick left 0 -100 --duration 600
```

Touch and pen coordinates are pixels of the account's first output: the
compositor puts a touchscreen that names no output on the primary one.

Headless accounts have no seat and refuse `input`; use `tilo-vm control
key|type|click` there. While an account holds seat0, its input goes to seat0.

## Opening apps: a throwaway identity

Everything in this guide drives the desktop over the control socket the image
binds at `$XDG_RUNTIME_DIR/tilo-session/test-control.sock`, and the shell that
bound it is gone. An account therefore fails these commands — `tilo-control` finds
no socket there — instead of accepting them. The session serves a socket of
its own, but only where the command line names one: `tilo-session
--test-control PATH`, or `TILO_TEST_CONTROL=PATH`
(development builds) answers `snapshot`, `click_at` and
`provision_test_identity`, not input or named controls.

A control-socket session skips the first-login flow, so a new account's user
has no identity and every app refuses to start. `--identity` gives it one:

```sh
tilo-vm account create apps --headful --identity
tilo-vm control --account apps state-dump --pretty
```

The session mints it through a control op (`provision_test_identity`); a
release build refuses it. The op belongs to the shell's socket, so an account
whose session is `tilo-session` cannot be given an identity this way yet, and
`--identity device` needs the shell too: it presses the onboarding controls by
name, which no session serves. The identity is
named "Throwaway test identity" and marked as one beside its record, and the
op never replaces or adopts a real identity. A newly minted identity comes
with fresh desktop state: the device stops the session, removes the user's
`~/.config/tilo/state.json` and the files beside it that belong to it
(`state.automerge`, its `.log` and `.meta`, `state.json.invocations`), and starts it
again, since a workspace made before the identity belongs to the Unix user
and hides app launches. Running the same create again keeps the identity and the layout.

Stand-in accounts set `TILO_IDENTITY_ROOT` to the account's actual home followed
by `/.local/share/tilo/test-identity`, for both the session and user manager.
The device key, owner public key and all device certificates stay there;
new throwaway personal identities use owner-only secret files in the same
user's identity config directory, even when normal sessions use a platform
secret store.
`account exec` inherits that identity root. The supported `tilo-vm account rm NAME`
stops the session and user manager, removes their drop-ins, and deletes the
account and home, including both identity trees. Real `device`/`vm` accounts do
not receive this override and cannot be removed this way.

To test what needs the machine's own identity (fs, home servers, anything
behind `/var/lib/tilo/device/cert.bin`), make one account this VM's person
instead:

```sh
tilo-vm account create me --headful --identity device
```

Its session gets no `TILO_IDENTITY_ROOT`, so it shows the first-login
onboarding a device shows, and the device presses through it by name (begin,
the acknowledgements, create, skip the backup and tour, enter) the way a
person's clicks would. Onboarding creates the identity and certifies the
machine's device key, exactly as on a device; that certificate starts
`fs.service`, which the create waits for. Nothing is minted or copied in. One
account per VM can be its person, and only on a VM whose identity tree holds no
one yet: the create refuses otherwise and never replaces or adopts an
identity. Removing that account removes the account, not the machine's
certificate; use a fresh VM for another person.

A throwaway cannot use the shared `/var/lib/tilo` identity tree. Its device
certificates expire after 24 hours; after expiry, remove and recreate the
account instead of treating a copied certificate as lasting access. Setting
`TILO_IDENTITY_ROOT` alone in a manual fixture does **not** isolate personal
identity records: also use a dedicated `HOME`/`XDG_CONFIG_HOME` and dispose of
both trees. Never point the override at a real account's device identity.

After upgrading the daemon, restart an existing stand-in account before asking
it to mint an identity, so its new environment takes effect. Older versions
could leave identity-keyed certificates in the shared tree after account
removal. This fix does not sweep or revoke those certificates: their roots
must be identified and reviewed individually, without deleting real-device
trust. Legacy throwaway secrets in a platform store also need reviewed
cleanup; deleting a home cannot remove those items. Deleting local files
also does not revoke copies held by peers.


## Several monitors, mixed scale

```sh
tilo-vm account create dual --headful --outputs 2560x1600@2,1920x1080@1
tilo-vm shot -o target/dual.png --account dual
tilo-vm input --account dual click left --at 1500 400   # on the second monitor
```

Each `--outputs` entry is one vkms connector (`Virtual-1`, `Virtual-2`, ...)
with its own CRTC, laid out left to right, top-aligned, the first one
primary. vkms offers the standard VESA (DMT) 60 Hz modes, like a monitor
without an EDID. The mode and scale are not injected: the device stages them
as a display change in the account user's settings, the compositor applies it,
and the device confirms it, exactly as the Settings app does, so the
compositor remembers the layout for that set of monitors. Outputs are fixed
when the account is created; running the same `account create` again re-applies
the layout, and a different list needs `tilo-vm account rm` first.

With `--outputs`, `input move` and `click --at` take logical desktop
pixels: `2560x1600@2,1920x1080` is 1280 + 1920 wide, and x 1280 is the left
edge of the second monitor.

## Third-party apps to check against

Desktop features that meet software Tilo did not write (XWayland, GTK
theming, dialogs as sheets, the tray, MIME handlers) need that software in
the VM. It is not in any image; install a set when you need it:

```sh
tilo-vm account apps list                        # the sets, what each app checks
tilo-vm account apps install --set verification  # xterm, xeyes, gtk4 demos, zenity, mupdf, nm-applet
tilo-vm input key super                       # start one from the launcher:
tilo-vm input type xterm
tilo-vm input key Return                      # then type try commands into xterm
```

The device runs the VM's own install line (`dnf install --transient` on the
dev image), so the packages sit in a transient overlay on `/usr` until the
next VM reboot, and restarts your account's session so it sees them. Other
accounts see them after their next restart. The sets are data:
`tools/tilo-vm/data/app-sets.toml`.

## seat0

```sh
tilo-vm seat claim                   # your session moves onto the real display (VT 1)
tilo-vm vm shot -o target/seat0.png  # what the hypervisor window shows now
tilo-vm seat release                 # back to your own seat; greetd returns
```

Only one account holds seat0. A claim on a held seat fails and names the holder;
`--steal` takes it anyway and moves the holder back to its own seat.

## When SSH is down

The hypervisor can still show you the VM: `tilo-vm vm shot`,
`tilo-vm vm serial`, `tilo-vm vm reset`, and `tilo-vm vm control ...`
(virsh, the QEMU monitor, utmctl or PowerShell). `tilo-vm doctor` lists what
your adapter supports.
