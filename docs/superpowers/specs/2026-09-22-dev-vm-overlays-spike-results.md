# Dev VM overlays: spike results

Validates the two early spikes of `2026-09-22-dev-vm-overlays-design.md` on a real VM.

## Environment

- Guest: stock `Fedora-Cloud-Base-Generic-44-1.7.x86_64.qcow2`, QEMU/KVM q35, 4 vCPU, 4 GiB,
  headless (`-display none`, so seat0's card is `bochs-drm`), cloud-init NoCloud seed.
- Kernel `6.19.10-300.fc44.x86_64`, systemd `259.5-1.fc44`, seatd/libseat `0.9.3`, libinput
  `1.31.3`, bubblewrap `0.12.0`, selinux-policy-targeted `43.3-1.fc44` (**enforcing**), pam `1.7.2`.
- Everything ran as root in the guest. The probe program `seatprobe` (C, about 150 lines) opens the
  seat with libseat (`LIBSEAT_BACKEND=logind`), opens the seat's DRM card via
  `libseat_open_device`, checks `drmIsMaster`, performs a legacy modeset of a CPU-filled dumb
  buffer, then runs a libinput udev context on the seat and logs device and key/pointer events.

## Verdicts

| # | Question | Verdict |
|---|---|---|
| 1a | vkms devices via configfs | **Works.** vkms configfs was merged upstream for Linux 6.19; older kernels were not tested here. Fedora 44's 6.19.10 has it; vkms is `=m` in `kernel-modules-core`. |
| 1b | One logind seat per vkms card via udev `ID_SEAT` | **Works.** One generic rule; no `loginctl attach`. |
| 1c | logind session per seat from a system unit (`PAMName=`, `User=`, `XDG_SEAT=`) | **Works.** `Active=yes State=active Type=wayland` on each non-VT seat. No VT or TTY is needed. |
| 1d | DRM master via libseat in that session | **Works.** `drmIsMaster=1` and modeset `rc=0` on each vkms card. A session on seat B is refused seat A's card (`Permission denied`). |
| 1e | Per-seat `uinput` devices seen by libinput on that seat only | **Works.** Seat A's session got 18 events from its own devices and none from seat B's. |
| 1f | Add/remove connectors at runtime | **Works with caveat.** You cannot add or remove connectors while the device is enabled (`EBUSY`). You *can* toggle `connectors/<n>/status` (1 = connected, 2 = disconnected) at runtime, and this sends a `HOTPLUG=1` uevent. |
| 1g | `seat claim` on seat0/vt1 via a drop-in | **Works.** The session gets `Seat=seat0 VTNr=1 TTY=tty1`, sees the real card and seat0's input devices, and switches back cleanly on release. |
| 2a | `ExtensionDirectories=` on `user@<uid>.service`: user units see the layer | **Works with caveat (SELinux labels).** |
| 2b | Same on the session unit: the session process sees the layer | **Works with caveat (SELinux labels).** |
| 2c | `bwrap --ro-bind /usr /usr` child sees the layer | **Works from user units. Fails when run directly by the session process**, because of an ambient capability that pam_systemd grants (details below). |
| 2d | Other users do not see the layer | **Works.** The host root and `tdo-b` get `No such file or directory`. |
| 2e | `ProtectSystem=strict`, `PrivateTmp=`, `ProtectHome=` in user units | **Works.** A nested namespace inherits the overlaid `/usr`. |
| 2f | Layer change needs | **A restart of the unit.** `systemctl --user daemon-reload` and `daemon-reexec` keep the old view. Restarting `user@<uid>` does *not* update a running session unit, so both must restart. |

## 1. vkms multi-seat

### Kernel

- vkms configfs (`/sys/kernel/config/vkms/<dev>/{planes,crtcs,encoders,connectors,enabled}`)
  is present in 6.19.10 and was verified end to end. Load the module with `create_default_dev=0`
  so no stray default card appears on seat0.
- The device appears as `/sys/devices/faux/<dev>/drm/cardN` (the parent is a `faux` bus device
  named after the configfs dir). Every vkms card also gets an fbdev (`fbN: vkmsdrmfb`), which is
  harmless.
- **Tilo's kernel:** `os/kernel/config-fragments/tilo.config` merges onto Fedora's config and
  does not mention `DRM_VKMS`, so it inherits `=m`. The payload spec installs all modules.
  Recommendation: pin `CONFIG_DRM_VKMS=m`, `CONFIG_INPUT_UINPUT=m|y` and `CONFIG_CONFIGFS_FS=y`
  explicitly in the fragment, and add a dev-image check that `modprobe vkms` works. That way a
  future config trim cannot silently remove them.

### `/usr/local/sbin/vkms-seat` (what `tilo-devd` has to do)

```bash
#!/bin/bash
# vkms-seat add <seatname> [connectors]   |  vkms-seat del <seatname>
set -e
V=/sys/kernel/config/vkms/$2
case $1 in
add)
  modprobe vkms create_default_dev=0
  n=${3:-1}
  mkdir $V $V/planes/p0 $V/crtcs/c0 $V/encoders/e0
  echo 1 > $V/planes/p0/type                     # primary
  ln -s $V/crtcs/c0 $V/planes/p0/possible_crtcs   # symlink targets must be absolute (or ../../../)
  ln -s $V/crtcs/c0 $V/encoders/e0/possible_crtcs
  for i in $(seq 0 $((n-1))); do
    mkdir $V/connectors/n$i
    ln -s $V/encoders/e0 $V/connectors/n$i/possible_encoders
    [ $i -gt 0 ] && echo 2 > $V/connectors/n$i/status   # spare connectors start unplugged
  done
  echo 1 > $V/enabled ;;
del)
  echo 0 > $V/enabled
  find $V -type l -delete
  rmdir $V/connectors/* $V/encoders/* $V/crtcs/* $V/planes/* $V ;;
esac
```

`vkms-seat add seat-a 2; vkms-seat add seat-b 1; vkms-seat add seat-c 1` gives
`loginctl list-seats` → `seat-a seat-b seat-c seat0`.

### `/etc/udev/rules.d/72-vkms-seat.rules`

```
# vkms devices created via configfs under the name "seat-<x>" become logind seat "seat-<x>"
SUBSYSTEM=="drm", KERNEL=="card[0-9]*", KERNELS=="seat-*", SUBSYSTEMS=="faux", ENV{ID_SEAT}="$id"
```

The stock `71-seat.rules` already tags DRM cards `seat` + `master-of-seat`, so setting `ID_SEAT`
is enough. The connector sub-devices inherit it. Logind seat names must start with `seat`, which
is why the configfs dir is named `seat-<overlay>`.

### `/etc/udev/rules.d/73-uinput-seat.rules`

```
# uinput devices whose phys is "seat-<x>" belong to logind seat "seat-<x>".
SUBSYSTEM=="input", ATTRS{phys}=="seat-*", ENV{ID_SEAT}="$attr{phys}"
```

The creator sets the phys string (`UI_SET_PHYS`; python-evdev `UInput(..., phys="seat-a")`). Both
`inputN` and `eventN` get `ID_SEAT=seat-a`. A root creator (tilo-devd) is fine; the session opens
the devices through logind `TakeDevice`, so no ACLs are needed.

### `/etc/systemd/system/tilo-dev-session@.service`

```ini
[Unit]
Description=Tilo dev session for overlay %i
After=systemd-user-sessions.service systemd-logind.service
Wants=systemd-logind.service

[Service]
Type=simple
User=tdo-%i
PAMName=greetd
Environment=XDG_SEAT=seat-%i
Environment=XDG_SESSION_TYPE=wayland
Environment=XDG_SESSION_CLASS=user
Environment=XDG_SESSION_DESKTOP=tilo
Environment=LIBSEAT_BACKEND=logind
SyslogIdentifier=tilo-session-%i
ExecStart=/usr/libexec/tilo-session --backend drm
Restart=always
RestartSec=2
```

Both `PAMName=login` and `PAMName=greetd` were tested; the latter used the repo's
`os/etc/pam.d/greetd`. Both give active seat sessions. Use **`greetd`**, because it is the
production stack (no drift). Fedora's `login` also pulls in `postlogin` → `pam_lastlog2.so`, which
fails to dlopen on F44 (`undefined symbol: pam_syslog`; it is optional, so this is just log noise).

Observed:

```
SESSION  UID USER   SEAT   LEADER CLASS   TTY IDLE SINCE
     22 1001 tdo-a  seat-a 2448   user    -   no   -
     23 1002 tdo-b  seat-b 2449   user    -   no   -
Seat=seat-a VTNr=0 Type=wayland Class=user Active=yes State=active Service=login
seatprobe: libseat seat name: seat-a / opened /dev/dri/card1 drmIsMaster=1 / modeset 1024x768: rc=0
```

Behaviour worth knowing:

- pam_systemd moves the session process into `session-N.scope`, so **`journalctl -u
  tilo-dev-session@a` shows none of its output.** Use `SyslogIdentifier=tilo-session-%i` and
  `journalctl -t tilo-session-a` (verified), or `_SYSTEMD_UNIT=session-N.scope`.
- `Restart=always` works. Killing the leader restarted the unit (`NRestarts=1`) with a new active
  session on the same seat. `systemctl stop` removes the session.
- **Disabling a vkms device under a live session** (`echo 0 > enabled`) succeeds immediately.
  The seat and the logind session disappear, but the process keeps running with a dead fd, so
  the unit stays `active` and `Restart=` never fires. Re-enabling creates a *new* minor
  (`card2` → `card4`). tilo-devd must **stop the session unit before disabling or reconfiguring
  the vkms device**, and restart it afterwards.
- Hotplug: `echo 1|2 > connectors/nK/status` emits `ACTION=change HOTPLUG=1` on the card. The
  DRM master sees the new state on its next probe. The sysfs `cardN-*/status` file stays stale
  until a master probes, so tests must not read sysfs.

### Seat claim: `/run/systemd/system/tilo-dev-session@a.service.d/50-seat0.conf`

```ini
[Unit]
Conflicts=getty@tty1.service
After=getty@tty1.service

[Service]
Environment=XDG_SEAT=seat0 XDG_VTNR=1
TTYPath=/dev/tty1
TTYReset=yes
TTYVHangup=yes
TTYVTDisallocate=yes
StandardInput=tty-fail
StandardOutput=journal
StandardError=journal
```

`systemctl daemon-reload && systemctl restart tilo-dev-session@a` → `Seat=seat0 VTNr=1 TTY=tty1
Active=yes`. The session opens the seat0 card and all seat0 input devices. Release: remove the
drop-in, run `daemon-reload`, and restart the unit (back on `seat-a`), then start greetd again. On
the real image, `Conflicts=` should name `greetd.service`. It was tested against `getty@tty1`
because the cloud image has no greetd.

## 2. Per-user `/usr` overlay

### Layer format

```
/var/lib/tilo-dev/layers/<name>/usr/bin/hello-overlay                        (0755)
/var/lib/tilo-dev/layers/<name>/usr/lib/extension-release.d/extension-release.<name>   → ID=_any
```

`<name>` must equal the directory basename, so content-addressed dirs use
`extension-release.<sha256>`.

**SELinux (enforcing) is the main caveat.** Files created under `/var/lib` are labelled
`var_lib_t`, and executing them through the overlay is denied (`avc: denied { execute } …
tcontext=…var_lib_t`, which shows up as `Permission denied`). Fix it by labelling each layer as if
it were mounted at `/`:

```bash
setfiles -r /var/lib/tilo-dev/layers/<name> \
  /etc/selinux/targeted/contexts/files/file_contexts /var/lib/tilo-dev/layers/<name>/usr
# usr/bin/* -> bin_t, usr/lib/* -> lib_t, ...
```

The same applies to `vm` layers in `/var/lib/extensions`. The layer builder must run this after
assembly and before a layer counts as immutable.

### Drop-ins

`/etc/systemd/system/user@1001.service.d/50-overlay.conf`:

```ini
[Service]
ExtensionDirectories=/var/lib/tilo-dev/layers/<l1> /var/lib/tilo-dev/layers/<l2>
```

`/etc/systemd/system/tilo-dev-session@a.service.d/50-overlay.conf`: the same
`ExtensionDirectories=` line.

- **Precedence: later entries win.** `l1 l2` resolves to l2's file, and `l2 l1` to l1's. The
  resulting `/usr` is `overlay ro lowerdir=/run/systemd/unit-extensions/1/usr:…/0/usr:/run/systemd/mount-rootfs/usr`.
  Mount propagation of `/` in the namespace is `shared,slave`.
- `user@.service` has no `ProtectSystem=` (only `Delegate=` and `KillMode=mixed`), so nothing
  conflicts.

### Results (tdo-a has the overlay, tdo-b does not)

```
== host root:                    No such file or directory
== tdo-a session process:        hello from layer l1
   user unit (systemd-run --user): hello from layer l1
   user unit ProtectSystem=strict PrivateTmp=yes ProtectHome=read-only: hello from layer l1
   user unit bwrap --ro-bind /usr /usr --unshare-all: hello from layer l1
   session bwrap:                bwrap: Unexpected capabilities but not setuid, old file caps config?
== tdo-b session / user units / bwrap: No such file or directory
```

**bwrap under the session process.** pam_systemd (since v254) gives sessions on a local seat
the ambient capability `CAP_WAKE_ALARM` (`CapAmb=0000000800000000`), and an unprivileged bwrap
refuses to run with any capability. **This differs from production.** greetd's worker calls
`setuid()` after `pam_open_session`, and the kernel clears the ambient set on a root-to-user
uid switch (`cap_emulate_setxuid`). A systemd unit with `PAMName=` re-raises PAM's ambient
capabilities after the switch (`exec-invoke.c`). A real greetd session therefore has
`CapAmb=0`, and the shell spawns `tilo-launcher` directly without trouble. The dev session unit
must match it with `CapabilityBoundingSet=~CAP_WAKE_ALARM`, which is verified to let bwrap run,
and tilo-devd checks `CapAmb`/`CapEff` of the slot's shell against that expectation.

### Changing layers

| Action | Running user units see |
|---|---|
| Edit the drop-in and run `systemctl daemon-reload` | old layer |
| `systemctl --user daemon-reload` | old layer |
| `systemctl --user daemon-reexec` (same PID, same namespace) | old layer |
| `systemctl restart user@<uid>` | new layer, but a running session unit still sees the **old** one |
| stop the session unit, restart `user@<uid>`, start the session unit | new layer everywhere |

**The mount namespace freezes host `/usr` at start.** `systemd-sysext refresh` of a VM-wide
layer is visible at once to the host and to non-overlay users. Overlay namespaces keep the old
view in both directions: a newly merged file is absent, and an unmerged one is still present.
This lasts until their units restart. The overlay's lowerdir is a snapshot of `/usr` taken when
the namespace was built.

## Spec changes

1. **Kernel requirement:** vkms configfs needs Linux 6.19 or newer (the upstream merge
   version, verified on 6.19.10). Pin `CONFIG_DRM_VKMS=m`,
   `CONFIG_INPUT_UINPUT`, and `CONFIG_CONFIGFS_FS=y` in `os/kernel/config-fragments/tilo.config`,
   and check them in the dev image.
2. **Session unit uses `PAMName=greetd`**, not `login`, so the PAM stack matches production.
   Add `SyslogIdentifier=tilo-session-%i`. `tilo-dev logs` reads by identifier (or by the
   session scope), not by `-u`.
3. **Hotplug semantics:** "connectors can be added and removed at runtime" becomes "each vkms
   card is created with N connectors (N is fixed per overlay). Connecting and disconnecting
   them at runtime is hotplug". Changing N means stopping the session, then disable,
   reconfigure and enable the card, then starting the session. The card gets a new minor.
4. **Seat lifecycle rule:** tilo-devd stops the session unit before it disables or removes a
   vkms device. Otherwise the unit keeps running with no seat.
5. **Seat and device naming:** the configfs dir is named `seat-<overlay>` and becomes logind
   seat `seat-<overlay>`. uinput devices use `phys=seat-<overlay>`. Two generic udev rules do
   the mapping, with no per-seat rules and no `loginctl attach`.
6. **SELinux labelling is part of the layer format:** every layer is `setfiles -r`-labelled
   against the image's `file_contexts` before it is sealed. This applies to both overlay layers
   and `vm` layers.
7. **Swap = restart both units:** stop the session unit, restart `user@<uid>`, start the session
   unit. `daemon-reexec` is not enough.
8. **VM-wide swaps restart every overlay:** after `systemd-sysext refresh`, restart all overlay
   session units and overlay `user@` units, not just "the affected system unit". Otherwise
   overlays keep the old VM-wide components.
9. **Capability parity:** the session unit sets `CapabilityBoundingSet=~CAP_WAKE_ALARM` so
   that, like a greetd session, it has no ambient capabilities. tilo-devd verifies this for each
   slot's shell.
10. The first-listed layer in `ExtensionDirectories=` is the bottom of the stack, and the last
    wins. Generation files should record layers in that order.

Not covered here: running real `greetd`/`tilo-session` binaries (the spike used a stand-in probe);
the SELinux domain of dev sessions compared with greetd sessions (the dev unit ran as
`unconfined_service_t` with `PAMName=greetd` and as `unconfined_t` with `login`, and a real
greetd session should be compared); and reflink deduplication.
