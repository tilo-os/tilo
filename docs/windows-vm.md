# Windows fallback VM

> **Status (2026-09): live, provisioned fallback.** The qOS session composes the
> singleton libvirt appliance, authenticated and certificate-pinned guest
> bridge, managed installer catalogue, and app-focused SPICE presentation. An
> unprovisioned or unhealthy guest remains unavailable without affecting
> Wine/Proton applications.

qOS has one Windows appliance on the system. It is a last-resort compatibility
domain, not a second qOS session and not a way to place arbitrary programs in
a less confined desktop.

## Provisioning

Windows Apps asks the owner to select a Windows installation ISO through the
normal qOS file portal. The owner-authorised system helper receives only that
already-open descriptor on stdin — never a host pathname — bounds it to 20 GiB,
parses it as ISO9660 or UDF, hashes and atomically stages it privately, then
calls the singleton provisioner. It creates one qcow2 disk, makes a small
guest-tools ISO, and starts the headless singleton. It deliberately does not
open a viewer or accept, log, or retain a product key. Complete Windows Setup
and enter any licence in the explicitly owner-authorised provisioning console.
The singleton domain attaches the copied installer and guest-tools images as
read-only CD-ROMs; its empty disk falls through to the installer on first boot
and takes priority after Windows is installed.

The unprivileged, typed status command is `qos-windows-owner status`; it emits
the appliance phase, licensing state, cached catalogue generation, published
application count, and whether recovery is possible without waking the VM.
Windows Apps invokes the root actions through standard polkit/pkexec. Recovery
first authorises a singleton start and then consumes a root-owned, expiring,
single-use recovery proof before it starts `virt-viewer` as the active user.
The recovery viewer does not receive root or libvirt-management authority.

For the best normal qOS application presentation, use a Windows Server ISO
licensed for the official Remote Desktop Services RemoteApp role. qOS also
accepts a client Windows ISO: the guest setup writes the native Terminal
Services application allow-list used by WinApps-style integrations and tries
the normal FreeRDP RAIL request. This is explicitly best-effort because client
Windows does not promise a RemoteApp host API; qOS never installs RDP Wrapper
or patches `termsrv`. If RAIL is unavailable, qOS attaches an app-focused
whole-guest SPICE/virt-viewer surface after the guest has focused the requested
program; the qOS UI labels this as a whole-surface fallback, never as seamless
windows. The full desktop remains a separate owner-authorised recovery surface.

When the host selects `app_focused_spice`, the authenticated guest bridge
requires an active console user, creates a one-shot interactive Scheduled Task
for the allow-listed executable, and launches `Start-qOSFocusedApp.ps1` in that
user's session. The helper temporarily stops that user's Explorer shell while
the application is running, restores it on exit, and removes its job file and
task. If Windows has no active console user, the bridge returns an explicit
`no managed interactive session` error and qOS does not open an empty desktop.

## Guest setup

In the provisioning console, open the attached `qos-guest-tools.iso`, copy the
example application manifest to a local administrator-controlled location,
replace its entries with the exact approved applications, then run:

```powershell
Set-ExecutionPolicy RemoteSigned -Scope Process
& D:\Install-qOSRemoteApp.ps1 -BootstrapRoot D:\ -ApplicationsPath C:\Admin\qos-published-applications.json
```

The script accepts only existing executables under `C:\Program Files`,
`C:\Program Files (x86)`, or `C:\Windows`; it writes the native RemoteApp
allow-list and keeps each published alias stable. On Server this is consumed
by the official RDS role; on client Windows it is the native but unsupported
compatibility surface used by WinApps-like clients. Discovery does not equate
to publication.

qOS also ships a pinned copy of Bottles' upstream program metadata to improve
catalogue matching. It is read-only reference data: only identities and runner
hints may be imported. qOS never executes a record's Steps, URLs, shell text,
or dependency instructions.

Once the authenticated bridge proves enrollment, qOS automatically detaches
both setup ISOs from the running guest and its persistent libvirt definition
before marking the appliance launchable. The tools image carries the one-time
enrollment key and therefore never remains readable to application users.

## Boundary

The libvirt network has `forward mode='none'`; it is host-only, without a
bridge to a physical NIC or a NAT escape hatch. RDP accepts only its host-side
address. The guest bridge is HTTPS on that network and requires fresh HMAC
headers from an owner-session-readable, host-protected bootstrap key. qOS pins
the guest TLS fingerprint after authenticated enrollment; a change requires
repair rather than an automatic trust update.

The target bridge returns a fresh temporary RemoteApp account and password for
one launch. The account is removed after ten minutes even if the client crashes.
This limits credential reuse; it does not make two Windows programs inside the
singleton VM mutually isolated. Files, clipboard, microphone, and egress are
still granted by qOS's existing workspace broker, not by this guest setup.

The full desktop is available only through the owner-authorised recovery action
for installation or repair. qOS's privileged owner-authorisation backend may
start the domain and attach installation media; it then invokes the viewer as
the active owner session, never as root. It is never a normal launch fallback.
