# Windows compatibility

Tilo presents Windows programs as applications, not as prefixes, compatibility
tools, or a second desktop, in three execution layers:

1. Wine for ordinary desktop programs.
2. Proton for games whose catalogue record asks for it, and for any program an
   administrator chose to open with it.
3. One managed Windows appliance for programs with an explicit VM fallback.

Search, the window's system menu and the Windows Apps manager all show the layer
a program runs on. Moving a program to another layer is an explicit operation
because it changes the program's state and its security boundary.

## What Tilo owns

Tilo owns the policy and presentation around existing engines:

- Wine owns Win32 compatibility and prefixes.
- Proton's own script owns bringing a prefix up to its version and applying
  protonfixes, keyed by the UMU database's game identifiers. A capsule starts
  that script directly on the system's libraries; Proton is offered on x86
  computers alone (see "Opening a program with Proton").
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

Each Wine or Proton launch gets its own short-lived PipeWire daemon and Pulse
server. It does not bind the session's `pipewire-0`, `pulse/native` or
`/dev/snd`: the broker creates a virtual output sink that tunnels playback only
to the session's audio server, so ordinary PulseAudio and native PipeWire
clients work and microphones stay invisible.

With the `Device("microphone")` grant the broker adds one virtual source tunnel
to that private graph. It otherwise creates no source and runs no device
discovery. The capsule cannot see the host Pulse/PipeWire socket or the raw
sound device.

The service prepares the graph (its sockets and configuration, which decide
the microphone) and the launch starts its two daemons right before the
sandbox, as children of the sandbox's own process (`tilo-launcher
windows-audio`). They are in the program's launch group and live exactly as
long as the program: they stay while the desktop restarts in place, and they
end when the program ends, is quit, or is ended with its group at logout,
which disconnects any audio handles the program holds.

### Network

A program the workspace grants specific network destinations gets a private proxy, in its
launch's own process tree: `tilo-launcher windows-network` starts it right before the sandbox
and becomes the sandbox, so the proxy lives exactly as long as the program. It stays while the
desktop restarts in place, and it ends when the program ends, is quit, or is ended with its
group at logout. The sandbox's network namespace stays empty; the proxy's socket, bound into
it, is its only route out.

The desktop prepares the policy from the workspace's grants and updates it while the program
lives, through a control socket in the program's keeper, which the desktop that adopts the
program after a restart finds again. Taking a grant back cuts the connections it allowed. With
no desktop up the proxy keeps enforcing the last policy; a proxy that dies leaves the program
offline, never unfiltered.

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
Unknown desktop software is offered a fresh Wine capsule; Proton is never
guessed for it.

### Opening a program with Proton

An administrator of the computer (a member of `wheel`) can open any program of
a private capsule with Proton instead of Wine, or back, in the program's "Open
with" choice in Windows Apps. The choice is per program: it is the shared
`routes` field of the Windows Apps resource, which the manager carries to the
service as the `SetOpensWith` request. The service checks the desktop's own
account, never the client's word, refuses everyone else with the
reason, and saves the choice with the program in its capsule's state. The
program's next start starts the pinned UMU-Proton with the fixes for any
program (`umu-default`), so no game's fixes apply to it.

The program keeps its files: Proton opens the same prefix its Wine setup
filled. `STEAM_COMPAT_DATA_PATH` is the prefix itself, which gets the layout
Proton expects before each start (`pfx` naming the prefix, a `steamuser`
profile linked to the capsule's own). Proton takes a prefix without its version
stamp as one to bring up to date rather than replace: it keeps the registry and
the installed programs and writes its version. Tilo takes the prefix's `Z:`
drive away before every start, and Proton's script adds it back for `/`, which
inside the capsule is only what its sandbox shows. Back to Wine, Wine opens
the prefix as Proton left it. Proton opens only 64-bit prefixes, so the choice
is refused for a 32-bit one and the program keeps opening with Wine. Other programs of the same capsule keep opening as
they did, but never at the same time as one on the other runner: Wine and
Proton cannot share a live prefix, so a start is refused with the reason while
a program of the setup is open on the other one.

Proton opens only on x86 computers. An ARM computer translates Windows programs
with FEX; Proton's own program files are x86, and the one wiring that gives
them the translator is the Steam Runtime's own container nested inside the
capsule, which a capsule's sandbox refuses. Windows Apps therefore refuses a
Proton start on an ARM computer and says why, naming that the program opens
with Wine here. Choosing Proton on such a computer is kept, and the refusal
comes when the program is started; Wine on ARM is unaffected. The ARM Windows
extension therefore carries FEX, the x86 guest root and the Windows VM media,
and no Proton; an x86 extension carries Proton and no Steam Runtime.

In a capsule Proton uses Wine's own synchronization: its esync and fsync keep
shared memory in `/dev/shm`, which a capsule cannot write, and this Proton's
Wine has no ntsync. Proton's script reads two kernel limits
(`/proc/sys/fs/file-max`, `/proc/sys/vm/max_map_count`), the only files of
`/proc` the capsule lets it read. Every library Proton loads comes from the
image or the Windows extension, which `os/dev/proton_libraries.py` checks when
the extension is built.

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
or ready. Discovery is bounded and keeps what the Start menu opens: each
shortcut under a Start Menu folder whose target is an application executable
under Program Files or per-user Local AppData, named as its shortcut shows, with
system, helper, updater, setup and uninstall executables left out. An MSI's
advertised shortcut carries no path of its own and names its program by its own
name. A program its installer gives no shortcut — an updater or a console tool
it keeps for itself — is not listed. An EXE with no installed launcher and no
setup-like name is kept as a portable application. A failed or launcher-less
setup stays visible with a retry action on the stored digest.

## Session boundary

The desktop session serves the compatibility service as one of its own
services (`compat` in its service table), on threads of its own beside the
session's loop. It loads the immutable Tilo, pinned UMU and metadata-only
Bottles catalogues; a computer without them, or without the runtime payloads,
still answers the manager, and a launch says that Windows support is not
installed.

The programs, the appliance and the layer are the device's `windows-apps`
resource, which the session's system service serves beside `windows-compat`:
an app reads what this computer has and calls its actions through the device's
daemon like any other device resource, and nothing of the manager is a second
channel. Workspace grants are re-read from the workspace's sandbox in the
session's layout for every operation, so a program opens under the sandbox of
the workspace the calling app is in.

Application query, executable, capsule path, environment, VM fallback intent
and grants never come from an app. Launch and remove-launcher use opaque server
entry IDs; installer inspection and either confirmed destination are the only
calls given a file, which the session opens itself from the path the person
chose; retry, vendor uninstall, publication removal and data deletion use
opaque server-owned IDs. Blocking service I/O happens on the system service's
worker thread; a view paints a bounded cache and clears it after an invalid or
disconnected response.

A launch is answered once its program started, never held for its lifetime.
The service watches each program it started: a list says which entries have
a program open in the manager's workspace, and reports each program that ended
since that connection last listed, with its exit status or why it never
opened, so a manager opened later still hears it. **Quit app** ends a capsule
program at once; the appliance's programs close inside Windows.

A program is given its display when it starts, not when the service starts:
the session answers with the Wayland socket its compositor bound and the
render node it draws on, and refuses the launch while it has neither. The
appliance's rail and SPICE presentations are given the same display.

A capsule program stays open while the desktop restarts in place to apply an
update. The service plans its sandbox for a display of its own, an app keeper's
(`tilo-app-keeper`), and asks the session to start it as one of its own
launches: `tilo-launcher windows-program` joins the launch's own scope, starts
the keeper there and the sandbox as the keeper's child, with the descriptors
the plan hands Bubblewrap passed down unchanged. Memory tiers, a force quit, a
crash notice and a logout treat it as any other app. The desktop that follows
the restart takes the keeper back, the program's windows return to their tabs,
and its service lists the program open again and can quit it; a program that
ended while no desktop was there is reported with its exit status and the last
line or two it wrote, and its tab closes rather than starting it again. A
program that fails says those last lines in the manager beside its status:
they went to its keeper's log, not to the service.

A Wine program shows on the Wayland socket the keeper binds. A Proton program
shows on an X display of the keeper's own: UMU-Proton carries no Wayland driver,
so it opens its window through Xwayland, and the keeper gives its sandbox that
display's socket and authority file. Both stay open across an in-place restart;
each is shut down with the desktop only when the login ends.

An X11 window tells its size limits where Wine's Wayland driver does not, but a
size is no mark of a dialog: a window that names a parent floats as a dialog of
it, and a program's own top-level window takes a tab whatever size it keeps. A
Proton program's fixed-size settings window (PuTTY's, for one) therefore takes
a tab of its own, as the same window does under Wine; a tab never narrows below
the smallest width its window keeps, so the strip scrolls rather than cutting
it off. Proton
loads Vulkan only for a program that draws with Direct3D: PuTTY draws with GDI,
and Proton said nothing about Vulkan for it.

A program shown from the Windows appliance stays open the same way: its RAIL or
SPICE presentation starts behind a keeper of its own
(`docs/windows-presentation.md`, "Through a desktop restart"). The manager
lists it open again after the restart and, as before it, offers no Quit: it
closes inside Windows. Each program shown that way ends when the appliance
itself restarts: the manager lists it closed once Windows is back.

A saved tab reopens its program after a reboot or a logout, when nothing kept
it running. The session keeps the window class each program showed
under, with the entry it belongs to and the workspace whose tab it stood in,
beside the capsules (`tilo/windows-compat/window-identities.json`). At the next
start it asks the compatibility service, as the session itself rather than
through a manager's handoff, to open that entry in the tab's workspace; the
service allows it only for an entry it can open now and a workspace this session
serves, and plans it exactly as a manager's launch. A program removed meanwhile
leaves its tab saying the app is not installed, and a window class seen only in
another workspace opens nothing there.

The entries a manager may address are read from capsule state and the
appliance's published applications when it connects; it keeps that set until
it ends. A VM installer transaction belongs to the workspace that imported its
descriptor: that workspace can resume and publish its candidates, and another
workspace sees neither. Publishing returns the opaque entry only to the
requesting manager, so an app launches without widening other managers.
Deleting a launcher or capsule removes authority at once, and an entry another
manager created needs a new authenticated launch, so a live client cannot
widen itself by guessing identifiers.

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
process tree. The server reuses Tilo's NetHost matcher and its protection against
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
automation, members and tests use the same surface as the window.

An x86 image ships one pinned Wine runner and one pinned UMU/Proton runtime, so a
fresh installation works offline after it acquires applications; an ARM image
ships Wine through its pinned FEX and x86 guest root, and no Proton. Other runners
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
