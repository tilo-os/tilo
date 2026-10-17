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

Compute workers are ranked. `tilo_relay_protocol::placement::place_compute_job`
chooses among the workers advertising in a workspace: only those that pass the
same eligibility rule admission enforces
(`ServerAdvertisement::compute_ineligibility`), then operator priority, power
(external, charging, unknown, battery), load as a fraction of capacity,
advertised latency and finally the device key, so equal candidates always
resolve alike. Every passed-over candidate carries its reason
(`critical_battery`, `no_gpu`, `at_capacity`, …). The relay answers it as the
action `compute.place` of provider `tilo.compute` on the workspace's
`workspace.compute:<workspace id>` resource, to the relay owner's own devices
acting as themselves (the principal the compute control stream admits) and to
no agent, bot or other person; it logs each choice and its reason. The action
only chooses: the requester still binds its job to that exact worker, and the
relay re-admits it against the worker's live advertisement. Workers advertise
no latency yet (the relay reports `0`), so ranking is by power and load until a
live round-trip producer exists.

The first workloads are workspace replication, content residency, inbound
collaboration/chat relay and remote-control relay. Sandboxed compute uses a
dedicated mutually authenticated `tilo-personal-compute/1` session and the
signed-package launcher; the relay never accepts a command, host path,
environment, or bearer secret from a requester. A worker advertises only after
its package catalogue, launcher, private staging store and power policy pass
health checks, and disconnect/health regression withdraws it immediately.

### Job content

A job names its inputs by blake3 digest and its result names its outputs the
same way; the bytes never ride in the durable compute envelopes. Each digest
moves on a stream of its own on the compute connection
(`StreamOpen::ComputeContent`, `tilo_relay::compute_content`), and the relay
decides from the job's durable record who may have it:

- **Inputs.** The job's worker may ask for a digest the job lists, while the
  job runs. The relay asks the job's requester, which answers from its own
  workspace content (`ComputeClient::serve_inputs`). A worker therefore reads
  exactly what the invoker itself can produce, never more: being a worker
  grants no read of its own, and the requester re-checks that the digest
  belongs to a job it submitted to that worker. The worker hydrates each
  listed digest once into a private per-job directory, bounded by
  `MAX_COMPUTE_INPUT_BYTES`, the job's memory limit and its lifetime; the
  launcher stages those bytes read-only into the sandbox and the directory
  is removed when the job ends. The package never receives a network or
  blob credential, host path, environment or argv from the requester.
- **Outputs.** Before reporting a completed job the worker stores every
  output and writes an index of exactly those digests beside the job's
  claim, so it never reports a digest the requester cannot fetch. Only the
  job's requester may ask for them (`ComputeClient::retrieve_outputs`), and
  only digests the job reported; the worker re-checks the claim's requester
  and the index. Outputs stay retrievable for a day (`OUTPUT_RETENTION`).

Every hop checks the bytes against the digest and its bound: the source
before sending, the relay while forwarding, the asker on receipt. A refusal
carries a stable code, so a job whose input its requester cannot produce
fails promptly as `input_unavailable` (`input_integrity`, `input_too_large`
and `input_timeout` likewise) rather than hanging. The requester has to be
online while its job hydrates.

An input the worker already holds does not travel. Before fetching one, the
worker sends a fresh random challenge through the relay
(`StreamOpen::ComputeContentProof`, under the same input rule), and the
requester answers `blake3::keyed_hash(nonce, bytes)` over its own copy, plus
a locator hint (for workspace content, the file version's manifest hash).
The worker stages its own copy only when that copy hashes to the digest and
reproduces the proof; otherwise it fetches as above. Possession is the
requester's permission (it reads its content under its own access), so a
job still runs only on bytes its invoker can produce: knowing a digest, or a
worker happening to hold the bytes, grants nothing. The requester still reads
its copy to prove it; the upload is what is saved. The worker's sources are
`HeldContent`: its retained outputs (a job chained on an earlier output runs
without the requester sending it back) and, where the worker keeps a replica
of the workspace, that replica's file versions (`WorkspaceContent`). On an
always-on device the replica is the home peer's (`fs replica`, the stores the
home-peer bundle names): the relay daemon hands its worker each bundle
workspace's cache directory for the relay workspaces it backs, and a job in
one of those workspaces may run on the replica's file versions
(`ReplicaContent`; a job in any other workspace never reaches it). The
replica process keeps its stores open, so the worker reads a version's file
beside it without opening the store, found by the requester's locator and
used only when the bytes hash to the digest and match the proof; a partly
hydrated or replaced file is fetched instead. Versions of 1 MiB or less are
loose blobs inside the store's database, so the worker asks the replica for
them: each workspace's replica serves its own store, read-only and
owner-only, on `replica-content.sock` in that workspace's cache directory
(`fs::replica_content`). It answers only versions wholly on the device,
checked against their manifest and at most 1 MiB, a few reads at a time; the
worker trusts the answer no further than for a file it read itself (digest
and proof), and a replica that is not serving means those inputs are
fetched, as before.
`InputTransfer` reports fetched and held inputs separately; the
`offloadable_action` and `compute_content` relay tests print both timings.
On a shared CI runner (requester, relay and worker on one host), offloading
the 12-photo thumbnail batch (129 MB) took 5.9–9.3 s with its inputs fetched
from the requester (3.1–5.0 s of staging) and 3.3–5.3 s to a worker whose
replica already held them (0.55–1.2 s of staging, no input bytes sent). A
1 MiB input staged in 42–54 ms fetched against 17–34 ms proven; a 15 MiB one
in 879 ms against 185 ms.

Placement weighs only what a worker would receive. `compute.place` takes the
job's inputs as `inputs: [{digest, bytes}]` (at most `MAX_COMPUTE_INPUTS`),
which `Offloader` sends. The relay notes, per live job, which inputs the
requester proved and which it forwarded; when the job completes, an input
proven and never forwarded is one the worker held, and a forwarded one is
not. It remembers up to `MAX_KNOWN_HOLDINGS` such digests per requester and
worker, plus the share of the last completed job's inputs the worker held,
which is what an input not seen yet is expected to be (a replica keeping the
originals holds the next photo too); a reattaching worker starts unknown.
Moving only the rest, plus the round trip, is weighed against the
requester's own run, so a worker expected to receive nothing costs only the
round trip: a home server holding a phone's photos takes the thumbnail batch
however slow the uplink. None of this is the worker's word or grants anything: every job
still proves possession or fetches, and a hint gone wrong (an evicted copy)
costs one fetched run, which unlearns it.

The opposite hint goes stale too: a worker seen fetching everything over a
slow link would otherwise never be tried again, even once its replica holds
the photos. Each job kept local on that estimate is credited with what it
would have saved had the worker held its inputs (the local run less the round
trip). Once `REPROBE_AFTER_MS` (10 minutes) has passed and that credit covers
the worst case of trying (every byte moves), the next such job is placed on
the worker anyway: `compute.place` answers it with `"reprobe": true`, the
`reprobing_held_inputs` reason and the estimate it overrode. The job shows
what the worker holds like any other. One that finds nothing held doubles the
wait and the credit the next try needs, at most `MAX_REPROBE_BACKOFF` (6)
times; one that finds anything held resets it. So a try costs at most what
staying local already forwent, and a worker that still holds nothing is tried
ever less often. On the thumbnail model (12 new 10.75 MB photos a minute, a
507 ms local run, 30 ms away): at the 38 MB/s loopback rate a worker whose
replica synced 45 minutes in is tried at minutes 11, 32 and 73 and takes
every batch from then on; over the phone's 1 MB/s uplink the first try waits
for 270 kept batches. A worker that never holds them is tried 20 times in a
week at 38 MB/s (58 s of fetching against 80 minutes forgone) and 5 times
over 1 MB/s (643 s against the same 80 minutes), ever further apart.

### Offloadable actions

An action whose contract declares `offload` (`tilo_actions::Offload`: a signed
package, its catalogue entrypoint, GPU need and CPU, memory and output
ceilings) is a pure content transform: files in, content out, the same bytes
whichever device computes them. Invoking one goes through
`tilo_relay::offload::Offloader::invoke`, the same path for a person, a script
or an agent, after the action door admitted the invocation:

- The inputs are workspace files, named by the manifest hash the invoker's own
  view resolved. `WorkspaceContent` reads each from this device's content
  store, checks it against its manifest and hashes the bytes; only digests it
  bound this way are servable to a worker, so the fs's manifest addressing and
  compute's byte digests meet without either being guessed from the other.
- It asks the workspace's relay `compute.place` (`RelayPlacement`). When
  another device is better placed it binds a job to that worker (no
  arguments, the declared ceilings, the inputs by digest) on its long-lived
  compute session, serves exactly those inputs, follows the result and fetches
  the outputs back, storing each as a file version.
- No better device, an unreachable relay, a package not installed here, or any
  remote failure runs the same transform locally. An action that fails on its
  own inputs fails identically either way.

The output is the outputs as content resources, in order, identical wherever
the action ran; where it ran and how long it took are reported beside it
(`OffloadRun::ran_on`, `Offloader::wall_times`). Thumbnailing a batch of images
(`thumbnail::batch`, entrypoint `thumbnail-batch` of `so.tilo.thumbnails`) is
the first offloadable transform.

An app declares one with `#[effect(content, kind = "read", offload(app = "…",
entrypoint = "…", cpu_ms = N, memory_mb = N, output_mb = N))]`; its inputs are
its content parameters (`#[param(content)]`, a list of content for
`Vec<String>`), and its handler is the local run. Photos publishes
`make-thumbnails` this way. The action door admits an invocation of it like any
other, then asks the device's route (`HostServices::action_offload`,
`tilo_actions::offload::ActionOffload`) where it runs:

- `tilo_relay::offload::OffloadRoute` runs it through `Offloader::offload`
  when every input is document media the invoking room reaches on this device,
  bound by its blob hash and read only through the invoking app's scope in its
  room (`WorkspaceContent::bind_blob` over `tilo_relay::offload::RoomMedia`),
  and keeps the outputs as that room's media (`WorkspaceContent::keep_blob`),
  so the door answers with exactly the `tilo.content` addresses the app's
  handler returns.
- Otherwise — a room this device places no compute for, an input not here, no
  better device, any remote failure — the door hands the app the invocation
  and its own handler answers it; the route hears how long that took
  (`ActionOffload::ran_here`, `Offloader::record_local`), which placement
  weighs next time.

Content named twice is one input on both paths, as a worker stages it.

A session that owns its profile's content store — the standalone runtime
(`tilo_runtime::standalone`) and the shell's compositor-free runtime host —
hands its broker the route `tilo_runtime::offload::action_offload` builds. It
is configured by the device's home-peer registry (`peers/home-peers.json`
beside the collaboration identity, or `TILO_HOME_PEERS_PATH`): the person's
registered relay that retains the session's room shard
(`tilo_relay_protocol::room_workspace_id`, as every relay binding derives it)
places its compute (`tilo_relay::offload::home_relay`). With no such relay, or
with `TILO_ACTION_OFFLOAD=off`, there is no route and the door behaves exactly
as before. With one, `DeviceOffload` returns the route at once and connects in
the background — a relay session subscribed to the room's workspace for
placement and a compute session pinned to the relay's device key, over the
store the apps' blob socket serves — answering `RunHere` with why until it is
up and for good if the relay refuses this device or does not answer within
`START_TIMEOUT`.

The desktop compositor's apps keep their media in the filesystem daemon, a
process of its own, so its broker gets the route
`tilo_runtime::offload::session_action_offload` builds for the session's rooms
(`compositor::session_bootstrap::session_action_offload`), configured the same
way. Its media is `tilo_relay::offload::SessionMedia`: the daemon's
document-media socket (`launcher::services::blob_socket`), connected as the
desktop session — the principal the shell already opens app content with,
which hands out nothing but one app's scope in one room — so inputs are read
and outputs kept through the invoking app's own scope, with no new protocol
and no authority beyond the session's. A daemon that does not answer is a
`RunHere` with why. Rooms the session creates after it started run their
offloadable actions here until the next session.

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
- A compute job names its worker, usually the one `compute.place` chose.
  `admit_compute_job` checks that worker's `ServerAdvertisement` (service,
  workspace, app, accelerator, capacity, energy saver and
  `PowerAvailability::is_critically_constrained`) by the rule placement uses.
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
- **An app asking another of the person's devices.** `actions invoke --device`
  is fire-and-forget; an app that needs the answer (Settings reading each
  device's storage summary) invokes the host's `tilo.own-device` provider,
  action `identity.device/invoke`, with `{device, app, resource, action, arguments}`.
  The broker admits it only for the person at this device and, from an app,
  only for that app's own actions; agents, bots, Shortcut steps and remote
  callers are refused. The shell sends it only to a device that is enrolled,
  authenticated to it under the person's own root, unexpired and unrevoked,
  as an ordinary invocation of `application:<shared room>:<app>` under the
  same invocation id, and follows it there by long-poll; the other device
  decides it for the device its transport authenticated. Its outcome becomes
  the local invocation's. No new frame exists for it.
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
