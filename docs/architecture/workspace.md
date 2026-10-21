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
| Sharing scope | `layout_v1` (`shared` or `personal`) | the governor |

Each record is its own document on its own `(app, room, record)` route
beside the room document, because an Automerge change graph cannot be served
in part ([`rooms.md`](rooms.md), "What it costs").

What one device decided for itself stays out of the room: which strip and
group are focused, widths, maximized groups, outputs, the paths of local
files, which windows are legacy windows hosted here. That is the device's
desktop (`DesktopTree` plus the shell's local fields), never a second copy of
the workspace.

**Layout across devices.** Each device, member or host writes only its own
namespace of `contents`, which the governor registers in
`contents_owners_v1` when it admits the device; a member publishes its own
strips and windows into the room, the governor does not commit them on its
behalf. Rearranging a window another device owns is a request to that
owner (`tilo_desktop_model::commands::DesktopCommand`), never a write into
its namespace. Such a request is room state like any other: the host
retains it while the owner is offline, the owner applies each request once
when it next holds the room, and it records which it applied in its own
namespace so a restart neither drops nor repeats one. A window someone moved
therefore stays where they put it instead of snapping back when its owner
returns. Following someone (focus, scroll position) is presence, not room
state.

**Sharing scope is room policy.** The room's governance says whether it
shares its members' layout (`tilo_desktop_model::projection::RoomLayout`).
In a *shared* room (the default, an invite-only room's) layout crosses
devices as above. In a *personal* room each member's strips and windows stay
on their device; only its records and its people are shared. Its governor
admits no member's change that puts anything into `contents` (a member may
only take its own out), the relay's doors refuse members' layout writes, and
devices send none and mirror none. A room admitted through a public door (a
community site's persistent room, `tilo-relay room --layout`, personal
unless its host says otherwise) or with open admission (listed in a public
directory, unless its governor chose) is personal. Windows of host or
unconfined programs reach only rooms whose every member is the same person
(their own devices, continuation), as hosted on the device running them so
their other devices view them from there; never a room with another person
or an agent in it, whatever its scope, and they leave such a room the moment
someone joins. In a shared workspace their bar says "Only on this device".

**Presence is per resource, layout is per person.** Inside a resource
several people have open (a note, a chat channel) each sees the others'
caret, selection, typing, pointer and who is in it: that is collaboration on
the document, in shared and personal rooms alike. Under "That I'm here" (a
public room's default) nothing else leaves the device: not which windows,
tabs or apps someone has, their desktop pointer, whom they follow, what they
say beside their pointer, or what the machine infers (an automatic status,
idle or away from a quiet or locked screen). "Nothing" hides them
everywhere, inside resources too.

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

Shared already: the types (the shell's `WorkspaceTree` wraps the desktop
model's `DesktopTree` rather than copying it), room admission (every
receiver runs `tilo_peer_state::room::admit_room_changes`, and the
retained-proof verifier its `check_candidate`), the reading of an admitted
room into its workspace (`tilo_peer_state::room::adopt_room`, on the shell's
two paths and in the runtime), the rule for which document frame starts a
retained baseline (`collab_types::DocumentFrame::retention`), and one
invitation format besides the shell's own. What is still doubled:

1. **Two engines in the shell.** Rooms a host governs go through
   `NativeWorkspaceOwner` (`WorkspaceTree::requires_native_owner`). Every
   other room (one this device minted, one joined with a direct invite, a
   person's own and continuation rooms) goes through the shell's own engine:
   `room_docs`, `room_authority`, `sync_room_doc`, `apply_room_changes`,
   `name_peer`, `certify_peer_identity`, `redeem_invited_identity`,
   `peer_room_allows`, `network/replica.rs`, the `PeerInviteToken` format in
   `network/invite.rs` and the hello store in `network/identity.rs`, about
   6,000 lines plus their tests. It re-implements what `StatePeer` does:
   identity rows (`GovernedPeerIdentity` beside `GovernedIdentity`),
   redemption (`redeem_invited_identity` beside `admit_invited_member`),
   membership writes, media binding (`media_authority_admitted` beside
   `validate_media_bindings`) and sender authorization (`peer_room_allows`
   beside `StatePeer::permissions`).
2. **Three layout schemas.** The room document's `contents` (native), the
   `so.tilo.browser-desktop/desktop` shared record (`shared_record.rs`,
   browser, relay, agent participant) and the device's whole-desktop
   document (`desktop_doc.rs`). In the browser one room holds many
   workspaces, so `Workspace::room` is not the room it replicates in.

## Migration

Each step lands on `main` on its own, green, and deletes the copy it
replaces. Pre-alpha: old paths are removed, not kept beside the new ones;
state of an older shape is refused and fetched again, not migrated.

1. **The shell governs its own rooms with `StatePeer`.** The pieces are
   in place: the engine follows a revoked governor's root-named successor
   (`StatePeer::follow_successions`) and the device's owner keeps the
   successions with the room; the owner holds rooms this device governs
   beside joined ones (`NativeWorkspaceOwner::create_governed`, seeded by
   `tilo_peer_state::workspace_seed`, no host custody of its writes); and
   the shell admits the windows it opens into such a room under its own
   registered namespace. What remains is creating new workspaces that way,
   then moving existing ones, after which `room_docs`, `sync_room_doc`,
   `apply_room_changes`, `try_sync_room_doc` and `ContentOwnership::Salted`
   go. Doing so before steps 2 and 3 would cost two things the shell's own
   engine does today: a person's other devices hold their own workspaces as
   co-governors over gossip, and a room is shared without a home relay with
   a direct invitation. So new workspaces move once a governed room can be
   carried to the person's other devices and to an invited person.
2. **Direct peers on the same engine.** Gossip becomes a carrier for
   `StatePeer` (`SyncResp` and `AppDocument` from an authenticated device,
   like a relay delta). A direct invitation is a workspace-join invitation
   redeemed by the governing device. `network/invite.rs`
   (`PeerInviteToken`), `redeem_invited_identity`, `name_peer`,
   `certify_peer_identity`, `GovernedPeerIdentity`, `peer_room_allows`,
   `room_gate.rs`'s legacy table and `network/replica.rs` go. (The shell's
   identity rows are persisted and compared byte for byte by every peer, so
   they change with this step rather than on their own.)
3. **One governor per room, also for a person's own rooms.** A person's own
   and continuation rooms are governed by the device that made them (or the
   person's home relay once moved there); their other devices are members
   with the owner's role and change governance through governor actions,
   as members of host rooms do. `RoomAuthority`, `room_authority`,
   `adopt_governed_peers` and the continuation pins go.
4. **One layout schema.** The browser, the relay's operator strips and the
   agent participant write `contents`; `shared_record.rs` and the relay's
   `desktop_guard` go (the topology registry decides who moves what). A new
   workspace in the browser is a new room.
5. **Requests to rearrange another device's windows are retained room
   state** applied once by their owner, with the applied ids in its
   namespace. Today a device may move and close only the windows it
   opened; this lets a person arrange the windows of their other devices,
   offline ones included.
6. **The device desktop holds only what is the device's.** `desktop_doc`
   keeps the workspace list and presentation; Activity and Replay read
   the room documents' history for everything else.

Steps 2 and 3 come before workspaces move onto the engine (the rest of
step 1); 4, 5 and 6 are independent of them. When 3 has landed,
`tilo-peer-state` is the room core and `tilo-joined-room` its carriers;
renaming them is then mechanical and optional.

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
