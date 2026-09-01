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
normal qOS file portal. Its portal worker immediately opens the selected
regular file without following its final path component. The owner-authorised
system helper receives only that already-open descriptor on stdin — never a
host pathname — bounds it to 20 GiB,
parses it as ISO9660 or UDF, hashes and atomically stages it privately, then
calls the singleton provisioner. It creates one qcow2 disk, makes a small
guest-tools ISO, and starts the headless singleton. It deliberately does not
open a viewer or accept, log, or retain a product key. Complete Windows Setup
and enter any licence in the explicitly owner-authorised provisioning console.
The singleton domain attaches the copied installer and guest-tools images as
read-only CD-ROMs on a Windows-inbox SATA controller, plus the packaged
`/usr/share/virtio-win/virtio-win.iso` driver image needed for stock Windows
Setup to see the virtio disk and NIC. qOS
does not download or redistribute that image; it uses the host distribution's
audited `virtio-win` package and retains its license notices. The empty disk
falls through to the installer on first boot and takes priority after Windows
is installed. The installed system disk itself remains on virtio-scsi for
normal performance.

The unprivileged, typed status command is `qos-windows-owner status`; it emits
the appliance phase, licensing state, cached catalogue generation, published
application count, and whether recovery is possible without waking the VM.
Windows Apps invokes the root actions through standard polkit/pkexec. Recovery
first authorises a singleton start and then consumes a root-owned, expiring,
single-use recovery proof before it starts `virt-viewer` as the active user.
The recovery viewer does not receive root or libvirt-management authority.

### Guided owner flow

Windows Apps is the one place qOS manages the appliance. It guides the owner
through the following sequence instead of asking them to assemble a VM:

1. **Try the smallest route first.** A portal-selected `.exe` or `.msi` is
   inspected and normally gets a new private Wine capsule (or the
   catalogue-selected Proton route). qOS keeps the exact validated installer
   copy for a failed setup, so a later escalation does not ask the owner to
   find the file again.
2. **Choose Windows only deliberately.** When the catalogue recommends the
   appliance, or a retained Wine/Proton setup needs attention and offers its
   explicit VM escalation, Windows Apps shows the shared-domain consequence
   and asks for a local confirmation. The action reopens only qOS's retained,
   digest-pinned installer copy; it neither asks for a second file nor accepts
   a host/guest path. It never silently moves an application into Windows.
3. **Select official media with the normal qOS file portal.** qOS validates
   and stages the already-open ISO descriptor, creates the singleton
   appliance, then opens the owner-authorised setup desktop. The owner accepts
   Microsoft's terms, signs in if wanted, and supplies an authorised product
   key, digital entitlement, or organisation activation method. qOS never
   receives, retains, or reports a product key.
4. **Return to Windows Apps.** The management card reports setup phase,
   authenticated guest integration, and the guest's factual activation state;
   it uses wording such as “Windows reports that it is not activated”, never
   “licensed” or “unlicensed by qOS”. This information is deliberately absent
   from normal published-app launching and window chrome.
5. **Install, discover, publish.** The same descriptor-bound installation
   opens in the focused guest surface. qOS lists bounded discovered candidates;
   the owner adds the intended programs to Windows Apps. Thereafter each
   launcher opens only that application, with Explorer suppressed.

The management card is also the home for standard, owner-authorised lifecycle
work such as updates, restart, suspend, and recovery. Those commands must use
the narrow polkit actions; a published application cannot restart the shared
VM or gain a desktop merely because it is running.

Every published Windows application runs under the appliance's one persistent
managed Windows user/session. qOS does not claim that clipboard, redirected
files, microphones, devices, or network access can be isolated between two
concurrent guest applications. Until a capability broker has materialised a
redirect, the focused-SPICE fallback exposes none of those resources. Windows
Apps labels this as **Shared Windows session access**; normal Wine and Proton
capsules remain separately confined.

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

The setup script finds `virtio-win-guest-tools.exe` on the separately attached
Fedora `virtio-win` CD, requires its SHA-256 to match the installer from the
host's digest-pinned RPM payload, requires a Windows-valid Authenticode
publisher signature, runs the visible passive installer, and verifies the
`vdservice` SPICE agent. This
provides the paravirtual display driver, automatic SPICE resize, and cursor
integration without a network download. `-SkipSpiceGuestTools` is for guided
repair only and leaves those integrations unavailable.

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
all setup media, including the driver ISO, from the running guest and its persistent libvirt definition
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
one launch when qOS has selected a native RAIL surface. The account is removed
after ten minutes even if the client crashes. This limits credential reuse; it
does not make two Windows programs inside the singleton VM mutually isolated.

The app-focused SPICE fallback is playback-only: files, clipboard, microphone,
and egress do not reach the guest. On a probed native RAIL surface, qOS
materialises the canonical workspace capability lease before preparing the
guest launch. A workspace grant by itself never makes a host resource visible
to the shared VM.

Filtered network access is a real `NetHost` lease, not a temporary NAT NIC.
The persistent VM NIC remains on the `forward mode="none"` host-only network.
For an allowed launch qOS starts a token-authenticated CONNECT relay bound only
to `192.168.251.1`; the guest's fixed RemoteApp wrapper receives only its
authenticated operation ID and reads the one-use bridge job. The authenticated
SYSTEM bridge owns one shared loopback proxy; each wrapper joins its
mutex-protected WinINet lease counter and gives `HTTP_PROXY`/`HTTPS_PROXY` only
to that target process tree.
The relay uses the same qOS name matcher and post-DNS device-local rejection as
the normal launcher gate. It resolves only after checking the requested name,
so a name grant cannot become an IP-literal bypass or a route to host-local
services. It supports TCP CONNECT only; no SOCKS, UDP, bridge, or guest NAT is
introduced. Ending a RAIL process deletes its guest job; the final active
wrapper restores the prior WinINet state, while one app exiting cannot cut off
another. Ending the host lease drops its policy contribution even if the guest
crashes. The first/last wrapper transition snapshots, applies, refreshes, and
restores the managed user's WinINet loopback proxy setting, improving ordinary
desktop-app coverage without changing machine-wide WinHTTP state. Applications which
deliberately bypass both standard proxy settings remain offline rather than
acquiring an unrestricted route.

Windows remains one shared security domain. While a lease is active, other
processes in the managed Windows session might observe its loopback proxy/job
and should be treated as able to use the effective union. qOS derives and shows
that union and its contributing published applications from materialized leases;
it does not claim per-application VM isolation.

The full desktop is available only through the owner-authorised recovery action
for installation or repair. qOS's privileged owner-authorisation backend may
start the domain and attach installation media; it then invokes the viewer as
the active owner session, never as root. It is never a normal launch fallback.

## Appliance management

The Windows Apps manager talks to a typed lifecycle service; it does not call
libvirt or `virsh` directly. The service owns exactly one domain,
`qos-windows-appliance`, and keeps its UUID, SMBIOS serial/UUID, fixed host-only
MAC, disk serial, secure-boot NVRAM and virtual TPM state stable. Keeping these
identities stable is important for Windows activation and avoids turning a
reboot into a new device. The service refuses a same-named foreign domain.

Manager status is passive: it reads the qOS state document and libvirt domain
state without waking Windows. It reports setup, activation state, guest
readiness, managed-session readiness, and domain state only in the management
UI. An unactivated guest remains usable for setup/testing, but qOS never calls
it licensed and never stores a key.

Start, graceful shutdown, restart, suspend, resume and forced shutdown are
typed actions. They are idempotent where safe: starting an already-running
guest, stopping an already-stopped guest, or closing an expired lease succeeds.
Forced shutdown is a separately labelled recovery action and is never an
automatic fallback for a graceful timeout. Paused, transitioning, crashed or
foreign states are surfaced as recovery-required rather than guessed around.

Activation and Windows Update use the `egress-open` management action. It
requires administrator authentication, takes a purpose (`activation` or
`windows-update`) and a bounded lifetime, and attaches a second virtio NIC only
while the lease is live. The persistent domain has no egress NIC: the default
host-only network remains `forward mode="none"`. The NAT network is defined but
inactive and non-autostart, and the live-only NIC is removed on close, expiry,
shutdown or restart. If qOS or the guest crashes, the lease cannot survive a
domain restart. Root-owned lease records are written before attachment and a
systemd reaper checks them every 30 seconds, so a controller/UI crash also
closes expired live NICs. A user can therefore select **Allow temporarily for
activation/updates**, complete the normal Windows flow inside the managed
console, and then revoke it immediately; qOS never implements KMS, activation
bypass, or a shared key.

The managed Windows experience uses one persistent human Windows session for
the appliance. qOS does not create a new Windows account per application or
pretend that applications are mutually isolated VMs. Native RAIL launches use
the same managed account/session, while elevation remains Windows UAC. If the
console session is unavailable, the app-focused SPICE fallback reports that
condition and does not show an empty desktop. Restarting Windows may close
applications; qOS reports that outcome and lets the user retry the app after
the guest bridge and catalogue become healthy.

Safe recovery is deliberately guided: retry a failed bridge or update first,
offer graceful restart/shutdown with progress, then expose forced shutdown,
repair and the full recovery desktop only after explicit administrator
authentication. Setup media is detached after authenticated guest enrollment;
re-running preparation never resets the disk or silently creates a clone.
