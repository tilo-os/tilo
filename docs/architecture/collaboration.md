# Collaboration architecture

Tilo uses one collaboration model across direct peer-to-peer sessions, a
user-owned home server, and a hosted workspace server. Iroh is the only
network transport. The placement of a peer changes availability and storage,
not identity or document semantics.

## Identity

First setup creates two related Ed25519 identities:

- A human root, stored as the personal `primary.json` signing identity and as
  `/var/lib/tilo/user-identity/{privkey,pubkey}.bin` on the primary device.
- One device key at `/var/lib/tilo/device-private/keypair.bin`, certified by the human
  root in `/var/lib/tilo/device/cert.bin`.

The device key is also the Iroh `EndpointId`. The shell, filesystem sync, and
workspace host therefore present the same device identity. A home or hosted
server receives a separate device certificate with `can_act_as_host`; it never
receives the human root secret. A company node runs a session beside its
relay for the person's apps and agents: that session has a key of its own,
which the node vouches for and the root certifies at install as one of the
person's devices (never a host), so it reads and writes the node's rooms as
an ordinary channel member.

The same chain can endorse a self-built OS image without exporting the human
root: `ImageEndorsement` binds an immutable OCI SHA-256 manifest digest and Tilo
generation to a root-certified device with a separate, time-bounded,
root-signed image-signing authorization. Device enrollment authority alone is
insufficient. Verification checks both grants and then the device's
domain-separated Ed25519 image signature. Mutable tags are never a signing
input. Official releases additionally retain their transparency-logged
Cosign signature; the identity endorsement expresses owner trust, not a bypass
of release-channel policy.

Existing installations are migrated idempotently by `shell
--provision-identity`. Existing keys are checked for agreement and are never
silently replaced.

Native Tilo and the legacy-host compatibility route deliberately have different
trust models. A normal Tilo installation creates identity through the dedicated
OOBE and requires a LUKS2-encrypted system before that identity is created.
Machines with TPM2 use one required startup PIN only after a dedicated preflight
proves verified boot continues from the live medium through the installed
target. Secure Boot state on the installer alone is not enough. Machines
without every part of that boundary require one normal disk passphrase. That
same credential is the local
session-unlock credential, and successful disk authentication hands directly
to the installed user's first session rather than immediately prompting for a
second password. Native provisioning remains responsible for protected-key
migration and does not infer a person's identity from the implementation-level
Unix account.

Only when Tilo Desktop is installed on top of an existing Linux distribution does
identity associate with the already-authenticated Unix login. Its files live in
that user's XDG trees with ordinary Unix ownership, and a newly-created identity
takes its public display label from the account's passwd entry. This route does
not create a second login or identity password, and it does not rewrite or
migrate a pre-existing identity record.

### Revoking a device

A lost or stolen device is revoked by its person's root (`RevocationEntry`).
Any device of theirs announces the signed entry with its identity
(`IdentityHello`); receivers check the root's signature, not who carried it.

In the rooms a workspace host governs, the host takes it from there
(`crates/hosting/tilo-relay/src/rooms/revocations.rs`). An entry of the host's
own person joins the host's revocation log, which every handshake, session and
administration door reads, so the device is refused in every room at once.
Every room the host governs writes into its governance each entry its owner's
root signed, and each that revokes a device the room certifies to the signing
root: the device leaves the room's access and its channels, and every
member's replica refuses it from the room's own history, after a restart too.

A device whose revocation arrives after it authenticated stops counting as
authenticated from then on. A room a desktop governs does not wait for that
device either. An invite
pins its issuing device as the room's governor, so revoking that device alone
would freeze the room. When the root revokes a device it also names another
of the person's devices to govern in its place: a `GovernorSuccession`
(`tilo-user-identity`, `succession.rs`) binds the revoked device's certificate,
its revocation and the successor's certificate under one root signature, and
the successor must be native and certified to own workspaces. Settings'
revoke signs one naming the device that revokes. It names no room, so one
statement moves every room the revoked device governed. Members receive it
with the person's identity, check it against the root that certified the
pinned governor, and re-pin each such room to the successor
(`WorkspaceTree::follow_governor_successions`); on the successor itself the
room becomes its own. From then on the revoked device's governance is
refused and the successor's admitted, and the room's channels follow their
new steward. Where a root named two successors for one device, the lower key
governs everywhere (`current_governor`).

Not covered yet: governance the revoked device wrote before its revocation
reached a member stays, for the successor to undo; a workspace host whose own
device is revoked does not hand its rooms to another host; and a room
`StatePeer` governs pins its authority's certificate, so a revoked authority
there is refused but not succeeded. The rule is engine-neutral
(`current_governor`), so the one room engine applies it when it governs
rooms a desktop mints.

## Direct collaboration

`crates/session/shell/src/network/actor.rs` owns an Iroh endpoint and one `iroh-gossip` topic
per workspace. An invite contains the inviter's authenticated `EndpointAddr`,
workspace room, expiry, and an Ed25519 signature. Iroh chooses a direct path
when possible and uses its encrypted relay path when NAT traversal cannot
establish one.

Direct collaboration needs no workspace host. It is the fast path for an
ad-hoc invitation and remains active when a workspace host is configured. It
does use number 0's public iroh infrastructure — their DNS service for address
lookup and their relays for NAT traversal — which sees endpoint ids and IP
addresses but no content (`docs/architecture/security.md`, "What leaves the
device by default").

An invite also signs the maximum role its bearer receives: Viewer,
Commenter, or Editor. The inviter chooses that role before the token is
minted; changing it invalidates the signature. After the joining device
presents its person-signed device certificate, the workspace authority turns
that signed choice into the ordinary room-membership capability. Redemption
carries a hash of the exact signed invite, so multiple outstanding invites
cannot exchange names or roles and an ambient room participant cannot consume
the next grant. From then on the same role checks gate app actions whether
peers connect directly, through a home host, or through hosted infrastructure.

Every inbound room message is authorized after device-certificate
authentication. Membership with `Read` is required to request or receive room
state and presence; publishing room mutations requires `Write`. The sole
bootstrap exception accepts the first governance snapshot only from the exact
authority endpoint signed into the invite. Topic ids are treated as lossy
routing hints, so a collision is rejected rather than selecting an arbitrary
room.

Only a locally governing administrator can mint an invite or change membership
and capabilities. Whether this device may invite, and through which authority,
is one decision (`shell::invite_policy::may_issue_invitation`) that the invite
composer, the palette's `peer.invite` rows and the issuer all ask: the grant for
the `peer.invite.named` action under the workspace's policy, then
the room's governor — this person's relay (the admitted governor is the
identity this room selects, which may be a workspace-specific identity rather
than the device's default person), this device sharing a room it governs on
its home relay, or, for a member who holds a delegated invite grant in a
room someone else governs, a refusal that sends them to the owner rather than
a composer or palette row the issuer would reject. Ordinary role controls
expose Viewer, Commenter, and Editor;
Owner is structural governance authority and is never created or transferred
by cycling a role chip. What a role may change depends on the path the change
takes:

- **Directed action invocations** (an action invoked through the session action
  broker by a person, script or agent) are decided against the grants on the
  action's node in the published tree, filtered by the action's kind
  ([permissions.md](permissions.md)).
- **Replicated app documents are checked change by change.** The SDK signs
  every document change, snapshot or incremental together with what each
  change claims to be — its action key and the storage units it writes — and
  a receiver, a relay that cannot read a sealed payload included, decides
  every claim against the author's grants before merging it. Owners and
  Editors therefore hold the grants to change an app document; a Commenter's
  grant covers comments and reactions, and nothing else in it.
- **Governance, compaction and redaction operations** are signed with their own
  claims, which the room's grants give the owner alone.

Per-action enforcement is a property of the claims, not of the payload: the
broker holds no Automerge document and the payload may be MLS ciphertext, so
whether a change is a comment or a rewrite is decided by the claim the author
signed, under the same rule-in-the-tree as every other action.

Publishing a surface stream, granting or injecting remote control, and room
state responses likewise need a grant for the change they claim; passive
presence, requests, and validated history reads need a `Read` grant.

Live session handles in cursor, app-presence, remote-input, stream, and
scrollback messages are bound to the authenticated transport endpoint before
dispatch. A member therefore cannot impersonate another member merely by
copying that member's numeric session handle.

### Liveness without chatter

A converged desktop that nobody is using sends no application message. The
transport keeps liveness: a gossip neighbour is a member while its link is up,
iroh sends a QUIC keep-alive after 5 s without traffic, and the collaboration
endpoint ends a connection that hears nothing for 15 s, which takes the link
down. A peer whose link went down is dropped 5 s later unless it returns, so a
peer that leaves cleanly is gone within 5 s and one that vanishes (power,
network) within 20 s (`DEPARTURE_GRACE`, `PEER_TIMEOUT` in `actor.rs`).

Everything else is said when it changes or when someone new can hear it. A
device announces its identity and codecs when a link comes up and again only
when they change; a new revocation is read from disk on the 5 s upkeep tick
and announced at once, as before. Room changes are pushed when they land, and
a repair pull follows 2 s after activity settles; a peer that sees a request
naming changes it lacks asks back, and an idle desktop pulls once every 5
minutes and whenever a peer joins. A resting pointer is not restated: it is
said again only for a peer that joins. An idle workspace-host session pings
its host after 15 s of silence and is redialled after 25 s.

Older builds evict a peer silent for 15 s and beat every 5 s with a wall-clock
`Heartbeat`. A current build that hears such a beat on a topic beats back on
it (with a numbered, not timed, heartbeat, plus its identity and codecs) for
as long as the older build is there, so mixed rooms interoperate at the old
cadence and rooms of current builds only are silent.

## Room admission

A room document — owner, capabilities, governed identities, media policy,
checkpoint coverage, durable receipts, effect receipts and window contents —
arrives as a `SyncResp`. Every receiver admits it through one function,
`tilo_peer_state::room::admit_room_changes`: the native room peer
(`StatePeer`) and the shell's workspace tree (`WorkspaceTree::apply_room_changes`)
call the same code and differ only in the data they pass it. A batch is
refused whole, or its whole candidate is returned; a refused batch changes
nothing, the room's authority included. The checks, in order:

1. **The sender holds `Write` on the room** under the capabilities already
   held, unless it is the pinned governance writer, which may grant itself
   anything. The native peer resolves the sender from its certified roster;
   the shell from the person its network layer authenticated, and a sender
   with no authenticated person reached the tree only through the network
   gate that already required `Write`.
2. The batch decodes under the shared incoming limits.
3. The document names the room it answers.
4. **Governance changes come only from the pinned writer.** Compared by
   value, so an echo of held governance under another actor is not a
   change. Governance includes durable receipts and import proposals.
5. **Window contents change only by their owner.** A native room's
   `contents_owners_v1` registry names each namespace's owner (a room
   without one belongs to the pinned authority device). A shell room has no
   registry: each machine owns the entries minted under its id salt, and the
   receiver refuses a batch that rewrites one of its own.
6. Effect receipts are immutable.
7. Checkpoint coverage and durable receipts never rewind — even from the
   governor.
8. The media policy is well formed and its transition permitted. The
   receiver then binds a policy to its own authority record before
   installing the candidate.

### Joining: governance first, app data after

A device becomes a member on the room's governance alone
(`tilo-joined-room`, `install`): the join's bootstrap is admitted in the
host's order, but only its governance — membership, roles, policy and the
certificates they import, everything an app message is authorized against.
The bootstrap's app messages (the host's own record checkpoints since its
last full one) are left out: a device that does not hold the room yet often
cannot decide them (the app surface a change names is read from the room's
own content; a sealed write needs its record's checkpoint), and the host
retains every one on its record's channel. They arrive through the room's
sync once it is installed and are authorized like any later write, so how
many messages a room has never decides whether someone can join it.

An app message whose prerequisite is still in flight (its author's
certificate, its record's checkpoint, the changes it builds on, its app
surface) is held, and every later room change and admitted document retries
what is held. The queue is bounded (`MAX_HELD_INPUTS`, `MAX_HELD_BYTES`) and
never refuses the input being admitted: when it is full, the message held
longest is decided again, applied if it now can be, and otherwise let go
unapplied with what it waited for (`ReceiveOutcome::dropped`, logged by the
room owner). Nothing is applied without being authorized, and nothing let go
is lost: the peer remembers what each message let go waited for (an author's
certificate, a record's state), and once one of those lands it reports
`replay_wanted`, on which the device asks the room's host to replay what it
retains (`NetCmd::NativeReplay`, a fresh subscription), and the message is
decided again. The shell's own hold for app surfaces does the same: a room
whose held inputs were let go is replayed once a surface is named. A device
that restarts subscribes afresh, and its host replays everything anyway.

A write that names an app surface this device has no copy of was made with
a newer build of the app, so the device needs that app's update. The shell
holds the write and runs that app's update (`app_versions::surface_named`).
This is the same `app update` an app that is behind takes before it starts.
Once the update is in, the shell decides the held writes again, against the
surface the updated package ships (`app_versions::installed_surface`, signed
by the package's publisher like the surface the app runs with). An app the
device does not have installed is not installed for it; its writes wait.

A room's sync arrives as a burst of relay inputs. Inputs of one session
queued back to back are admitted together
(`NativeWorkspaceOwner::receive_batch`, at most `native_admission::MAX_BATCH`
at a time). Each one is authorized alone, in order, exactly as a single
input is. The batch is published once, as one atomic state-file replacement.
A crash before that publish loses nothing: the uncommitted inputs are still
retained by the host, and the next subscription replays them.

### Write authority on a room's governance

The governance writer is pinned from local, authenticated knowledge, never
from the batch being admitted:

- **An invite** names the authority endpoint inside its signature, so the
  receiver can check it (`pin_room_authority`). A native room pins the
  invite's authority device certificate.
- **The room was minted here** (`Local`): no remote sender writes its
  governance, except another device of the person who owns it.
- **The room is a person's** (`Person`): any authenticated, unrevoked
  device of that person. The person's signed workspace directory pins it
  for rooms they own and names the governor of rooms someone else owns; a
  continuation offer, which only the person's own devices send, pins the
  person provisionally until that directory names the real governor.
- **Nothing names a governor** (`Unpinned`, a room joined without an invite
  naming its administrator, or enrolled before the directory arrives): every
  governance change is refused, the first snapshot included. The first peer
  to answer does not become the governor. A state file written while a room
  still waited for that first answer loads as the same read-only
  `Unpinned`; it is upgraded only by an invite, the person's directory or a
  continuation offer from the person's own device. A pin an older build
  took from a first answer is a recorded `Peer` or `Person` like any other
  and is kept: a state file does not say how a pin was taken.

This covers a governance change on the hop that authenticated it. A relayed
or delegated governance change — one a member forwards on the governor's
behalf — needs the operation signed rather than the hop authenticated.

### Reading governance

`tilo_peer_state::room::governance_of` returns a named `RoomGovernance`:
every governed value exactly as stored, capability and identity rows as
their raw JSON blobs. Admission compares it byte for byte, so a changed but
undecodable row is still a change. Decoded views of the same document state
are cached on the native peer's `CachedDocument`: `policy()` (owner and
decoded capabilities) and `identities()` (each identity row decoded). A
document message is authorized from those caches, so it pays no JSON decode
while the room's governance is unchanged.

Effect receipts are compared by their stored bytes; a receipt is decoded
only when its bytes differ from what is held. The native peer's receipt
ledger keeps each receipt as received, never re-encoded.

### Retention

- **Identity admissions.** A governed identity row keeps its first
  redemption receipt (`admission`) for good, since that is its admission
  evidence. Of the later receipts (`admissions`) it keeps only the latest
  `tilo_peer_state::governance::MAX_RETAINED_IDENTITY_ADMISSIONS` (8). No
  replay protection is lost: every redemption also writes each receipt it
  has seen into the per-invite `invitation_receipts_v1` ledger, in the same
  transaction that drops one from the row. That ledger refuses a second
  redemption of a spent invitation, from any device, and is never pruned.
- **Effect receipts** are not pruned. They are immutable evidence ("omission
  cannot erase old evidence"), and dropping one is safe only behind a
  compaction checkpoint every peer agrees on, which does not exist yet.
  Admission keeps their per-message cost flat instead: an unchanged receipt
  is never decoded.

## Workspace hosts

`crates/hosting/tilo-relay` is the persistent server tier. Despite the retained
binary name, it is an authenticated Iroh workspace host, not a transport
relay. Peers prove possession of certified device keys on the relay ALPN
(`tilo-relay/1`) before subscribing.

Devices talk to a host through `crates/hosting/tilo-relay-session`: the
session client (`RelayClient`), the co-edit session (`RelayCoeditSession`),
anti-entropy, the names and answers of the actions a room host serves
itself (a room's `tilo.workspace` governance; the host's own `tilo.host`
operations, creating, listing and entering rooms, asked on the person's
continuity workspace before any room is held; a member's own `tilo.member`
namespace claim and effect receipt), the host's old administration door
(`RoomAdminClient`, now only for removing a member) and the workspace join
client (`WorkspaceJoinClient`). The shell, the action broker, joined rooms and
the runtime depend on that crate alone; `tilo-relay` is the daemon, depends on
the session crate and re-exports it under its historic paths, and appears in
client crates only as a dev-dependency for tests that start a real host. The
wire messages stay in `tilo-relay-protocol` and the frame codec in
`tilo-relay-client`.

The shell can attach a host with `ShellNetwork::connect_host`. For unattended
configuration, `TILO_COLLAB_HOST` accepts an Iroh endpoint id or the JSON
representation of a complete `EndpointAddr`. Durable messages travel to both the direct gossip mesh and the
host. The host persists and store-and-forwards them; presence stays ephemeral.
Multiple hosts federate through the same authenticated Iroh protocol.

This gives three deployments without three protocols:

1. Direct invitation: peers exchange an invite and collaborate over Iroh.
2. Home server: a certified always-on device pins workspaces and bridges
   offline periods.
3. Hosted server: the same daemon and protocol run on infrastructure operated
   for the user.

## Wire boundaries

- Direct fan-out: deterministic `iroh-gossip` topic derived from workspace id.
- Persistent host: the relay stream, ALPN `tilo-relay/1`, mutually
  authenticated with device certs. There is one framing on it, at the peer
  protocol version the handshake's first field names
  (docs/architecture/compatibility.md); the ALPN never changes, and a device
  at another version is told so in the handshake.
- Content: `iroh-blobs` on the filesystem endpoint.
- Messages: postcard-encoded `NeighbourMsg`; non-ephemeral messages are stored
  by the host as opaque co-edit deltas.
- Frames: `tilo_delivery_protocol::transport_v2::EnvelopeV2`, state and
  private actions on one stream, each private action signed by the device
  that sends it (below).
- Compact state: a co-edit publisher leaves the outer
  `Envelope::Delta::signature` all zero, because the session and the
  payload's own signed op authenticate it, so such a delta travels as
  `EnvelopeV2::UnsignedDelta`: 66 bytes less per keystroke frame and per
  recipient. The session reader restores the same `Envelope::Delta`, so the
  host stores and routes one value whatever the wire was.
- Presence: the host coalesces, paces and batches the presence it
  forwards, per receiving connection (`tilo_relay::presence_mailbox`).
  Latest-state presence — a pointer, a caret, a selection, the range being
  typed — has a slot (`NeighbourMsg::presence_slot`, which the host reads
  from the message, decoding only the two variants that can have one); a
  newer message in the same slot from the same author replaces one still
  pending, so a busy connection gets where things are, never a backlog, and
  never loses the last caret to a full queue. Rates: a moving pointer at most
  every 50 ms (20 Hz) per slot, which receivers' glide (`POINTER_EASE_MAX`)
  draws as smoothly as the shell's 30 Hz; carets, selections and typing on
  every change, after a 5 ms batching window; unslotted presence (heartbeats,
  `SyncReq`, arrivals) never coalesced, in order, and never overtaking
  presence posted before it. Deltas never wait for presence. Every presence
  travels the state lane, so a client's presence keeps its order. The host
  writes each flush as one `EnvelopeV2::PresenceBatch` per room, every
  item coded against what that
  session last received in the same slot (kept prefix and suffix, changed
  bytes between): a pointer move costs a few dozen bytes instead of ~236. The
  session reader expands a batch into ordinary `Envelope::Presence` values.
  A slot can only supersede the same
  authenticated author's presence, and the host forwards the bytes unchanged.
- Coded deltas: the host writes each live zero-signature delta of at most
  4 KiB as an
  `EnvelopeV2::CodedDelta`: ranges copied from the last delta that session
  received from the same author on the same route (the delta's leading kind
  and channel bytes) in the same room, and the bytes in between. Two
  keystrokes of one author repeat their keys, channel, app, cluster and record
  ids, Automerge actor and signed-op ids, so a keystroke costs the recipient
  less than half the ~481 B compact frame. The session reader rebuilds the
  `UnsignedDelta` byte for byte and returns the same `Envelope::Delta`;
  signatures inside it verify as before, and the host decodes nothing new.
  Both halves keep at most 64 slots of at most 4 KiB per session. Deltas are
  never delayed for it. Replays and positions go uncoded.
- Packets, not frames: a 27 B presence batch costs about 35 B of
  QUIC packet on top, so the host writes whatever goes to one connection
  together in one write where it can. A live delta's write carries its
  position and every presence that may go early: carets, selections, typing
  and events at any time (their 5 ms window exists only to share a frame),
  and a pointer once 25 ms have passed since it last went (never later than
  its 20 Hz pace, never more often than twice it). A batch that falls due
  takes the same company. A keystroke's `Typing` and `Caret` reach the host
  while its delta is still being committed; while a delta of the same author
  is unpublished, they are held, up to 20 ms, and leave in the delta's write,
  so a member receives the character and the caret after it in one packet.
  The delta never waits, and the frames are the same on every protocol.
- Client pointer pacing: the shell and the toolkit publish a moving pointer
  at about 30 Hz, and the host forwards one at 20 Hz, so the client's session
  writer (`tilo_relay_session::client`, `pointer_pacing`) writes each pointer
  slot at most every 50 ms (`coedit::POINTER_GAP`, which is also the host's
  pace): a move after a pause at once, a move inside the gap held and
  replaced by newer ones, and the latest written when the gap ends, so the
  resting position always arrives. A held move that has gone stale by then
  waits up to 15 ms for the producer's next move, when that is due sooner
  than the staleness it saves, and the fresher move goes in its place; a
  30 Hz cursor so reaches peers a few milliseconds old rather than half a
  producer period. Anything but a delta or another
  latest-state presence takes the held pointers ahead of it in the same
  write, so an app's `Left` never overtakes its last pointer. A move the host
  would only have coalesced away no longer costs an upstream packet or the
  acknowledgement it draws. The writer paces whatever session it serves,
  whoever the author is; the frames are unchanged. The host counts its own
  gap from when the previous move was due rather than from when its writer
  got to it, and a pointer does not wait the 5 ms batching window for
  company, so moves that already arrive a gap apart are forwarded as they
  arrive and a late write never adds to the next move's wait. The direct
  (gossip) path paces the same way with the same `PointerPacer`, slot
  classification (`pointer_pacing::slot_pacing`) and gap: the shell's network
  actor hands every pointer move to the room's hosts at once (their writers
  pace it) and holds it for gossip, broadcasting a slot at most every 50 ms,
  the resting position always, carets and typing past a held move, and any
  other message only after it. A peer-to-peer drag so costs each neighbour
  20 packets a second instead of 30, and the acknowledgements they draw;
  the actor arms no timer while nothing is held.
- Remote actions (v1): versioned JSON `RemoteActionFrame`s (`tilo_actions::remote`).
  A caller follows an invocation with one waiting request at a time —
  `wait_ms` on `Invoke`, or on `Observe` with the `seen` update — which the
  host holds on its change signal, holding no thread, until the invocation
  moves or the wait (at most 10 s) runs out. The answer is read when it is
  sent, as the caller is then. A host that predates the fields ignores them and
  answers at once, and its callers fall back to polling.

Automerge remains the convergence layer. Transport events never decide
authority: room governance, signed operations, device certificates, and
revocations are validated above Iroh.

### Private actions are signed by their source device

A cross-device action (catalogue, invoke, status, cancel) reaches the target
device's broker through the workspace host, which attaches the source's device
certificate. Certificates are public, so the host's word that a device sent an
action proves nothing: a host that has seen a member's certificate could name
that member as the source of an action it made up. The hop is authenticated;
the call must be too.

So the sending device's `RelayClient` signs every action it sends as itself
(`SignedDirectedAction`): domain `tilo-peer-action-v1\0`, its issue time, and
the exact encoded action (room, request id, source, target, message), with
the key its certificate names. The host verifies the signature against the
session's device before routing, and forwards the signed bytes unchanged in
`CertifiedSignedDelivered`; it never signs. The target's `RelayClient` accepts
the delivery only if the attached certificate's device made that signature,
and the broker then:

- refuses an effect (anything but a catalogue refresh or a status read,
  `ActionMessage::requires_signature`) that arrives unsigned, with
  `action_signature_required`, on first admission and on every durable
  recheck;
- refuses an action issued more than 60 seconds from its clock either way
  (`action_stale`), or one it already admitted within that window
  (`action_replayed`, `ActionReplayWindow`). The durable journal's
  per-invocation idempotency remains the second line;
- still checks the certificate against the room's current authority and
  membership: a signature identifies the device, it does not authorise it.

Refusals carry typed codes (`action_not_authorised`, `unknown_action`,
`action_confirmation_required`, `invocation_conflict`, …), not one opaque
string.

A session that holds no device key (a state-only client) cannot sign, so
brokers serve it reads and refuse its effects. Replies from the broker to the caller
are not yet signed end to end: a host can still forge an observation or a
refusal to the caller, though not an effect at the target.

## Operational rules

- Losing reachability must not create a new identity.
- An unreadable or mismatched key/certificate is a hard error.
- Invites bind the complete Iroh address to the signing endpoint id.
- A host is optional for correctness but required for 24/7 availability and
  offline store-and-forward.
- The legacy standalone transport-relay crate and all libp2p dependencies are
  removed.
