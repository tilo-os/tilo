# Windows compatibility: design target and status

> **Status (2026-09): partial implementation.** The catalogue, deterministic
> route resolver, metadata adapters, capsule/broker core, VM boundary, and
> provisioning/guest-payload scaffolding exist. The Windows Apps UI still uses
> an empty live provider until a session daemon is wired to the broker, so the
> end-to-end launch, RemoteApp publication, and catalogue update flows below
> are design targets unless explicitly marked current.

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

## The Windows appliance (design target)

There is exactly one appliance per qOS user. It is a compatibility fallback,
not an alternative session.

Normal operation is headless:

- a guest service publishes an allowlisted inventory of installed programs;
- qOS creates program shortcuts from that inventory;
- starting one uses a RemoteApp channel and adopts its individual surface into
  the active qOS workspace;
- closing the last program permits the appliance to suspend after a delay;
- the Windows shell and desktop are not published.

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

## Management experience (design target)

The target Windows Apps system application lists installed programs and exposes:

- current layer, capsule/appliance identity, and catalogue explanation;
- runner and Windows compatibility settings;
- file, network, clipboard, and device grants using standard qOS controls;
- active processes and other programs sharing the same state boundary;
- snapshot, repair, duplicate, export, reset, and delete actions;
- an explicit route override and a reset-to-catalogue action.

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
