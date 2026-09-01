# Windows application presentation

This note is the runtime truth for the singleton Windows appliance. It is
intentionally narrower than the Windows Apps product flow: it describes what
the host and guest presentation stack actually enforce.

## Selection

At qOS session startup, the compatibility service runs
`/usr/libexec/qos-windows-rail --probe`. Native RAIL is selected only when the
immutable image contains both upstream FreeRDP and `xwayland-satellite`, and
the active qOS Wayland socket exists. Otherwise the authenticated guest bridge
is configured for app-focused SPICE. A failed RAIL launch never silently
normalizes into a Windows desktop; a later explicit launch may use SPICE only
after the service has selected that typed backend.

`xwayland-satellite` is required because the qOS compositor does not embed an
X window manager. The wrapper creates a private, random X display for one
launch, writes a mode-0600 MIT-MAGIC-COOKIE authority without exposing its
cookie in argv, disables the TCP X listener, starts FreeRDP, then kills the
satellite and removes the authority. Satellite maps each FreeRDP RAIL X window
to an ordinary Wayland `xdg_toplevel`, so qOS owns placement and chrome.

## One Windows identity and session

Guest enrollment creates exactly one fixed, non-administrator `qos-user`
service identity. It generates a strong random credential internally and
stores it only below the SYSTEM/Administrators-only guest bridge directory;
there is no account chooser, username field, or password prompt. Re-running
enrollment reuses the private credential and reconciles the local account.
An administrator can explicitly rotate it with
`-RotateManagedSessionCredential`; missing or damaged private state also
causes recovery rotation. The account cannot change its own password. The
bridge adds it to `Remote Desktop Users`, sets `fSingleSessionPerUser=1`, and
reuses that identity for every RAIL launch. It never creates a per-launch
account and never exposes this credential to ordinary users.

qOS does not configure Winlogon autologon: that mechanism persists a reusable
plaintext credential in Windows' interactive-login configuration. RAIL itself
establishes the service session. A SPICE recovery desktop can require an
administrator-guided recovery action if no matching interactive service
session exists; it does not turn this hidden service credential into a normal
login prompt.

FreeRDP does not currently multiplex later RemoteApp execute requests onto an
existing RAIL connection. qOS therefore serializes foreground RAIL launches to
preserve the one-session invariant. Supporting simultaneous independent
Windows application windows requires a reviewed FreeRDP connection-sharing
API or a qOS RAIL session broker; creating extra users/sessions is not an
acceptable workaround.

## Integrations and leases

All RDP redirectors start disabled: no home drive, arbitrary mount, printer,
smartcard, serial/parallel device, clipboard, file transfer, or microphone is
inferred from Windows or FreeRDP defaults. The live RAIL broker projects:

- text/image clipboard from the canonical `Device("clipboard")` grant; and
- microphone input through a private PipeWire/Pulse graph that exposes a
  source only for a canonical qOS microphone grant.

Both redirectors are live in the session composition and are revoked when the
RAIL process exits. A file projection is represented end-to-end as an opaque
portal handle plus its exact `read-only` or `read-write` decision. The launch
boundary rejects duplicate handles and rejects a broker attempt to turn a
read-only handle into a writable RDP drive. A materialising broker must expose
only a per-lease directory below `/run/qos/windows-vm/leases/<lease>/`; its
rootless bubblewrap mount namespace overlays read-only drives with `--ro-bind`,
rather than trusting an advisory FreeRDP flag. The projection is removed on
revocation.

When such a projection is present, FreeRDP redirects that narrow drive only.
Windows can copy or drag the projected file from the drive into a published
application. Cross-desktop RDP clipboard file transfer remains disabled even
with a text clipboard grant: FreeRDP would otherwise resolve an ambient X11/
compositor file offer, which has no portal handle. Raw workspace paths are not
a safe substitute. Audio playback is live through the same private graph.
Ordinary application egress uses the host-only, token-authenticated `NetHost`
relay; the guest has no NAT route. Active applications in the one shared
Windows session receive the effective union, which the management UI reports.
The separate all-or-nothing NAT lease remains limited to owner-authorised
activation and Windows Update maintenance.

The manager receives a server-derived `VmSharedSessionLeaseSummary` inside
`VmAvailability.shared_session`. It contains `active`, a sorted
`effective_grants` list, and active contributors identified by authenticated
guest publication ID and display name. The summary is recorded from the
capability broker's materialized result, not client input. This reflects the
real security boundary: Windows applications in one user/session can see the
session-wide union of redirects and are not mutually isolated.

SPICE remains stricter. Libvirt blocks SPICE clipboard and file transfer in
the server XML, has no USB redirection device, and exposes only a local
libvirt-attached display. Provisioning installs the SPICE vdagent from Fedora's
attached, versioned `virtio-win` media only after verifying both the host-pinned
SHA-256 and a Windows-valid Authenticode publisher signature, then verifies its `vdservice`;
qOS never downloads a mutable guest-tools executable. The explicit
virtio-serial/spicevmc channel then carries cursor and arbitrary-resolution
requests. The viewer requests auto-resize, local cursor rendering, 100% zoom,
and reconnect. The server-side clipboard/file blocks remain authoritative even
when vdagent is running.

## Latency, scaling, and graphics

The local RAIL connection uses the required `||alias` RemoteApp form, LAN
tuning, AVC444 graphics, bounded automatic reconnect, ungrabbed qOS keyboard
handling, and low-latency Pulse output. Desktop-only dynamic-resolution and
appearance flags are not sent to RAIL. `xwayland-satellite` handles compositor scaling and
fractional-scale/viewport protocols when available; mixed-DPI behavior remains
limited by its smallest-output DPI policy and the Windows application's DPI
awareness.

The VM already uses KVM, host CPU passthrough, Hyper-V enlightenments,
virtio-scsi with native I/O, discard, and a fixed 8 GiB/4-vCPU baseline. For a
machine with one ordinary GPU, RDP AVC444 or the 2D SPICE fallback is the safe
default: Windows has no production virgl/virtio-gpu 3D path equivalent to a
Linux guest, and passing the only GPU through would remove it from qOS.
Hardware 3D remains gated on an independently assignable second GPU or a
hardware/driver-supported SR-IOV VF or mediated device. The read-only
`qos-windows-vm-control graphics-capabilities` probe reports those kernel
interfaces but never creates or automatically assigns a partition. A separate
polkit owner action can persistently attach only an already-created,
IOMMU-isolated, vfio-bound partition while the singleton is off; detach uses
qOS's exact root-only record. Looking Glass is useful only after such
assignment and is still a whole framebuffer, not RAIL. See
`windows-presentation-capabilities.md` for the exact probe and source contracts.

## Live versus gated

Live in the shipped image:

- singleton persistent VM and persistent managed Windows account/session;
- certificate-pinned, HMAC-authenticated allow-listed launches;
- rootless native RAIL when client Windows/Server accepts RAIL;
- native qOS toplevels, resize requests, normal keyboard/pointer input, HiDPI
  mediation, output-only audio, and focused SPICE fallback;
- signed Fedora virtio-win guest tools, Windows vdagent-backed SPICE resize and
  cursor integration, with clipboard/file transfer still blocked in libvirt;
- qOS portal picker to opaque, one-shot RAIL document drive, read-only by
  default and enforced in a rootless mount namespace;
- filtered shared-session `NetHost` egress without a guest NAT route;
- server-derived shared-session permission reporting; and
- KVM/host-passthrough storage and CPU tuning.

Still gated by a missing authority, guest component, upstream limitation, or
hardware:

- ambient cross-desktop file clipboard drag/drop (the explicit **Open file…**
  portal flow is the secure supported replacement);
- simultaneous apps on one FreeRDP RAIL connection;
- production 3D acceleration on a single non-partitionable GPU; and
- native RAIL itself on client Windows releases where the unsupported host
  behavior rejects it.
