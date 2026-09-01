# Windows fallback VM

> **Status (2026-09): provisioning and boundary scaffolding.** The singleton
> appliance model, host-only libvirt network, guest payload, authenticated
> RemoteApp DTOs, and capability checks exist as components. The qOS session
> daemon has not yet connected these pieces end to end, so ordinary launches do
> not currently provision or publish live RemoteApps. The operational flow
> below is the target unless explicitly marked implemented.

qOS has one Windows appliance per user. It is a last-resort compatibility
domain, not a second qOS session and not a way to place arbitrary programs in
a less confined desktop.

## Provisioning (target operational flow)

The owner supplies a Windows installation ISO to `qos-windows-provision
prepare --iso /absolute/path`. The tool copies it to root-only state, creates
one qcow2 disk, and makes a small guest-tools ISO. It deliberately does not
accept, log, or retain a product key. Complete Windows Setup and enter any
licence in the explicitly owner-authorised provisioning console.

For the target normal qOS application presentation, use a Windows Server ISO licensed for
the official Remote Desktop Services RemoteApp role. The guest payload refuses
consumer Windows editions rather than using RDP Wrapper, patching `termsrv`, or
pretending an ordinary RDP desktop is a RemoteApp window. A client Windows ISO
can be used only for the recovery console and is not a successful appliance
configuration.

## Guest setup (target operational flow)

In the provisioning console, attach `qos-guest-tools.iso`, copy the example
application manifest to a local administrator-controlled location, replace its
entries with the exact approved applications, then run:

```powershell
Set-ExecutionPolicy RemoteSigned -Scope Process
& D:\Install-qOSRemoteApp.ps1 -BootstrapRoot D:\ -ApplicationsPath C:\Admin\qos-published-applications.json
```

The script accepts only existing executables under `C:\Program Files`,
`C:\Program Files (x86)`, or `C:\Windows`; it writes the official RemoteApp
allow-list and keeps each published alias stable. Discovery does not equate to
publication.

qOS also ships a pinned copy of Bottles' upstream program metadata to improve
catalogue matching. It is read-only reference data: only identities and runner
hints may be imported. qOS never executes a record's Steps, URLs, shell text,
or dependency instructions.

Detach the guest-tools ISO immediately after setup through the owner-authorised
console. It carries a bootstrap key for first enrollment and must not remain
readable to RemoteApp users.

## Boundary (implemented contract; end-to-end wiring is roadmap)

The libvirt network has `forward mode='none'`; it is host-only, without a
bridge to a physical NIC or a NAT escape hatch. RDP accepts only its host-side
address. The guest bridge is HTTPS on that network and requires fresh HMAC
headers from a root-only bootstrap key. qOS pins the guest TLS fingerprint
after authenticated enrollment; a change requires repair rather than an
automatic trust update.

The target bridge returns a fresh temporary RemoteApp account and password for
one launch. The account is removed after ten minutes even if the client crashes.
This limits credential reuse; it does not make two Windows programs inside the
singleton VM mutually isolated. Files, clipboard, microphone, and egress are
still granted by qOS's existing workspace broker, not by this guest setup.

The target full desktop is available only through the owner-authorised recovery action
for installation or repair. qOS's privileged owner-authorisation backend may
start the domain and attach installation media; it then invokes the viewer as
the active owner session, never as root. It is never a normal launch fallback.
