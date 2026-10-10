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

## Direct collaboration

`crates/session/shell/src/network/actor.rs` owns an Iroh endpoint and one `iroh-gossip` topic
per workspace. An invite contains the inviter's authenticated `EndpointAddr`,
workspace room, expiry, and an Ed25519 signature. Iroh chooses a direct path
when possible and uses its encrypted relay path when NAT traversal cannot
establish one.

Direct collaboration has no server requirement. It is the fast path for an
ad-hoc invitation and remains active when a workspace host is configured.

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
composer, the palette's `peer.invite` rows and the issuer all ask: `Invite` on
the `peer.invite.named` action under the workspace's capability policy, then
the room's governor — this person's relay (the admitted governor is the
identity this room selects, which may be a workspace-specific identity rather
than the device's default person), this device sharing a room it governs on
its home relay, or, for a member who holds a delegated `Invite` grant in a
room someone else governs, a refusal that sends them to the owner rather than
a composer or palette row the issuer would reject. Ordinary role controls
expose Viewer, Commenter, and Editor;
Owner is structural governance authority and is never created or transferred
by cycling a role chip. What a role may change depends on the path the change
takes:

- **Directed action invocations** (an action invoked through the session action
  broker by a person, script or agent) are checked against the action's
  declared permission requirement — `Read`, `Comment`, `Write`, `Invite` or
  `Admin` — and the caller's live capabilities.
- **Replicated app documents are checked only as a whole.** The SDK signs every
  document change, snapshot or incremental and whatever action produced it, as
  `OpClass::Splice`, which requires `Write`. The identity service checks the
  role table and live capability restrictions before signing, and every broker
  checks them again on receipt. Owners and Editors can therefore change any
  part of an app document; Commenters and Viewers can change none of it, so a
  comment stored inside an app document is refused for a Commenter; and an
  Editor is not limited to particular actions.
- **Governance, compaction and redaction operations** are signed with their own
  classes, which the role table grants to the owner only.

Per-action enforcement does not exist for replicated app state, and signing the
action's declared class would not create it: the broker holds no Automerge
document, the payload may be MLS ciphertext, and an Automerge change does not
name the action that made it, so a Commenter could label any edit as a comment.
Enforcing actions such as "commenters may add comments" needs structure the
receiver can check — for example comments kept as their own records that
accept only `Comment`-class changes. Until then, treat the app-document role
split as write or no write.

Publishing a surface stream, granting or injecting remote control, and room
state responses likewise require `Write`; passive presence, requests, and
validated history reads require `Read`.

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
relay. Peers prove possession of certified device keys on the `tilo-relay/1`
ALPN before subscribing.

Devices talk to a host through `crates/hosting/tilo-relay-session`: the
session client (`RelayClient`), the co-edit session (`RelayCoeditSession`),
anti-entropy, room administration (`RoomAdminClient`) and the workspace join
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
- Persistent host: `tilo-relay/1`, mutually authenticated with device certs.
- Content: `iroh-blobs` on the filesystem endpoint.
- Messages: postcard-encoded `NeighbourMsg`; non-ephemeral messages are stored
  by the host as opaque co-edit deltas.
- Private actions: the v2 envelope (`tilo_delivery_protocol::transport_v2`)
  on `tilo-relay/3`, and on `tilo-relay/4`, where each action is signed by the
  device that sends it (below).
- Compact state: `tilo-relay/5` (`ALPN_COMPACT_STATE_V1`) is `tilo-relay/4`
  whose co-edit deltas travel as `EnvelopeV2::UnsignedDelta`. A co-edit
  publisher leaves the outer `Envelope::Delta::signature` all zero, because the
  session and the payload's own signed op authenticate it, so the field is
  dropped on the wire: 66 bytes less per keystroke frame and per recipient. Each
  session's reader restores the same `Envelope::Delta`, so the host stores and
  routes one value whatever the wire was. A client dials `tilo-relay/5` first and
  falls back to `/4`, then `/3`, on `no_application_protocol`. A host writes
  `UnsignedDelta` only to `/5` sessions; older ones get the full envelope they
  know.
- Presence (TIL-710): the host coalesces, paces and batches the presence it
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
  still travels the state lane it always did, so a client's presence keeps
  its order. `tilo-relay/6` (`ALPN_PRESENCE_BATCH_V1`) is `/5` on which the
  host writes each flush as one `EnvelopeV2::PresenceBatch` per room, every
  item coded against what that
  session last received in the same slot (kept prefix and suffix, changed
  bytes between): a pointer move costs a few dozen bytes instead of ~236. The
  session reader expands a batch into ordinary `Envelope::Presence` values.
  Clients dial `/6` first and fall back to `/5`; older sessions get the same
  coalesced presence, one frame each. A slot can only supersede the same
  authenticated author's presence, and the host forwards the bytes unchanged.
- Coded deltas (TIL-755): `tilo-relay/7` (`ALPN_CODED_DELTA_V1`) is `/6` on
  which the host writes each live zero-signature delta of at most 4 KiB as an
  `EnvelopeV2::CodedDelta`: ranges copied from the last delta that session
  received from the same author on the same route (the delta's leading kind
  and channel bytes) in the same room, and the bytes in between. Two
  keystrokes of one author repeat their keys, channel, app, cluster and record
  ids, Automerge actor and signed-op ids, so a keystroke costs the recipient
  less than half the ~481 B compact frame. The session reader rebuilds the
  `UnsignedDelta` byte for byte and returns the same `Envelope::Delta`;
  signatures inside it verify as before, and the host decodes nothing new.
  Both halves keep at most 64 slots of at most 4 KiB per session. Deltas are
  never delayed for it. Clients dial `/7` first and fall back to `/6`; older
  sessions, replays and positions get the frames they always did.
- Packets, not frames (TIL-776): a 27 B presence batch costs about 35 B of
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

So on `tilo-relay/4` (`ALPN_SIGNED_ACTIONS_V1`) the sending device's
`RelayClient` signs every action it sends as itself
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

Older peers degrade explicitly. A client dials `tilo-relay/4` first and falls
back to `tilo-relay/3` only on `no_application_protocol`; there it cannot sign,
so current brokers serve it reads and refuse its effects. A host writes the
signed variants only to sessions that negotiated `tilo-relay/4` and gives an
older broker the unsigned frame it knows. Replies from the broker to the caller
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
