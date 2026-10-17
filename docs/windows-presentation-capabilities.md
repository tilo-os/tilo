# Windows presentation capability notes

This is the implementation and operator note for the presentation pieces that
depend on a particular Windows SKU, guest package, or GPU. It records what Tilo
can probe and enforce; detection is not presented as hardware assignment.

## Native RemoteApp

The Fedora 44 image installs the distribution packages `freerdp` and
`virt-viewer` from Tilo's digest-pinned RPM snapshot. Fedora's `freerdp`
package provides `/usr/bin/xfreerdp`. `/usr/bin/xwayland-satellite` is built
from pinned upstream source with a clipboard fix
(`os/external-assets/xwayland-satellite/`), and Xwayland comes from the
snapshot. Satellite forwards `-auth` and `-nolisten` to Xwayland in the 0.8
series used by the
image. Tilo checks both binaries and the active Wayland socket before selecting
native RAIL.

FreeRDP's current command contract describes a published RemoteApp alias as
`/app:program:||alias`. Tilo supplies that exact prefix, certificate pinning,
password input on stdin, and three bounded automatic reconnect attempts. It
does not pass desktop-only dynamic-resolution, wallpaper, theme, Aero, or
window-drag flags into a RAIL session. The upstream FreeRDP maintainer has
specifically identified those sizing/appearance switches as desktop-session
options when diagnosing RAIL resize faults.

This fixes a concrete client-side interoperability error; it does not turn
client Windows into a supported RemoteApp host. Windows Server with a licensed
Remote Desktop Services RemoteApp deployment is the supported host. Client
Windows still receives the native `TSAppAllowList` setup as best effort, with
no RDP Wrapper, service patch, or replacement `termsrv` binary. If it rejects
RAIL, Tilo reports that backend failure and offers the explicitly labelled
whole-surface SPICE presentation.

Primary references:

- FreeRDP command syntax and current option definitions:
  <https://github.com/FreeRDP/FreeRDP/blob/master/client/common/cmdline.h>
- FreeRDP RemoteApp notes, including the required `||` alias marker:
  <https://github.com/FreeRDP/FreeRDP/wiki/RemoteApp>
- xwayland-satellite command line and forwarded Xwayland flags:
  <https://github.com/Supreeeme/xwayland-satellite/blob/main/xwayland-satellite.man>
- Fedora package contents: <https://packages.fedoraproject.org/pkgs/freerdp/freerdp/>
  and <https://packages.fedoraproject.org/pkgs/xwayland-satellite/xwayland-satellite/>

## Windows SPICE agent and resize

Tilo does not fetch `spice-guest-tools-latest.exe`. Provisioning already
attaches `/usr/share/virtio-win/virtio-win.iso` from Fedora's `virtio-win`
package. `Install-TiloRemoteApp.ps1` locates the versioned guest-tools bundle on
that read-only CD, requires its SHA-256 to match the installer in the host's
digest-pinned RPM payload, requires Windows to validate its Authenticode
publisher signature and trust chain, launches its visible passive installer,
and verifies that the `vdservice` SPICE agent exists and runs automatically.
Signature failure stops installation with recovery guidance to check the
Windows clock/trusted roots and reattach the official media.
`-SkipSpiceGuestTools` exists only for a guided
repair flow; using it means automatic guest resolution changes remain
unavailable.

The domain now declares the virtio-serial controller and
`com.redhat.spice.0` spicevmc channel required by the agent. `virt-viewer`
attaches through local libvirt with `--attach`, requests
`--auto-resize=always`, automatic reconnect, 100% zoom, and a local
cursor. The SPICE server independently keeps clipboard and file transfer off,
so installing vdagent does not grant those capabilities.

The guest-tools bundle and Windows vdagent are built from published open
sources and distributed by Fedora's `virtio-win` package; its Fedora spec also
ships corresponding sources and license notices. Tilo redistributes only the
unchanged, repository-reviewed Fedora payload already present in the immutable
image.

Primary references:

- SPICE Windows guest tools and vdagent sources:
  <https://www.spice-space.org/download.html>
- SPICE's required virtio-serial/channel domain configuration:
  <https://www.spice-space.org/spice-user-manual.html>
- virtio-win guest-tools installer composition:
  <https://github.com/virtio-win/virtio-win-guest-tools-installer>
- Fedora virtio-win package spec, including the spice-vdagent source and
  installer payload: <https://github.com/virtio-win/virtio-win-pkg-scripts/blob/master/virtio-win.spec>
- virt-viewer flags: <https://gitlab.com/virt-viewer/virt-viewer/-/blob/master/man/virt-viewer.pod>

## Graphics on a one-GPU host

`tilo-windows-vm-control graphics-capabilities` is a read-only JSON probe which
works before the Windows appliance exists. It examines only PCI display-class
devices and the kernel's documented interfaces:

- `sriov_totalvfs`, `sriov_numvfs`, and `virtfnN` for SR-IOV;
- `mdev_supported_types/*/{device_api,available_instances,devices}` for mdev;
- the VF's driver and IOMMU group before calling an existing VF
  `vfio_ready`; the VF must be the group's only member, be display class, and
  not be the boot VGA device.

The result is one of `paravirtual_2d`, `partition_capable_not_configured`, or
`partition_ready_for_explicit_assignment`. `automatic_assignment` is always
false. The probe never writes `sriov_numvfs`, creates an mdev, unbinds a host
driver, or detaches a boot/display GPU.

An owner may attach only a partition which the probe reports ready:

```console
$ tilo-windows-owner graphics-capabilities
$ pkexec /usr/libexec/tilo-windows-owner lifecycle shutdown
$ pkexec /usr/libexec/tilo-windows-owner graphics-attach sriov-vf 0000:03:00.1
# or: graphics-attach mdev 53764d0e-85a0-42b4-af5c-2046b460b1dc
$ pkexec /usr/libexec/tilo-windows-owner lifecycle start
```

The controller re-probes at attachment time, accepts only a display VF already
bound to `vfio-pci` with an IOMMU group or an already-created `vfio-pci` mdev,
and persists a `managed='no'` hostdev only while the singleton is off. It never
unbinds or manages the parent device. Detach also requires shutdown and uses
only Tilo's root-only record of the exact device:

```console
$ pkexec /usr/libexec/tilo-windows-owner lifecycle shutdown
$ pkexec /usr/libexec/tilo-windows-owner graphics-detach
```

This makes prepared single-GPU partitions usable without pretending every GPU
can be partitioned. The Windows vendor driver must support that VF/mdev; Tilo
does not claim hardware encode, RAIL, or SPICE framebuffer integration merely
because PCI attachment succeeded.

Without such a reviewed partition, the safe production path remains RDP's
graphics pipeline for RAIL or a 2D virtio-gpu/SPICE surface. Domain XML now
states `accel3d='no'` explicitly: enabling virgl on the Linux host would not
manufacture a supported Windows virgl driver. The Fedora virtio-win guest
tools install the applicable Windows paravirtual driver and SPICE agent, which
improves display, storage, input, and network behavior without taking Tilo's
only physical GPU away.

Primary references:

- Linux PCI SR-IOV sysfs interface:
  <https://docs.kernel.org/PCI/pci-iov-howto.html>
- Linux VFIO mediated-device sysfs interface:
  <https://docs.kernel.org/driver-api/vfio-mediated-device.html>
- libvirt video acceleration and device models:
  <https://libvirt.org/formatdomain.html#video-devices>
- Red Hat's Windows virtio-driver guidance and warning that directly assigning
  a GPU makes it unavailable to the host:
  <https://docs.redhat.com/en/documentation/red_hat_enterprise_linux/10/html-single/configuring_and_managing_windows_virtual_machines/>
