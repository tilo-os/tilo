# Windows compatibility

Tilo presents Windows programs as applications, not as prefixes, compatibility
tools, or a second desktop, in three execution layers:

1. Wine for ordinary desktop programs.
2. Proton through UMU for games whose catalogue record asks for it.
3. One managed Windows appliance for programs with an explicit VM fallback.

Search, the window's system menu and the Windows Apps manager all show the layer
a program runs on. Moving a program to another layer is an explicit operation
because it changes the program's state and its security boundary.

## What Tilo owns

Tilo owns the policy and presentation around existing engines:

- Wine owns Win32 compatibility and prefixes.
- UMU owns the non-Steam Proton invocation contract, the Steam Runtime, game
  identifiers and protonfix integration.
- Steam owns Proton selection for games installed through Steam.
- libvirt and QEMU/KVM own the appliance lifecycle.
- FreeRDP RemoteApp is the appliance's normal presentation path.
- Tilo's grants, portals, launcher, window chrome, actions and resources remain
  the user-visible security and automation surfaces.

A Tilo-owned Proton capsule launch gets a 12.5% scheduler utilization floor, one
microsecond of timer slack and priority 2 in the ordinary best-effort I/O class.
This lowers frequency-ramp, wake-up and asset-loading latency without realtime
priority or `CAP_SYS_NICE`. Wine productivity applications do not get it, and a
kernel that does not support the hint ignores it.

The compatibility catalogue stores references to upstream identifiers and
pinned runner constraints. It does not copy upstream launchers or add a second
dependency-script language.

## Capsules

A capsule is the user-facing unit behind a Wine or Proton prefix. It records the
runner, architecture, Windows version, components, program entry points,
catalogue provenance, launch overrides and Tilo grants. One installed program
gets one capsule. A suite may share a capsule; unrelated installers are never
put together to save disk space.

The capsule filesystem holds only Windows-owned state and a Tilo exchange
directory, and it launches through the per-workspace confinement:

- `C:` is private to the capsule.
- `Q:` is its exchange directory; user-selected files are imported or published
  into it.
- a linked directory is an ordinary Tilo filesystem grant and takes effect at the
  next launch.
- network, devices, clipboard and collaboration use Tilo grants.

A Wine file picker is not authority to browse the host. A Tilo grant gives the
containing sandbox access; a Windows path alone never does.

### Audio

Each Wine or UMU launch gets its own short-lived PipeWire daemon and Pulse
server. It does not bind the session's `pipewire-0`, `pulse/native` or
`/dev/snd`: the broker creates a virtual output sink that tunnels playback only
to the session's audio server, so ordinary PulseAudio and native PipeWire
clients work and microphones stay invisible.

With the `Device("microphone")` grant the broker adds one virtual source tunnel
to that private graph. It otherwise creates no source and runs no device
discovery. The capsule cannot see the host Pulse/PipeWire socket or the raw
sound device. Revoking a launch kills both proxy processes, which disconnects
any audio handles the program holds.

### Installing and opening software

Windows Apps uses the shared file chooser for **Add Windows app…** and registers
`.exe` and `.msi` open associations. The portal worker opens the selected file
without following its final path component and carries that descriptor with a
display name to the UI. The open descriptor crosses the authenticated
compatibility socket with `SCM_RIGHTS`; no host path, runner, prefix, command or
capability record enters the request.

The service bounds the file at 16 GiB, verifies the PE or MSI signature and
hashes the descriptor's bytes. It then offers the catalogue's recommended
destination and lets the owner choose a private Wine/Proton capsule or the
appliance. The confirmation is short-lived and bound to the same descriptor,
name, size, digest and installer kind; a changed source is rejected. The client
cannot submit a runner, prefix, guest path, command or operation ID.

For a capsule, the service copies the installer into descriptor-anchored state
while hashing again, and only that copy runs. EXE setup is a typed direct
invocation; MSI setup is `msiexec.exe /i C:\tilo-installers\<digest>.msi`.
Unknown desktop software is offered a fresh Wine capsule; Proton stays a
catalogue-selected route.

For the appliance, the service streams the same digest-pinned descriptor over
the certificate-pinned, HMAC-authenticated host-only bridge. Guest staging is
bounded, expiring, one-use and private to the operation. Setup opens in the
app-focused surface, and Windows Apps keeps polling the server-owned operation
if the viewer closes. Bounded discovery returns unpublished candidates and the
owner picks which become Tilo launchers; removing a launcher keeps the guest
install. Vendor uninstall is offered for a typed MSI product code or a narrowly
bound EXE record. MSI always runs the fixed `msiexec /x <product-code>` form. An
EXE record is accepted only when all of these hold:

- it came from HKLM, has a non-empty product-specific `InstallLocation` below
  Program Files or Program Files (x86), and both the application and the
  uninstaller are below that root;
- the root, every directory between, and both files are non-reparse paths owned
  by SYSTEM, Administrators or TrustedInstaller, with no write-capable allow ACE
  for another principal;
- `CommandLineToArgvW` parsing yields one absolute `.exe` plus at most 32
  arguments, each at most 4096 characters and 16384 in aggregate, with no
  control characters;
- the executable path, root, association, argv and SHA-256 are stored
  administrator-only and revalidated immediately before launch and again after
  UAC elevation.

The raw `UninstallString` is neither stored nor run: Tilo quotes the stored argv
into a direct `CreateProcessW` call without a shell. A missing `InstallLocation`,
an unquoted or ambiguous path, a system, temp or user root, a writable or
reparse path, a changed digest or an out-of-bound argv means no vendor-uninstall
action appears.

Setup is an ordinary Tilo window while Windows Apps reports installing, failed
or ready. Discovery is bounded and keeps only application executables under
Program Files or per-user Local AppData, excluding system, helper, updater,
setup and uninstall executables. An EXE with no installed launcher and no
setup-like name is kept as a portable application. A failed or launcher-less
setup stays visible with a retry action on the stored digest.

## Session boundary

The compositor loads the immutable Tilo, pinned UMU and metadata-only Bottles
catalogues and owns the compatibility service. The launcher writes an
authenticated app and workspace preamble to a connected descriptor; the service
corroborates the launcher PID and kernel start time with a single-use,
short-lived registry record the compositor owns. Requests carry only bounded
typed methods. Launch and remove-launcher use opaque server entry IDs; installer
inspection and either confirmed destination are the only methods allowed a
descriptor; retry, vendor uninstall, publication removal and data deletion use
opaque server-owned IDs the session already authorized.

Workspace grants are re-read from the compositor's tree for every operation.
Application query, executable, capsule path, environment, VM fallback intent and
grants never come from the client. Blocking service I/O runs on the Windows Apps
worker thread; the render thread paints a bounded cache and clears it after an
invalid or disconnected response.

The managed-entry registry carries a monotonic compositor generation and is
re-read from capsule state. A running manager starts with the entry snapshot
authenticated at its launch. A VM installer transaction belongs to the
workspace that imported its descriptor: that workspace can resume and publish
its candidates, and another workspace sees neither. Publishing returns the
opaque host entry only to the requesting session, so an app launches without
widening other sessions. Deleting a launcher or capsule removes authority at
once, and an entry another manager created needs a new authenticated launch
snapshot, so a live client cannot widen itself by guessing identifiers.

## Catalogue resolution

Resolution is deterministic and returns an explanation with the launch plan. A
record matches from strongest to weakest identity:

1. exact installer or executable digest;
2. exact store or product identifier, including an UMU identifier;
3. signed publisher plus product metadata;
4. normalized executable name, as a low-confidence hint.

Local overrides beat vendor and community catalogues but stay narrow: an
installed runner and safe runtime options, never arbitrary host paths, preload
libraries, environment injection or commands.

Unknown desktop software defaults to a fresh Wine capsule. Unknown games do not
silently use Proton, and no unknown program enters the appliance. An appliance
fallback is explicit in a trusted record or chosen by the person after a failed
local launch, and it returns one deterministic launch plan.

## The appliance

There is exactly one appliance, a compatibility fallback and not an alternative
session. Normal operation is headless:

- a guest service publishes an allowlisted inventory of installed programs;
- Tilo makes program shortcuts from that inventory;
- the session uses a labelled, app-focused SPICE surface and suppresses Explorer
  while the requested program runs;
- the FreeRDP RAIL path is used when Tilo has a RAIL-capable local surface and
  the guest supports it;
- the Windows desktop is not an application route.

If publication or the guest service is unavailable, launch fails closed with
repair and recovery actions. A full console exists only as an owner-only
recovery action, marked in Tilo chrome as unconfined guest administration.

The appliance uses UEFI Secure Boot, a software TPM 2.0, KVM acceleration,
VirtIO storage and networking, memory ballooning, discard and host CPU
passthrough where safe. Its disk and TPM state live in one protected directory.
Provisioning needs the person's Windows installation media and licence; Tilo
does not redistribute Windows.

## Appliance capabilities

The VM boundary does not bypass workspace policy. The default device model has
no shared host filesystem, host clipboard, camera, microphone, USB forwarding or
bridged network.

A per-program launch gets short-lived capability leases from the Tilo broker.
The presentation and file-transfer channel exposes only the leased resources,
and every lease is revoked when the RemoteApp session ends. The shipped network
is host-only with forwarding off: no NAT and no physical network. Native RAIL
can turn a host-authorized `NetHost` request into a short-lived host-only
CONNECT relay and a guest-local proxy inherited by the RemoteApp wrapper's
process tree. The relay reuses Tilo's NetHost matcher and its protection against
rebinding after DNS, and dropping the lease closes the listener instead of
attaching an egress NIC. These are host-side lease controls, not per-process
isolation: every program in the one persistent Windows session sees the union of
active redirects and authorized requests. Clipboard is off unless the workspace
grant allows it, and its state shows in window chrome.

A guest compromise is possible, so the appliance is one shared security domain
whose programs can influence each other. The UI says so; programs that need
mutual isolation do not fit the appliance.

## Windows Apps

Windows Apps lists programs and retained setup transactions, explains the
selected route, shows capsule identity without host paths, displays resolved
access and disabled reasons, and launches opaque entries. **Add Windows app…**
shows progress while the service runs setup. A failed discovery stays visible as
a retained installation with Retry setup and Delete app & data.

The lifecycle actions say what they remove:

- **Remove launcher** unregisters one Tilo entry and keeps the Windows install
  and its data.
- **Run uninstaller** runs only the typed MSI product code or the digest-bound,
  Program Files-rooted EXE argv contract of that application, then keeps the
  leftovers for inspection or deletion.
- **Delete app & data** retires and recursively deletes the private capsule,
  after a second click.

For each program Windows Apps shows the layer, capsule or appliance identity,
catalogue explanation, runner and prefix or guest-storage identity, the
effective host access, the shared-appliance boundary, and the publish,
remove-launcher, vendor-uninstall and private-data actions.

The fixture catalogue exists only in the headless screenshot path; a missing
service shows a repair state.

## Presence in the system

Every operation is a Tilo action on a resource, so assistive technology,
automation, collaborators and tests use the same surface as the window.

The image ships one pinned Wine runner and one pinned UMU/Proton runtime, so a
fresh installation works offline after it acquires applications. Other runners
are verified, content-addressed objects, activated atomically; a capsule keeps
its previous runner until a migration succeeds. Catalogue updates are versioned
and authenticated, and a bad update rolls back without touching program state. A
capsule snapshot holds prefix state and configuration; an appliance snapshot
holds its disk and TPM-consistent state, never host exchange files.

## Non-goals

- Reimplementing Wine, Bottles, winetricks, protonfixes, libvirt or FreeRDP.
- Treating a filename as authority for a silent VM fallback.
- Showing a nested Windows desktop during ordinary use.
- Claiming a VM program is isolated from other programs in the same VM.
- Giving a Windows process host access because it asked for a drive.
