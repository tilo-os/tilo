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
record (`native_authority`: installed replicas and joins in flight) defines
which workspaces exist;
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
window needs its owner's control grant. In a personal room only a window
its person shares on purpose streams.

**Sharing one window in a personal room.** A member shares one of their
windows with a personal room through the room's resource share
(`tilo_action_policy::share`, the `resource.share` member action) of the
window's own node, `room › <app> › window › <tab>`, under the share id
`window.<tab>`; stopping is `resource.unshare`. The host writes it into the
room's governance as grants: who shared it (`granted_by`), which window (the
node) and until when (the grant's lifetime). It takes the share only from
the device the tab's topology namespace is registered to, so nobody shares
another device's window and the registry says which device hosts it
(`tilo_desktop_model::window_share`). Every other member shows it as a tab
under the sharer's tab id in their own layout, a window group at the end of
the workspace's last strip, marked with the sharer's presence while they
are here, and drops it when the share ends.

How that tab shows the window depends on what the window is. An outside
window (a terminal, any Wayland or X client) keeps its state in its process,
so the sharer streams that window, and no other, to the members viewing it
and to the room's hosts, and the tab is a `HostedWindow`, which the stream
names. A Tilo app's window is a view of a resource that already lives in the
room, so none of its pixels are streamed: its sharer also shares what it
shows, under the share id `window.<tab>:shows`, on the resource's own node
(the record the app says the window shows, at the item its bar anchors, or
the window's node again for the app's own record as a whole;
`window_share::shows_node`). Ending the window's share ends both. Every other
member's tab is a `PeerWindow` of the same app, which opens it on that
record (`WorkspaceTree::room_tab_record`) like any peer's window, and the
sharer's cursor and selection show in it as any collaborator's do. When the
share ends, a Tilo app's window the member used here (gave input to) stays,
as their own window under an id of their device's; one they left alone
closes.

The sharer reaches it on the window itself: its window menu and its bar's
menu offer "Share with <workspace>", and "Stop sharing" while shared, for a
window this device runs in a personal room only. While the room's record
holds the share, the window's bar says "Shared with <workspace>" with the
faces of the people there (`ui::widget::SharedWith`). A share outlives
neither its window nor its sharer's stay: a window this device closed or
moved out of the workspace has its share stopped, through the room's outbox,
which asks again on the host's next event until the room no longer holds the
share; leaving or deleting the workspace stops its shares before the room is
released. Every door makes the one
request `tab.share` / `tab.unshare` make (`overlay_host::window_share_controls`).

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

- **Governor**: the host that holds the room's governance (`StatePeer::new`
  under the host's `Rooms`): the room host every device runs in-process for
  the rooms placed on it, the same host `tilo-relay` runs, and the
  standalone runtime for its profile room.
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

1. **One engine in the shell.** Every room goes through `NativeWorkspaceOwner`;
   this device's native owner is a member of each, including the rooms its
   own host governs. `room_docs` is only a read-only cache of each room's
   admitted document, written by `WorkspaceTree::project_native_room`, which
   also keeps the room's admitted roster (`WorkspaceTree::room_peer_identities`).
   A governance change made here (a role, a grant, a rename) is asked of the
   room's governor (`GovernanceRequest`, taken by
   `network/governance_requests.rs`) and shows once the admitted document
   carries it; a sandbox capability taken back also narrows here at once.
   Window streams go to and are taken from the room's admitted roster, not a
   gossip topic; gossip carries only the person's own topic between their
   devices.
2. **Three layout schemas.** The room document's `contents` (native), the
   `so.tilo.browser-desktop/desktop` shared record (`shared_record.rs`,
   browser, relay, agent participant) and the device's whole-desktop
   document (`desktop_doc.rs`). In the browser one room holds many
   workspaces, so `Workspace::room` is not the room it replicates in.

## Migration

Each step lands on `main` on its own, green, and deletes the copy it
replaces. Pre-alpha: old paths are removed, not kept beside the new ones;
state of an older shape is refused and fetched again, not migrated.

1. **Done: the shell governs no room itself.** Every device runs the room
   host in-process, and a workspace made here is placed on it
   (`WorkspaceTree::create_hosted_workspace`, `pending_placements`). The
   shell's own engine went: `sync_room_doc`, `apply_room_changes`,
   `RoomAuthority` and the continuation pins, `ContentOwnership::Salted`,
   `network/replica.rs`, `network/hub.rs`, the `PeerInviteToken` format and
   the gossip topics of rooms. A state file from before loads unchanged; its
   rooms are no longer served by the shell's engine.
2. **Governance the host's `Rooms` has no action for yet.** Removing or
   banning a member, granting a sandbox capability in the room, and
   transferring ownership have no governor action, so a device cannot ask
   for them yet; `Rooms` publishing actions for them completes this.
3. **One governor per room** holds: the host that governs a room is its
   only governor, and a person's other devices are members with the owner's
   role.
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

4, 5 and 6 are independent of each other. `tilo-peer-state` is the room
core and `tilo-joined-room` its carriers; renaming them is mechanical and
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
