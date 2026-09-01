# Windows compatibility: design target and status

> **Status (2026-09): live implementation with explicit follow-up work.** The catalogue,
> deterministic route resolver, capsule broker, authenticated compositor
> session service, per-launch audio proxy, portal installer intake, and Windows
> Apps lifecycle client are wired. Wine/UMU programs can be installed, listed,
> launched, retried, unregistered, vendor-uninstalled, and deleted. The
> singleton VM delegate, pinned HTTPS guest bridge, app-focused SPICE fallback,
> and managed guest install/publish/uninstall lifecycle are composed by the
> session when the owner has provisioned Windows.
> Route-policy mutation, snapshots, migration/reset, and automatic
> failure-triggered fallback ordering remain unavailable and are shown as such.

qOS is intended to present Windows programs as applications, not as prefixes,
compatibility tools, or a second desktop. The target has three execution layers:

1. Wine for ordinary desktop programs.
2. Proton through UMU for games whose catalogue record asks for it.
3. One managed Windows appliance for programs with an explicit VM fallback.

The selected layer is always visible in search, the window's system menu, and
the Windows Apps manager.  Moving a program to another layer is an explicit
operation because it changes the program's state and security boundary.

## Do not build another compatibility ecosystem

qOS owns the policy and presentation around existing engines:

- Wine owns Win32 compatibility and prefixes.
- UMU owns the supported non-Steam Proton invocation contract, Steam Runtime,
  game identifiers, and protonfix integration.
- Steam owns Proton selection for games installed through Steam.
- libvirt and QEMU/KVM own the appliance lifecycle.
- FreeRDP RemoteApp is the normal appliance presentation path.
- qOS's existing workspace grants, portals, launcher, window chrome, actions,
  and resources remain the user-visible security and automation surfaces.

The compatibility catalogue stores references to upstream identifiers and
pinned runner constraints. It does not copy upstream launchers or invent a
second dependency-script language.

## Capsules

A capsule is the user-facing unit corresponding to a Wine or Proton prefix. It
records the exact runner, architecture, Windows version, component set,
program entry points, catalogue provenance, launch overrides, and qOS grants.
One installed program gets one capsule by default. A suite may deliberately
share a capsule, but unrelated installers are never placed together merely to
save disk space.

The capsule filesystem contains only Windows-owned state and a qOS exchange
directory. It is launched through the existing per-workspace confinement:

- `C:` is private to the capsule.
- `Q:` maps to its exchange directory.
- user-selected files are imported or published into that directory.
- a linked directory is an ordinary qOS filesystem grant and takes effect on
  the next launch when a new mount bind is required.
- network, devices, clipboard, and collaboration use existing qOS grants.

Wine file pickers do not constitute authority to browse the host. qOS grants
the containing sandbox access; a Windows path alone never does.

### Audio (implemented)

Each Wine/UMU launch receives its own short-lived PipeWire daemon and Pulse
server. It is not a bind of the session's `pipewire-0`, `pulse/native`, or
`/dev/snd`: the broker creates a virtual output sink which tunnels only
playback to the qOS session audio server. This makes ordinary Wine PulseAudio
and native PipeWire clients work without making microphones visible.

When the ordinary qOS `Device("microphone")` grant is present, the broker adds
one virtual source tunnel to that same private graph. It otherwise creates no
source and runs no WirePlumber/device-discovery process. The capsule still
cannot see the host Pulse/PipeWire socket or the raw sound device. Revoking a
launch kills both proxy processes, disconnecting any audio handles the Windows
program still holds. The Windows Apps manager reports playback as available
and microphone state through the standard qOS grants list.

### Installing and opening user software

Windows Apps uses the shared qOS/XDG file chooser for **Add Windows app…** and
registers `.exe` and `.msi` open associations. The portal worker immediately
opens the selected regular file without following its final path component,
then carries that descriptor and a separate display name to the UI. The
already-open descriptor crosses the authenticated compatibility socket with
`SCM_RIGHTS`; no host path, runner, prefix, command string, or capability
record enters the request.

The service bounds the file at 16 GiB, verifies the PE or MSI signature, and
hashes the exact descriptor bytes. It then presents the catalogue's recommended
destination and lets the owner explicitly choose a private Wine/Proton capsule
or the one Windows appliance. The confirmation is short-lived and bound to the
same open descriptor, display name, size, digest, and installer kind; changing
or replacing the source is rejected. The client cannot submit a runner,
prefix, guest path, command, or operation ID.

For a capsule, the service copies the installer into descriptor-anchored state
while hashing again. Only that managed copy can run. EXE setup is a typed
direct invocation; MSI setup is the fixed
`msiexec.exe /i C:\qos-installers\<digest>.msi` argv shape. Unknown desktop
software is recommended a fresh Wine capsule, while Proton remains a
catalogue-selected route.

For the appliance, the service streams the same digest-pinned descriptor over
the certificate-pinned, HMAC-authenticated host-only bridge. Guest staging is
bounded, expiring, one-use, and private to the operation. Setup opens in the
app-focused surface, and Windows Apps continues polling the server-owned
operation if the viewer closes. Bounded Program Files discovery returns
unpublished candidates; the owner chooses which become qOS launchers. Removing
a launcher keeps the guest install. Vendor uninstall is offered for either a
typed MSI product code or a narrowly bound EXE record. MSI always executes the
fixed `msiexec /x <product-code>` form. An EXE record is accepted only when all
of these discovery and execution checks hold:

- the record came from HKLM, has a non-empty product-specific
  `InstallLocation` below Program Files or Program Files (x86), and both the
  discovered application and uninstaller are below that same root;
- the root, every intervening directory, and both files are non-reparse paths,
  owned by SYSTEM, Administrators, or TrustedInstaller, with no write-capable
  allow ACE for another principal;
- native `CommandLineToArgvW` parsing yields one absolute `.exe` plus at most
  32 arguments, each at most 4,096 characters and at most 16,384 characters in
  aggregate, with no control characters; and
- the exact executable path, root, application association, parsed argv, and
  SHA-256 are persisted administrator-only and all revalidated immediately
  before launch and again after UAC elevation when elevation is required.

The raw `UninstallString` is neither stored nor run. qOS quotes the persisted
argv back into a direct `CreateProcessW` call without a shell. Missing
`InstallLocation`, unquoted/ambiguous executable paths, system/temp/user roots,
writable or reparse paths, digest changes, and out-of-bound argv all mean that
no vendor-uninstall action is exposed.

Setup remains interactive as an ordinary qOS window while Windows Apps stays
responsive and reports installing/failed/ready state. Discovery is bounded and
keeps only application executables under Program Files or per-user Local
AppData, excluding system, helper, updater, setup, and uninstall executables.
An EXE with no installed launcher and no setup-like name is retained as a
portable application. Failed or launcher-less setup remains visible with a
retry action bound to the retained digest.

## Current session boundary

The compositor loads the immutable qOS, pinned UMU, and metadata-only Bottles
catalogues and owns the compatibility service. The launcher writes an
authenticated app/workspace preamble to a connected inherited descriptor. The
service corroborates the launcher PID and kernel start time with a
compositor-owned, single-use, short-lived registry record. Requests contain
only bounded typed methods. Launch and remove-launcher use opaque server entry
IDs. Installer inspection and either confirmed destination are the only
methods allowed one descriptor. Retry, vendor uninstall, publication removal,
and data deletion use opaque server-owned IDs already authorized for the
session.

Workspace grants are re-read from the canonical compositor tree for every
operation. Application query, executable, capsule path, environment, VM
fallback intent, and grants never arrive from the client. Blocking service I/O
runs on the Windows Apps worker thread; the render thread paints a bounded
cache and clears it after an invalid or disconnected response.

The managed-entry registry has a monotonic compositor generation and is
re-read from capsule state. A running manager starts with the exact entry
snapshot authenticated at its launch. A VM installer transaction belongs to
the authenticated workspace that imported its descriptor: reconnecting that
workspace can resume and publish its candidates, while another workspace sees
neither status nor candidates. Publishing returns the resulting opaque host
entry only to that workspace's requesting session, so it can launch the app
immediately without widening other live sessions. Launcher or capsule deletion
removes authority immediately. Entries created by another manager still require
a new authenticated launch snapshot, so an existing client cannot widen itself
by guessing identifiers.

## Explainable catalogue resolution (implemented core; fallback ordering is roadmap)

Resolution is deterministic and returns an explanation with the launch plan.
Records match from strongest to weakest identity:

1. exact installer or executable digest;
2. exact store/product identifier, including an UMU identifier;
3. signed publisher plus product metadata;
4. normalized executable name as a low-confidence hint.

Local user overrides beat vendor and community catalogues but remain narrow:
they may select an installed runner and safe runtime options, not arbitrary
host paths, preload libraries, environment injection, or commands.

Unknown desktop software defaults to a fresh Wine capsule. Unknown games do
not silently use Proton, and no unknown program silently enters the appliance.
An appliance fallback must be explicit in a trusted record or chosen by the
user after a failed local launch. The resolver currently returns one
deterministic launch plan. A broker-managed ordered fallback plan, with an
explicit decision recorded before advancing, is a target behavior rather than
an automatic live flow.

## The Windows appliance

There is exactly one appliance on the qOS system. It is a compatibility
fallback, not an alternative session.

Normal operation is headless:

- a guest service publishes an allowlisted inventory of installed programs;
- qOS creates program shortcuts from that inventory;
- the current Wayland session uses a clearly labelled, app-focused SPICE
  surface and suppresses Explorer while the requested program runs;
- the typed FreeRDP RAIL path is used only when qOS has a real RAIL-capable
  local surface and the guest supports it;
- the Windows desktop is not a normal application route.

If RemoteApp publication or the guest service is unavailable, launch fails
closed with repair and recovery actions. It does not reveal a desktop. A full
console exists only as an explicit owner-only recovery action and is marked as
unconfined guest administration in qOS chrome.

The appliance uses UEFI Secure Boot, a software TPM 2.0, KVM acceleration,
VirtIO storage and networking, memory ballooning, discard, and host CPU
passthrough where safe. Its qcow2 disk and TPM state live in one protected
state directory. Provisioning requires user-supplied Windows installation
media and licensing; qOS does not redistribute Windows.

## Appliance capabilities

The VM boundary does not bypass workspace policy. Its default device model has
no shared host filesystem, host clipboard, camera, microphone, USB forwarding,
or unrestricted bridged network.

Per-program launches receive short-lived capability leases from the qOS
broker. The presentation/file-transfer channel exposes only the resources in
the lease, and all leases are revoked when the RemoteApp session ends.
The shipped appliance network is host-only with forwarding disabled; it has no
NAT or physical-network access by default. Any future egress must be a separate
brokered capability lease and remain visible in the existing qOS policy
surface. Clipboard is off unless the workspace grant permits it and its enabled
state is visible in window chrome.

Guest compromise remains possible, so the appliance is treated as one shared
security domain: programs inside it can potentially influence each other. The
UI says this plainly. Programs requiring mutually hostile isolation are not a
fit for the singleton appliance.

## Management experience

Windows Apps lists programs and retained setup transactions, explains the
selected route, shows capsule identity without exposing host paths, displays
resolved access and disabled reasons, and launches opaque entries. **Add
Windows app…** uses the standard portal and shows immediate progress while the
service performs blocking setup. Failed discovery is not hidden: it appears as
a retained installation with Retry setup and Delete app & data.

Lifecycle actions deliberately say what they remove:

- **Remove launcher** unregisters one qOS entry and keeps the Windows install
  and all private data.
- **Run uninstaller** executes only the typed MSI product code or the
  digest-bound, Program Files-rooted EXE/argv contract discovered for that
  application, then retains leftovers for inspection or deletion.
- **Delete app & data** atomically retires and recursively deletes the whole
  private capsule. It requires a second explicit click.

The fixture catalogue exists only in the headless screenshot path; a missing
live service shows an unavailable/repair state. Implemented management
information includes:

- current layer, capsule/appliance identity, catalogue explanation, runner, and
  prefix or guest-storage identity;
- effective host access and the shared-appliance security boundary;
- separate publish, remove-launcher, vendor-uninstall, and private-data actions.

Remaining management targets are snapshots, duplicate/export/reset, runner
migration, explicit persistent route override/reset, active-process inspection,
and richer links into the existing qOS grant controls.

Route-policy and remaining mutation controls stay disabled until they have a
trusted action handoff. VM launch appears only when the provisioned singleton,
owner-readable bridge authority, and authenticated guest are ready.
Wine/Proton audio is available through the brokered,
playback-only PipeWire/Pulse graph described above; microphone capture remains
subject to the existing qOS microphone grant.

The eventual operations should exist as qOS actions/resources. Accessibility tools,
automation, collaborators, and tests therefore use the same semantic surface
as the visual UI.

## Updates and rollback (design target)

The image ships one pinned Wine runner and one pinned UMU/Proton runtime so a
fresh installation works offline after initial application acquisition.
Additional runners are verified, content-addressed objects. Activation is
atomic, and a capsule continues to reference its previous runner until a
migration succeeds. Catalogue updates are versioned and authenticated; a bad
update can be rolled back independently of program state.

Capsule snapshots include prefix state and configuration. Appliance snapshots
include its disk and TPM-consistent state, but never host exchange files.

## Non-goals

- Reimplementing Wine, Bottles, winetricks, protonfixes, libvirt, or FreeRDP.
- Treating a filename as sufficient authority for a silent VM fallback.
- Showing a nested Windows desktop during ordinary use.
- Claiming that a VM program is isolated from other programs in the same VM.
- Giving a Windows process host access merely because it requested a drive.
