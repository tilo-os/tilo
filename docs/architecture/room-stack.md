# One room stack

A room is where a workspace's state lives: its governance, its members, the
records it holds and the signed operations that change them
([`rooms.md`](rooms.md), [`collaboration.md`](collaboration.md)). Today two
pieces of code decide what a room is, and they disagree. This document is
the plan to end with one, shared by the native shell, the Cloud host, the
other workspace hosts and the browser.

## What exists

### The shell's room engine

The native shell governs and follows rooms through its workspace tree and
network actor:

- **Governance.** `WorkspaceTree` (`crates/session/shell/src/shell/state.rs`)
  holds one room document per room (`room_docs`), a governance-writer pin per
  room kept outside the document (`room_authority`: `Local`,
  `Peer(origin)` or `Unpinned`), peer names and identities. Governed fields of
  the desktop-model `Workspace` are projected into the room document
  (`sync_room_doc`). Incoming changes are validated as a whole
  (`apply_room_changes`): declared room, media, coverage, the governance
  writer against the pin, owned contents, and per-entry seals on room
  records.
- **Members and admission.** In `crates/session/shell/src/shell/network.rs`:
  invitations (`issue_invite_for`, `redeem_invited_identity`), standing
  invitations with host-signed `HostAdmissionV1` for public rooms
  (`receive_room_admission`, `apply_room_admissions`), device delegates and
  `RecordingDelegationV1` (`publish_recording_delegations`,
  `apply_recording_delegations`), direct rooms between two people
  (`shell/community.rs::prepare_direct_room`), identity hellos and
  announcements.
- **Documents.** `document_role` / `peer_document_role` authorize app
  operations: author, role, channel class, payload route, the room index's
  comment shape, and the capability check.
- **Transport.** One gossip topic per room and relay sessions to the room's
  hosts (`network/actor.rs`), `room_topic_id`, retained checkpoints.

About ten thousand lines of this are room logic rather than shell logic.

### StatePeer

`crates/collab/tilo-peer-state` is a transport-agnostic admission state
machine per room: a governance document plus one document per record, with
the carrier's identity an explicit input (`TransportOrigin`). It is used by
the Cloud host, the workspace and device hosts, the browser peer, the relay's
retention proofs, and, through `crates/collab/tilo-joined-room`
(`NativeWorkspaceOwner`), by the shell itself for rooms a durable host
governs (`WorkspaceTree::requires_native_owner`, `project_native_room`).

It is strict where the shell's engine is loose: transactional admission,
exact checkpoint coverage, durable receipts, replay floors, sealed-history
custody without keys, bounded restore, a signed join and delegation protocol.
It lacks what the shell's engine has: it answers `UnsupportedFamily` to
`RoomAdmission`, `RecordingDelegation`, `IdentityAnnouncement` and MLS
governance (`state.rs` `receive_from`); it has no channel class; it admits
foreign people only through invitation redemption under the foreign-device
cap; its owner is always the authority's root; and it signs every local
publication `OpClass::Splice`.

### Why this is the problem

The shell already runs both engines, switching per room. The Cloud host and
the browser can only hold rooms StatePeer understands, so a Cloud user
cannot be in the Tilo Community room or their direct room with the Tilo team,
and cannot send feedback there ([`feedback.md`](feedback.md), "In the
browser"). Every feature added to one engine has to be added to the other or
works on one kind of device only. The shared pieces that do exist
(`tilo_peer_state::room::{governance_of, candidate, extract_governed_fields}`,
the desktop-model projection, the broker's `PeerPublishSink` and
`collab_inbound`) show where the two already meet, and the duplicated
`extract_governed_fields` in `state.rs` shows where they drifted.

## The two ways to one stack

**(a) Extend StatePeer** with public rooms, delegated host admission,
cross-root direct rooms, comment signing and recording delegations, and move
the shell's remaining rooms onto it. This ends with one stack only if the
shell's engine is then deleted; extending StatePeer alone leaves two, and
teaches StatePeer the shell's tricks without reconciling the models (the
shell's governance fields, pins and seals would be re-expressed a second
time).

**(b) Move the shell's governance down** into a shared crate and have
StatePeer converge into it. On its own this also leaves two: StatePeer is
already the shared engine every other host runs, so a second shared crate
beside it is a third stack until StatePeer is folded in.

Both end with one stack only when the two engines become one crate with one
model. The plan below does exactly that: **the room core is StatePeer's
admission machine, moved and renamed, with the shell's governance model
(the desktop-model `Workspace` vocabulary, channel classes, delegated
admissions, seals) as its governance.** Nothing survives beside it: the
shell's `room_docs` engine, `tilo-peer-state` and `tilo-joined-room` as
separate crates, and the shell's network-level admission code are removed.

What each costs, honestly:

| | (a) extend StatePeer | (b) shell governance down, StatePeer folded in |
|---|---|---|
| Ends with one stack | only after deleting the shell engine | only after folding StatePeer in |
| Model | StatePeer's strict admission, shell features re-expressed | shell's governance vocabulary, StatePeer's admission kept |
| Risk | duplicating shell semantics a second time | losing StatePeer's rigor if the shell's loose paths win |
| Churn in hosts | low (they already use StatePeer) | low if the move keeps StatePeer's API |

The plan takes StatePeer's API and admission rigor (so the hosts barely
change) and the shell's governance model (so nothing the shell does is
lost), which is the union of (a)'s low churn and (b)'s single model.

## The room core: `tilo-room`

One crate, `crates/collab/tilo-room`, formed by moving
`crates/collab/tilo-peer-state` and `crates/collab/tilo-joined-room` into it.
Its parts:

- **`room`**: the per-room state machine (today's `StatePeer`), transport
  agnostic, admission transactional, deferral on missing prerequisites.
- **`governance`**: the room document's schema and validation, one copy.
  It takes the shell's `apply_room_changes` rules (writer pin, declared room,
  owned contents, record seals, immutable channel class) and StatePeer's
  (`governance_change_allowed`, coverage, durability, receipts, media).
  The desktop-model `Workspace` and `projection` stay the vocabulary.
- **`members`**: who is in the room and with what role, computed from
  governance plus verified records held outside it: invitation redemptions,
  `HostAdmissionV1` from a delegated host, `WorkspaceDelegation`, device
  delegates, `RecordingDelegationV1`, identity hellos and announcements.
  StatePeer's `WorkspaceDelegation` and the relay's delegated host become one
  delegation.
- **`documents`**: admission of app operations: the envelope, route and
  attestation checks StatePeer does now, plus the channel class, comment
  records (`OpClass::Comment` on records the signer owns and the room
  index's comment shape) and the capability decision. Local publications are
  signed with the class the record requires.
- **`keys`**: who may hold a channel's key (today `shell/channel_policy.rs`).
  MLS itself stays in `channel-crypto` through the action broker; the room
  decides membership, the broker keeps the group. Public rooms keep no key.
- **`session`**: carrying a room: relay sessions (today
  `tilo-joined-room::session` and the shell actor's `HostRouteScheduler`),
  redemption at hosts, retention classification (one classifier, today split
  between `tilo_peer_state::transport` and `neighbour_wire::retention`).

Kinds of room, one model: a private room (governed, encrypted channels), a
public room (governed, plaintext, delegated host admission, standing
invitation), and a direct room (derived id, governed by its creating end, the
other person an Editor). The Tilo Community rules stay in `tilo-community`,
driving any host through `CommunityHost`.

## What each host implements

- **Storage** of a room's durable state (`tilo-joined-room`'s `Store`
  today): the shell in `state.json`, the Cloud host in a store root per room,
  the browser in its profile storage.
- **Carriers**: gossip topics and relay sessions in the shell, relay
  sessions in the Cloud host and workspace hosts, HTTPS delivery to the
  browser. Each hands the room `(origin, message)` and publishes what the
  room returns.
- **The apps boundary**: `PeerPublishSink` and `collab_inbound::deliver`,
  unchanged.
- **Presentation**: the shell projects rooms into its workspace tree (as
  `project_native_room` does now); the browser into its desktop admission.

## Order of work

Each step is its own pull request to main and leaves the shell working and
green. Pre-alpha: old paths are removed, not kept beside the new ones, and
stored state is not migrated.

1. **`tilo-community`**: the community configuration and the join, retry,
   refusal and leave rules, shared; the shell's `keep_community` calls it.
2. **`tilo-room` by move**: `tilo-peer-state` and `tilo-joined-room` become
   `tilo-room`; every user is updated; no behaviour change.
3. **The core learns the shell's model**: channel classes, delegated host
   admission, direct rooms, comment signing, recording delegations, identity
   announcements, record seals, one retention classifier, one
   `extract_governed_fields`. The shell's tests for these move with them.
4. **The shell on the core**: every room through the core; `room_docs`,
   `room_authority`, `apply_room_changes`, `document_role`,
   `peer_room_allows`, the admission and delegation code in `network.rs`
   and `network/replica.rs` are deleted; the shell's `CommunityHost` holds
   the community and direct rooms through the core. May split into local
   rooms first, then invited, public and direct rooms.
5. **Several rooms in the Cloud host and the browser**: a store root and a
   relay session per room, `tilo-community` keeping the Cloud user in the
   community with one-action leave, each room served over HTTPS delivery on
   its own origin, one browser peer per room.
6. **Cloud feedback sends**: Chat in the destination room posts the remark
   (a comment record in `#feedback`, a message in the team's room), and an
   end-to-end test drives it against a local relay and Cloud host.

Steps 2 and 3 are independent of the shell's move and of each other only in
part: 3 builds on 2. Step 5's Cloud-host plumbing can start once 3 has
landed, in parallel with 4.
