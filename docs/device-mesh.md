# Device sync, remote control and serving mesh

Tilo treats a person's devices as certified peers in the same collaboration
system. Device sync is not a separate app service: the same signed workspace
operations used with another person are replicated between devices belonging
to one identity. Same identity changes the authorization result, not the
signature, revocation or record-validation path.

The shell also has a private, user-root-derived continuation lane for the
device-specific part of “continue on another device.” Its bounded checkpoint
contains safe app launch intents, logical file references and per-device focus;
it excludes local paths, credentials, compositor surface ids and file bytes.
An authenticated same-identity peer merges the checkpoint, discovers missing
workspaces, and opens matching local app instances in their synchronized tabs
without stealing the active workspace or focus. The always-on relay retains
the latest aggregate checkpoint so sleeping devices can catch up later without
exposing personal session state to collaborators.

## One replication substrate

`tilo-relay::mesh::ReplicationIntent` maps a device's residency choice onto the
existing data planes:

- workspace layout, app records, ebook reading state and media-library records
  use signed collaboration operations;
- file names, metadata, versions and ACLs use collaborative filesystem records;
- file, book and media bytes use the filesystem's content-addressed blob
  hydration and deduplication.

An app may register an `AppRecords` resource family, but may not create its own
account, pairing, sync status or conflict protocol. Selecting ebook or media
replication automatically includes the file tree and blobs on which it depends.
An always-on composite server defaults to `FullWorkspace`; an interactive
device defaults to on-demand hydration. The home-peer deployment starts the
authenticated filesystem worker for every `workspaces[]` entry and advertises
content residency only while its aggregate freshness proof is complete.

## Device roles and authority

A device is interactive, always-on, or hybrid. Runtime service advertisements
are intersected with the immutable capabilities in its root-signed device
certificate. In particular, only `can_act_as_host` devices serve, and retaining
all workspace content additionally requires `can_read_all_workspaces`.
Revoked, expired, foreign-identity or wrongly scoped devices remain rejected by
the existing authentication and operation validators.

An always-on device can pin workspace state and store-and-forward collaboration
traffic. It can retain content, relay a remote-control connection, or advertise
app/compute capacity only when the corresponding service has actually started
and passed its own health checks; a certificate authorizes that service but does
not create it. Relay permission is not control permission:
the target compositor still creates the consented share, gates its surfaces and
filters remote input. Direct control and relayed control therefore reach the
same target-side authorization path.

## Multiple servers

Hosts are chosen by configuration, not ranked: a relay serves the workspaces
bound in its `relay.toml`, and a client reaches the home peers it registered.
There is no placement function choosing among candidate servers yet; one should
arrive together with a producer of live load and latency advertisements, not
ahead of it.

The first workloads are workspace replication, content residency, inbound
collaboration/chat relay and remote-control relay. Sandboxed compute uses a
dedicated mutually authenticated `tilo-personal-compute/1` session and the
signed-package launcher; the relay never accepts a command, host path,
environment, or bearer secret from a requester. A worker advertises only after
its package catalogue, launcher, private staging store and power policy pass
health checks, and disconnect/health regression withdraws it immediately.
Until workspace CAS hydration is connected to this protocol, the production
owner explicitly rejects jobs with remote inputs and never reports an output
digest that the requester cannot retrieve.

## Integration seams

- Shell/device settings write device role and per-workspace replication intent.
- `tilo-relay` exposes its effective `DeviceMeshProfile` through `RelayHandle`.
- The workspace runtime consumes the workspace subscription; filesystem and app
  runtimes consume the resolved resource set.
- Reachability discovery produces `ServerAdvertisement` values only after the
  peer's device certificate and same-identity/workspace relationship verify.
- Remote control needs an exact-target route, which today means a direct path.
  A workspace relay fans out to every member, so it is reported as the reason
  control is unavailable and never carries input. Relayed exact-target control
  is future work.
- The requester names the compute worker. `admit_compute_job` checks that
  worker's `ServerAdvertisement` (service, workspace, app, accelerator,
  capacity, energy saver and `PowerAvailability::is_critically_constrained`).
  The worker executes only verified tapp catalogue entrypoints, and retains
  exact requester/worker/workspace binding, bounded diagnostics, cancellation
  and monotonic result attribution.

No new wire message was added to `tilo-relay/1`: adding an enum variant to the
postcard protocol would make old peers reject the stream. Compute uses its own
versioned ALPN so old relay peers keep their byte-compatible v1 stream.

Device administration follows the same rule. `can_administer_devices` is a
certificate capability like `foreign`: a certificate that grants it signs under
its own preimage tag (`tilo-devcert-v3`) and encodes its capabilities behind a
marker byte older verifiers refuse. Every certificate that does not grant it
keeps its byte-identical encoding and signature, so no existing peer sees a
change; only a machine enrolled for administration is refused by builds that
predate it. Admin traffic travels on its own versioned ALPN,
`tilo-device-admin/1` (docs/device-reach.md), never inside an existing stream.

## Personal device fabric

`shell::device_fabric` is the common, transport-neutral policy model for the
person's interactive devices. It builds on Tilo's certified same-identity lane;
proximity, Bluetooth pairing, or sharing a Wi-Fi network are never treated as
authorization.

- **Play here** transfers a logical media identity, playhead, and playback
  state rather than a local path or PipeWire node. The destination must be
  online and advertise the required audio/video capabilities, and the old
  output is paused before the new one resumes.
- **Live audio route** covers streams that have no portable media identity:
  all system playback or the currently focused application's playback can be
  sent to a certified device advertising an audio output. Source choices are
  fresh compositor-minted tokens; applications and peers never receive a
  PipeWire node id. The target compositor prompts locally, starts its local
  sink, and returns a random capability bound to the exact peer and session.
  Version 1 is negotiated as bounded 20 ms, 48 kHz stereo S16LE frames with
  bounded capture and playback queues. Replay, reordering, oversized frames,
  stale consent, and a different peer are rejected. Policy disable, peer loss,
  session expiry, send failure, or focus loss for an application route tears
  down the workers and revokes the route; the Devices surface always exposes
  pending/streaming status and a stop action.
- **Speaker groups** turn outputs on several devices into one stereo pair,
  surround set, or set of points around a room. The device that creates a
  group authors it in its own snapshot, like a display placement; members
  are named by compositor-minted endpoint selectors, never `PipeWire` ids or
  names. Each member device asks its own user once ("Join 'Living room' as
  the right speaker?") and on approval authors a membership bound to the
  exact group, output and role, so changing any of them asks again. Any of
  the person's devices may then play to the group: every member admits each
  session under its own consent and returns a fresh random capability bound
  to that source and session. Leaving, deleting the group, a role change,
  the source going offline, or disabling media continuity tears sessions
  down; disabling media also withdraws the device's memberships.
  Settings → Speakers sets groups up through a Settings-only broker command
  (`speakerCommand`): choose outputs, play a short identify sound on each to
  tell left from right, place room speakers, name and save. Calibration
  measures every member with one microphone on any device and produces a
  before/after proposal; trims change only when it is applied, and one
  revert restores the previous ones.
- **Universal clipboard** is a content-addressed, de-duplicated 50-item
  history. Entries are explicitly classified as shareable and capped at
  256 KiB. Primary selection, secret fields, and Tilo's secret MIME type never
  enter the shared history.
- **Roaming settings** converge per key. Hardware identity, display
  calibration, interfaces, mounts, and local credentials are rejected by
  namespace. Wi-Fi secrets use a separate type whose payload must already be
  sealed to the person's root-derived secret-sync key; plaintext passwords
  cannot be represented in fabric state.
- **Use as a screen** stores the person's spatial arrangement and maps an edge
  crossing through physical layout coordinates. Input still requires a
  short-lived, target-issued lease; adjacency grants no input authority.
- **App continuation** remains in `personal_continuity`. The fabric advertises
  capability but does not duplicate launch intents, CRDT records, topology, or
  focus state.
- **Legacy app windows** (Wayland and X11 clients) run only on the device that
  started them; Tilo apps run on every device against the same records, so
  only these need a stream, and no device ever streams its desktop or chrome.
  Rooms carry such a window as a `HostedWindow` tab naming its host. Each
  other device of the person lists the hosted tabs it currently lays out,
  with the tile's logical size, in a `WindowViews` action to that tab's host,
  repeated every 2 s while it shows any and sent empty once it shows none,
  together with what its hardware decoders take (codec and largest
  picture).
  The host streams just that window's surface to each listed viewer over
  the direct media route, to those devices alone and never to other members
  of the room. The encoder is chosen as for collaboration: hardware first,
  AV1/VP9 only when every viewer decodes it in hardware, else H.264. Frames
  are scaled down on the host to the lowest hardware-decode limit among the
  viewers (one encode serves all), so a board never falls back to software
  decoding. The window is sized for the largest viewer while no tab on the
  host shows it,
  and paced by its own damage up to the encode cap; a frame held back by the
  cap is sent when due.
  A viewer that stops refreshing lapses after 6 s. Pointer, wheel, keys and
  input-method text come back as window-local input; the person's focus on
  another device moves the host's keyboard to that window until the host's
  own input is used again; the host's own cursor neither moves nor changes
  its image, and its next input takes the pointer back. The first frame a
  new viewer gets is a keyframe, held if it overtakes the stream start. Copy and paste ride the universal clipboard. The
  app keeps its normal lifecycle on the host, and its encoder stops with the
  last viewer.
- **Staying awake and waking up.** An action from one of the person's own
  devices (`actions invoke --device`) counts as use: it renews a 10-minute
  suspend-only lease in the target session's inhibitor registry, so the
  screen still blanks and locks and the lease lapses once the requests stop.
  Each device's presence carries its wake routes (each interface's MAC as
  the network sees it, Ethernet or Wi-Fi, whether a magic packet wakes it,
  and the IPv4 subnet it was last seen on). Only an interface whose
  magic-packet wake is actually enabled wakes a device; a Wi-Fi chip may
  list WoWLAN while its machine does not wake from it, so the image enables
  it only where that is verified. A device
  without one cannot be woken over the network, and `device.wake` says so
  instead of sending anything. The typed `device.wake` action
  (provider `continuity.devices`, argument `device`: a label or device id)
  picks a device that is awake on the target's last subnet, this one first.
  That device sends the magic packet to UDP port 9 and probes the target
  until it answers, then reports back. Without such a device the action
  fails at once and names the subnet (`no awake device on 10.0.0.0/24`).
  Waking is device administration and needs no optional continuity switch.
- **Notifications** project only the app-authorized redacted or full payload.
  Dismiss and action replies return to the exact originating device instead of
  turning a mirrored notification into ambient remote-control authority.
- **Nearby Send** uses the resumable content transport, but Files sees only
  short-lived handles bound to its authenticated workspace and connection.
  The receiver consents locally, source traversal is anchored to an opened
  capability, and staging work is bounded and canceled on teardown. Receive
  destinations are likewise opaque handles minted only from that Files
  instance's writable grants. The receiver chooses a destination for the exact
  transfer; “remember” lasts only for the live authenticated Files session and
  never turns a path into ambient authority. Installation stays anchored to an
  open directory descriptor, uses collision-safe names, and retains bounded
  completion/failure health so interrupted receives can be recovered without
  silently falling back to Downloads.
- **Call handoff** moves a typed call session and its controls, not an app's
  capture handles. The target compositor asks once, mints local camera,
  microphone, or screen grants only after consent, and revokes the whole batch
  if the handoff cannot commit.
- **Controller forwarding** normalizes a locally classified physical gamepad
  into fixed axes, buttons, and hat events. Raw HID descriptors, serials,
  evdev codes, keyboard events, and pointer events are not representable on the
  wire. Both compositors continuously enforce the exact focused app and
  workspace and neutralize the virtual controller when that binding changes.
- **Shared clock** has its own probe and report, part of Media continuity
  because speaker groups and remote audio are what play against it; they carry
  timestamps only, no health or power data. Each round trip carries four
  `CLOCK_MONOTONIC` microsecond timestamps, and the prober keeps a per-peer
  NTP-style estimate of offset and skew: lowest-delay samples from a sliding
  window, a Theil–Sen fit for rate, an error bound, and a synced flag. The
  clock runs only toward a peer while a consumer holds a `FabricClockSync`
  handle for it, at 4 Hz, and forgets the estimate when the last handle drops
  or Media is turned off. A member without an estimate is shown syncing, and
  after a bounded wait fails with a reason, instead of playing silence.

The Devices surface exposes these capabilities individually. Disabling a
capability tears down its active leases; disabling continuity tears down all of
them. Device identities and transport routes stay compositor-owned rather than
being disclosed to applications.

This separation is an intentional Tilo advantage over product-specific
continuity silos: collaborative documents remain multi-person CRDT state,
personal interaction state remains private to certified devices, and physical
machine state remains local. Continuity is carried over negotiated certified
peer channels with explicit capability and schema versions; older peers can
continue using the collaboration protocol without being asked to decode new
continuity messages. Proximity improves discovery and layout, but never grants
authority by itself.
