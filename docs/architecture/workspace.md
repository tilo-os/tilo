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

**A workspace's room never changes.** It is given when the workspace is made
(`Workspace::new` and `Workspace::create` mint one, `Workspace::in_room` takes
the room created or joined) and read with `Workspace::room`; nothing can set
it. A new workspace's local id is its room's own (`WorkspaceId::of_room`, the
transport shard Activity, the relay and custody derive), the same on every
device; a workspace stored before keeps its id, and an id a workspace no
longer carries resolves through the device's aliases
(`WorkspaceTree::workspace_aliases`). Action ids name the room
(`workspace.select.<room>`); the older `workspace.select.<number>` resolves
through the same aliases.

**The rooms a device holds are its workspace list.** The native owner's
record (`native_authority`: installed replicas and joins in flight) and, until
the shell's own engine goes, its rooms define which workspaces exist;
`desktop.workspaces` adds only the device's presentation. A join knows the
room it joins before it talks to the host (the door's and every invitation's
grant names it), so the workspace is made for that room at once and says
Joining… until the replica installs (`WorkspaceTree::join_native_room`); a
failed join keeps it, saying why, with Join again. Deleting or leaving a
host's room takes the workspace and the replica off together
(`native_workspace_owner::release_workspace_room`). Moving a workspace to the
person's home relay keeps its room: the relay takes over governing the same
room. A state file from before is brought to this on load
(`WorkspaceTree::bind_workspaces_to_rooms`), nothing deleted and no id
changed. A workspace can be archived on a device: kept whole with its
room, off that device's switcher and desktop, listed in workspace settings
to restore or delete (`WorkspaceTree::archived_workspaces`). Archiving is the
device's presentation, never the room's; the migration archives what nobody
showed there (a room held with no workspace, a placeholder that holds files).

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
unconfined programs (a terminal, any Wayland or X client) are shared like
every other window, by the room's scope. They have no resources to render
natively, so they cross as hosted on the device running them, and their
content goes to the members viewing them as a live stream of just that
window: a member's device that shows the tab tells its host so, to the host
alone (`SurfaceViewRequest`), and the host encodes the window only while
someone views it, damage-driven, with the hardware encoder where there is
one, and sends it to that viewer alone over the authenticated direct route,
end-to-end encrypted to its device. A member only watches; driving someone's
window needs its owner's control grant. In a personal room nothing streams.

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
retained baseline (`collab_types::DocumentFrame::retention`), and the
canonical workspace-join invitation.

Every room this session governs is now a `StatePeer` room in
`NativeWorkspaceOwner`: a new workspace is created as one
(`create_governed_workspace`, at the tick after its first frame), and a
state file from before has each such room created once when the owner opens
(`ShellNetwork::govern_awaiting_rooms`). That one creation imports what the
workspace already showed: its strips and windows in this device's topology
namespace, and the record index its apps referenced
(`tilo_peer_state::workspace_seed_with_layout`), so an upgrade keeps the
tabs and records the file held instead of replacing them with an empty
room. Until that one-time pass finishes for a room, `requires_native_owner`
is false and the older engine still writes it, so a gesture during the
import lands in the tree the seed is taken from; once every room found at
open has its native record (`native_rooms_ready`), every room this device
governs is native. An import that cannot be published leaves the workspace
and its windows in the tree, logs why and is retried; it never deletes a
tab or a record it could not carry. This is the ready window migration step
1 still has to delete: it is not claimed gone here.

A room with no workspace host is carried by `direct_carrier`: gossip
delivers the same `SyncResp` and `AppDocument` a relay session does,
admitted as `Input::direct` only after the transport authenticated a device
the room's current governance names. A direct invitation is a
workspace-join invitation issued by the governing device
(`ShellNetwork::issue_direct_invitation`) and redeemed through the same
signed exchange a relay answers; a person's other devices receive their
device-addressed invitation in the person's own workspace list. The
governor's join ledger is also the list, resend and cancel source for those
invitations (`pending_issued_invitations`, `issued_invitation`,
`cancel_invitation`), so the people sheet never reads a second local
inventory for a room this device governs. A member's direct carrier dials
the governor the room names *now*, not only the endpoint an older
invitation named, so a succession, a restart or a reconnect reaches the
current governor.

Retained requests to rearrange another device's windows are room state
(`layout_requests_v1`). The native owner drains the requests addressed to
its own salt, admits each against the exact revision its requester saw,
claims it in its own namespace (`layout_applied_v1`) under the same change
that publishes the rearranged entries, and never applies it twice; a
request it cannot apply yet stays retained. Only the device that owns a
salt may write its claims, and only the person who asked may add or remove
a request; both are refused by room admission.

What is still doubled:

1. **The older room engine remains, for owner-less sessions and joined
   rooms written by an older state file.** `room_docs` is now also the
   admitted-document cache a native room's publication writes (command
   admission and Activity read it), and `sync_room_doc`, `apply_room_changes`,
   `try_sync_room_doc`, `room_authority`, `name_peer`, `certify_peer_identity`,
   `redeem_invited_identity`, `peer_room_allows`, `network/replica.rs`, the
   `PeerInviteToken` format in `network/invite.rs` and the hello store in
   `network/identity.rs` remain for a session with no native owner at all
   (tests, an offscreen renderer), for a governed room during the one-time
   creation pass above, and for a *joined* room written by an older state
   file, which has no native record to reopen and cannot be governed from
   here. None of them writes a room this session governs natively once the
   pass is done, and no new invitation is minted in the old format.
   Removing the rest is the remaining work of migration step 1; this page
   does not claim it is done. A joined old-shape room is not read as an
   empty room by the native owner: it keeps its workspace and its last
   projection on the older engine, and re-joining its host fetches it
   again. There is no automatic migration for it.

Settled: **one layout schema.** The room document's `contents` is what the
native shell, the browser, the relay's operator strips and an agent
participant all write (`tilo_desktop_model::room_layout`): one workspace per
document, every device publishing only entries minted in its id namespace,
with tab-to-record bindings, authors, reads at any heads and retained
rearrangement requests in the same document. A page's workspace is the
room's (`WorkspaceId::of_room`), and it publishes its entries into the room's
own governance document as `SyncResp`; the room's `StatePeer` admits them
within the namespace the host registered to that device, then republishes the
one admitted document. The separate `so.tilo.browser-desktop/desktop` record
and the relay's `layout_guard` are gone: one admission decides every layout.
The device's own whole-desktop document (`desktop_doc.rs`) is not a copy of a
room's layout and goes with step 6.

## Migration

Each step lands on `main` on its own, green, and deletes the copy it
replaces. Pre-alpha: old paths are removed, not kept beside the new ones.
State of an older shape that this device governs is imported in place,
without dropping its tabs or record index; state it cannot import (a joined
room with no native record) is fetched again by joining its host, never
read as an empty room. Until a governed room's import finishes the older
engine still writes it; the native owner takes it over at once afterwards.

1. **The shell governs its own rooms with `StatePeer`.** The pieces are
   in place: the engine follows a revoked governor's root-named successor
   (`StatePeer::follow_successions`) and the device's owner keeps the
   successions with the room; the owner holds rooms this device governs
   beside joined ones (`NativeWorkspaceOwner::create_governed`, seeded by
   `tilo_peer_state::workspace_seed_with_layout`, no host custody of its
   writes); and the shell admits the windows it opens into such a room under
   its own registered namespace. New workspaces are created that way and the
   rooms a state file already held are imported on the owner when it opens,
   with their windows and record index. Steps 2 and 3 landed first, as they
   had to: a governed room is carried to the person's other devices and to
   an invited person. What remains is deleting `room_docs`' older governance
   path, `sync_room_doc`, `apply_room_changes`, `try_sync_room_doc` and
   `ContentOwnership::Salted` once no owner-less session or joined old-shape
   room needs them; a governed room and a room the owner holds already use
   neither.
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
   as members of host rooms do. The governing device issues each of their
   devices a workspace-join invitation in the person's workspace list; the
   device redeems it as an owner-role member. `RoomAuthority`,
   `room_authority`, `adopt_governed_peers` and the continuation pins go.
4. **One layout schema.** Done: the browser, the relay's operator strips and
   the agent participant write `contents` through `room_layout`, and the
   room's `StatePeer` admits each write by the one-writer-per-entry rule the
   topology registry states. The browser sends its part of the room document
   as `SyncResp` (a namespace claim first), not as a separate desktop record;
   `shared_record.rs`, the desktop-record layout route and `layout_guard` are
   gone. A workspace in the browser is the room's, one per room.
5. **Requests to rearrange another device's windows are retained room
   state** applied once by their owner, with the applied ids in its
   namespace. The schema (`room_layout::request`, `pending`, `claim`) and
   the native owner's drain/claim/apply have landed: a request waits while
   its owner is away and is applied once when it returns, in the same change
   that marks it. A command the owner's reducer refuses (for example a tab
   that has since gone) stays retained rather than being dropped. A page that
   moves another device's window asks instead of writing it; the relay judges
   the ask on that window's `moderation` node before it admits it, and the
   owner (the host's operator strips included) applies it once and records
   that it did. Closing another device's tab still runs through that owner's
   ordinary close lifecycle, not a second mutation here.
6. **The device desktop holds only what is the device's.** `desktop_doc`
   keeps the workspace list and presentation; Activity and Replay read
   the room documents' history for everything else.

Steps 2 and 3 come before workspaces move onto the engine (the rest of
step 1); 4, 5 and 6 are independent of them. When 3 has landed,
`tilo-peer-state` is the room core and `tilo-joined-room` its carriers;
renaming them is then mechanical and optional.

Status of steps 1–3 in this tree: the owner creates and governs every room
the shell owns and imports an existing store's layout and record index;
direct peers carry admitted `StatePeer` traffic over gossip, dial the
governor the room names now after a succession or restart, and a direct
invitation is the canonical workspace-join exchange, listed and cancelled
from the governor's ledger. A person's other devices hold their rooms as
owner-role members through the device-addressed invitation in the person's
workspace list. The shell's old governance/projection path still exists for
sessions with no native owner at all, for the one-time creation pass, and
for joined old-shape rooms (see above); it is not reached for a room this
session governs once that room's native record exists. Steps 4–6 are
unchanged.

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
