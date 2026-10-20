# One workspace

A workspace is one thing everywhere: on the native desktop, on a workspace
host, in the standalone runtime and in the browser. It has three parts, and
all three live in one room:

1. **Layout**: its strips, the window groups on them and the apps open in
   them.
2. **People**: its collaborators, their roles and grants, how someone is
   invited and admitted, and who may hold its channel keys.
3. **State and history**: the records its apps keep, its tasks, receipts and
   shortcuts, and the history of all of these.

This page is the model and the plan for getting the code to match it. The
reasoning behind rooms and records is in [`rooms.md`](rooms.md), what is
checked on admission and why in [`collaboration.md`](collaboration.md).

## The model

**A workspace is a room.** `tilo_desktop_model::Workspace` is the type and
`Workspace::room` its identity: a workspace that exists is a room that
exists, including a person's own. Nothing else names a workspace across
devices.

**A room is one admitted document plus the records it references.** The room
document (schema: `tilo_desktop_model::projection`) carries:

| Part | Keys | Who writes |
|---|---|---|
| Layout | `contents` (strips, groups, tabs), `contents_owners_v1`, `desktop_counter_v1` | each device its own namespace |
| People | `owner`, `members`, `members_joined_v1`, `grants`, `sandbox`, `peer_identities_v1`, `invitation_receipts_v1`, `media_policy_v1`, bots and delegates | the governor |
| State | `records` (the record index), `tasks_v1`, `effect_receipts_v1`, `automations_v1`, `schedule_claims_v1`, `presets_v1`, `name` | members, under the room's grants |
| Custody | checkpoint coverage, durable receipts, import proposals | the governor |

Each record is its own document on its own `(app, room, record)` route
beside the room document, because an Automerge change graph cannot be served
in part ([`rooms.md`](rooms.md), "What it costs").

What one device decided for itself stays out of the room: which strip and
group are focused, widths, maximized groups, outputs, the paths of local
files, which windows are legacy windows hosted here. That is the device's
desktop (`DesktopTree` plus the shell's local fields), never a second copy of
the workspace.

**History nests.** A workspace's history is the room document's history
(layout, people, tasks, receipts) as the outer level and each record's own
history as the inner one ([`rooms.md`](rooms.md), "History nests"). Undo,
Activity and Replay read those two levels; nothing keeps a third log of the
same changes.

**One engine decides what a room is.** `tilo_peer_state::StatePeer` admits
every change to a room, on every host, in one of two roles:

- **Governor**: the device or host that holds the room's governance
  (`StatePeer::new`). Today a workspace host (`tilo-relay`), the room
  service, the standalone runtime for its profile room.
- **Member**: a device that holds a replica (`StatePeer::joined`, kept by
  `tilo_joined_room::NativeWorkspaceOwner`).

The engine is transport agnostic: a carrier hands it `(origin, message)` and
sends what it returns. Carriers are the relay session (Iroh or HTTPS), HTTPS
delivery to the browser and, for direct peers, gossip.

Invitations are `tilo-workspace-join` invitations, redeemed into the room by
`governance::admit_invited_member`. MLS stays in `channel-crypto` behind the
action broker: the room decides who may hold a channel's key
(`StatePeer::governed_access`), the broker keeps the group.

**One projection.** Every host reads an admitted room into a `Workspace`
through the same functions, and the browser, the relay and the shell write
layout through the same `contents` schema.

## Where the code is today

The types are already shared: the shell's `WorkspaceTree` wraps the
desktop model's `DesktopTree` rather than copying it, and both engines
admit a room document through `tilo_peer_state::room::admit_room_changes`.
What is still doubled:

1. **Two engines in the shell.** Rooms a host governs go through
   `NativeWorkspaceOwner` (`WorkspaceTree::requires_native_owner`). Every
   other room (one this device minted, one joined with a direct invite, a
   person's own and continuation rooms) goes through the shell's own engine:
   `room_docs`, `room_authority`, `sync_room_doc`, `apply_room_changes`,
   `name_peer`, `certify_peer_identity`, `redeem_invited_identity`,
   `peer_room_allows`, `network/replica.rs`, the `PeerInviteToken` format in
   `network/invite.rs` and the hello store in `network/identity.rs`, about
   6,000 lines plus their tests. The shell's engine re-implements what
   `StatePeer` does: identity rows (`GovernedPeerIdentity` beside
   `GovernedIdentity`), redemption (`redeem_invited_identity` beside
   `admit_invited_member`), membership writes, receipt immutability, media
   binding (`media_authority_admitted` beside `validate_media_bindings`) and
   sender authorization (`peer_room_allows` beside `StatePeer::permissions`).
2. **Three layout schemas.** The room document's `contents` (native), the
   `so.tilo.browser-desktop/desktop` shared record (`shared_record.rs`,
   browser, relay, agent participant) and the device's whole-desktop
   document (`desktop_doc.rs`). In the browser one room holds many
   workspaces, so `Workspace::room` is not the room it replicates in.
3. **Three projections of an admitted room into a `Workspace`**: the shell's
   `project_native_room` and `apply_room_changes`, and the runtime's
   `standalone::joined::current_room`.
4. **Copies of shared rules**: `tilo-relay-shell`'s retained proof runs the
   room admission steps one by one instead of calling them; the browser
   peer keeps its own checkpoint-retention rule beside `neighbour_wire::retention`.
5. **Three invitation formats**: workspace-join invitations, the shell's
   `PeerInviteToken`, and an attenuable `collab_types::invite::InviteToken`
   nothing uses.

## Migration

Each step lands on `main` on its own, green, and deletes the copy it
replaces. Pre-alpha: old paths are removed, not kept beside the new ones;
state of an older shape is refused and fetched again, not migrated.

1. **Unused invitation format.** Delete `collab_types::invite` and its fuzz
   target.
2. **One admission body.** `admit_room_changes` is split into building the
   candidate and checking it; the retained proof calls the check instead of
   repeating it.
3. **One retention rule.** The browser peer asks `neighbour_wire::retention`
   whether a frame is a checkpoint.
4. **One projection.** One function reads an admitted room document into a
   `Workspace`'s governed fields; the shell's two paths and the runtime use
   it.
5. **One identity row.** The shell reads and writes `peer_identities_v1`
   through `GovernedIdentity` and `room::identity_row`;
   `GovernedPeerIdentity` and `peer_identities_of` go.
6. **The shell governs its own rooms with `StatePeer`.** A room this device
   mints is a governed `StatePeer` in the shell's owner store, as the
   runtime's profile room is; layout is written under the `contents` registry
   instead of salted ownership. `requires_native_owner` is true for every
   room, and `room_docs`, `sync_room_doc`, `apply_room_changes`,
   `try_sync_room_doc` and `ContentOwnership::Salted` go.
7. **Direct peers on the same engine.** Gossip becomes a carrier for
   `StatePeer` (`SyncResp` and `AppDocument` from an authenticated device,
   like a relay delta). A direct invitation is a workspace-join invitation
   redeemed by the governing device. `network/invite.rs`
   (`PeerInviteToken`), `redeem_invited_identity`, `name_peer`,
   `certify_peer_identity`, `peer_room_allows`, `room_gate.rs`'s legacy
   table and `network/replica.rs` go.
8. **One governor per room, also for a person's own rooms.** A person's own
   and continuation rooms are governed by the device that made them (or the
   person's home relay once moved there); their other devices are members
   with the owner's role and change governance through governor actions,
   as members of host rooms do. `RoomAuthority`, `room_authority`,
   `adopt_governed_peers` and the continuation pins go.
9. **One layout schema.** The browser, the relay's operator strips and the
   agent participant write `contents`; `shared_record.rs` and the relay's
   `desktop_guard` go (the topology registry decides who moves what). A new
   workspace in the browser is a new room.
10. **The device desktop holds only what is the device's.** `desktop_doc`
    keeps the workspace list and presentation; Activity and Replay read
    the room documents' history for everything else.

Steps 1 to 5 are independent of each other. 6 comes before 7 and 8; 9 and
10 need only 4. When 8 has landed, `tilo-peer-state` is the room core and
`tilo-joined-room` its carriers; renaming them is then mechanical and
optional.

## Records that outgrow a snapshot

Chat keeps its messages in a chain of `ChatPart` records
(`tilo_app::framework::chain`), because one record outgrows what a
snapshot can carry: the relay folds a channel into one snapshot only up to
16 MiB, and a fold always carries the record's whole history. The chain is
an app-level segmentation, and the question is where it belongs.

It stays in the SDK, as one resource. Segmenting one record's history in
the storage layer would need what Automerge does not have (a snapshot
without the history before it) and a compaction checkpoint every peer
agrees on, and it would not help: a chat's current state grows with it, so
a newcomer would still load all of it. Every layer below (the engine's
per-route documents, coverage, the relay's folds, sealed compaction, the
transfer limits) already treats a record as the unit, and serves whole
records on demand.

So the chain's consumers see one resource and never parts:
`Chain::publish` already presents every held part as one resource, and the
framework, not the app, holds an earlier part when a reader scrolls or
pages back to it. Headless readers (bots, moderation, the Discord bridge,
agents) read and page the chain through that resource instead of
enumerating parts themselves.
