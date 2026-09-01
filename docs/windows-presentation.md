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
RAIL process exits. Clipboard file transfer remains disabled even with a text
clipboard grant: the compatibility protocol does not yet carry opaque portal
document handles, and raw workspace paths are not a safe substitute for a
file-portal selection. Audio playback is live through the same private graph.
The current VM egress mechanism is an all-or-nothing NAT NIC for activation
and Windows Update; it cannot enforce a canonical filtered `NetHost` list, so
ordinary application egress is not projected.

The manager receives a server-derived `VmSharedSessionLeaseSummary` inside
`VmAvailability.shared_session`. It contains `active`, a sorted
`effective_grants` list, and active contributors identified by authenticated
guest publication ID and display name. The summary is recorded from the
capability broker's materialized result, not client input. This reflects the
real security boundary: Windows applications in one user/session can see the
session-wide union of redirects and are not mutually isolated.

SPICE remains stricter. Libvirt blocks SPICE clipboard and file transfer in
the server XML, has no USB redirection device, and exposes only a local
libvirt-attached display. The spicevmc channel exists for cursor and arbitrary
resolution support, but those features remain gated until the immutable guest
tools media carries and installs a reviewed Windows SPICE agent. The viewer
already requests auto-resize, local cursor rendering, 100% zoom, and reconnect.

## Latency, scaling, and graphics

The local RAIL connection uses LAN tuning, AVC444 graphics, dynamic resolution,
full window drag, font/theme composition, ungrabbed qOS keyboard handling, and
low-latency Pulse output. `xwayland-satellite` handles compositor scaling and
fractional-scale/viewport protocols when available; mixed-DPI behavior remains
limited by its smallest-output DPI policy and the Windows application's DPI
awareness.

The VM already uses KVM, host CPU passthrough, Hyper-V enlightenments,
virtio-scsi with native I/O, discard, and a fixed 8 GiB/4-vCPU baseline. For a
machine with one ordinary GPU, RDP AVC444 or the 2D SPICE fallback is the safe
default: Windows has no production virgl/virtio-gpu 3D path equivalent to a
Linux guest, and passing the only GPU through would remove it from qOS.
Hardware 3D remains gated on an independently assignable second GPU or a
hardware/driver-supported SR-IOV mediated function. Looking Glass is useful
only after such assignment and is still a whole framebuffer, not RAIL.

## Live versus gated

Live in the shipped image:

- singleton persistent VM and persistent managed Windows account/session;
- certificate-pinned, HMAC-authenticated allow-listed launches;
- rootless native RAIL when client Windows/Server accepts RAIL;
- native qOS toplevels, resize requests, normal keyboard/pointer input, HiDPI
  mediation, output-only audio, and focused SPICE fallback;
- server-derived shared-session permission reporting; and
- KVM/host-passthrough storage and CPU tuning.

Still gated by a missing authority, guest component, upstream limitation, or
hardware:

- portal file drives, file drag/drop, and ordinary app network egress;
- SPICE automatic Windows resolution changes (Windows vdagent is not yet on
  the qOS guest-tools image);
- simultaneous apps on one FreeRDP RAIL connection;
- production 3D acceleration on a single non-partitionable GPU; and
- native RAIL itself on client Windows releases where the unsupported host
  behavior rejects it.
