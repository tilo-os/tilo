# One workspace

A workspace is the one unit of collaboration. It is the same thing on the
native desktop, on a workspace host, in the standalone runtime and in the
browser. There is no second unit beside or under it: no room, no personal
space, no continuity lane.

A workspace has three parts:

1. **Layout**: its strips, the window groups on them and the apps open in
   them.
2. **People**: its members, their roles and grants, and how someone is
   invited and admitted.
3. **State and history**: the resources its apps keep, its tasks and
   shortcuts, and the history of all of these.

A person has as many workspaces as they like, and every one of them is on
every one of their devices. None of them is special. A new workspace starts
empty.

## Members and devices

**A member is a person's identity**: a certified human root
([`collaboration.md`](collaboration.md), "Identity"). Membership, roles,
grants and history name members, never devices.

**A member has devices.** Each device has its own key, certified by the root;
the device key is its iroh `EndpointId`. Pairing brings the identity to a new
device; any of the person's devices can pair another, and the devices that hold
the identity key approve it. Revocations travel in the same hellos as
certificates and win over them. Another person's device is never admitted
through pairing; it comes in through an invitation to a workspace, as that
person's device.

**Each device has a role in each workspace it holds:**

- **Participant**: it holds the workspace's keys, reads and writes its state,
  and can do work in it.
- **Store**: it holds the workspace's encrypted data and serves it, and can
  read none of it.

One device can be a participant in some workspaces and a store for others.
The two roles add up: a workspace can have any number of each.

**Each device decides how much it wants to do.** A server may want to store
and sync every resource and file of every workspace it holds. A home server
or workstation may also take its person's work: run actions on their behalf,
answer search, run agents. A laptop or phone hydrates on demand. What a device
offers depends on the device itself (power, battery, load, capacity,
availability) and changes as those change. When several devices offer the
same work, placement picks among the eligible ones by operator priority, then
power (external, charging, unknown, battery), then load as a fraction of
capacity, then latency, and finally the device key, so equal candidates
always resolve alike; a passed-over device carries its reason.

A host is not a different kind of node. "Workspace hosting" is a device with
a large appetite, usually always on. Anyone can run it, with the same
software and the same protocol as every other device.

## Transport

Devices talk over iroh, and nothing else. A device reaches the other devices
that hold a workspace, directly or through any device that stores it;
store-and-forward is what a store does. There is no separate relay protocol,
no second send path, and no gossip lane beside it.

## State

**All state is a CRDT-backed resource.** That includes settings and
preferences. An app's state is per (app, workspace): opening an app in a
workspace opens that workspace's state of it, and an app opened in a new
workspace starts empty. Within it an app keeps typed resources
(`#[derive(Model)]`, [`resources.md`](resources.md)) with stable ids; a
resource that outgrows one document is its own document, because an Automerge
change graph cannot be served in part.

**A resource exists once; workspaces reference it.** Bringing a resource into
a workspace adds a reference and moves no bytes; edits through either
workspace are edits to the one document. The reference grants access, and
removing it stops future changes from flowing; it cannot un-share what a
device already synced. "Duplicate here" is a different verb: it forks a new
resource with a provenance link. Bringing something in is a search result:
search lists resources from the person's other workspaces, labelled with
where they live, and activating one brings it here.

**A file is a resource whose identity is a path.** History, sharing and
permissions have one implementation, and the file case is not privileged.

## People

Membership and permissions are workspace state, changed by actions like any
other state and checked by the action policy
([`permissions.md`](permissions.md)). A participant with the grant admits a
member or a device and hands over the keys; there is no single device that
owns the workspace's governance.

**Late joiners get the full history.** A member or device added later reads
all of the workspace's existing state and history. Content keys are separate
from the keys of any one membership epoch; a joiner receives the past keys
with its admission and loads older history on demand, without any other
member being online. Removal stops future access; it cannot recall what was
already shared.

## Layout

**Layout across devices.** Each device writes only its own namespace of the
workspace's `contents`. Rearranging a window another device owns is a request
to that owner (`tilo_desktop_model::commands::DesktopCommand`), never a write
into its namespace. The request is workspace state: it is kept while the
owner is offline, applied once when the owner next holds the workspace, and
recorded in the owner's namespace so a restart neither drops nor repeats it.
Following someone (focus, scroll position) is presence, not state.

What one device decided for itself stays on that device: which strip and group
are focused, widths, maximized groups, outputs, local file paths. That is the
device's desktop, never a second copy of the workspace.

**Sharing a window.** A window of an outside program (a terminal, any Wayland
or X client) keeps its state in its process, so its device streams that window
alone to the members viewing it, end-to-end encrypted, only while someone
views it. A Tilo app's window is a view of a resource the workspace already
holds, so sharing it shares the resource and nothing is streamed: other
members open the same resource in the same app. Driving someone else's window
needs its owner's control grant.

## Presence

Presence is per resource, layout is per person. Inside a resource several
people have open, each sees the others' caret, selection, typing, pointer and
who is in it. Under "That I'm here" nothing else leaves the device: not which
windows or apps someone has, their desktop pointer, whom they follow, or what
the machine infers (an automatic status, idle or away). "Nothing" hides them
everywhere, inside resources too.

## History

A workspace's history is two levels: the outer level is actions on the
workspace itself (references added and removed, members joined and left,
grants changed, layout), the inner level is each resource's own history. The
workspace timeline shows a resource's edits as roll-ups ("Alice made 12
changes to Chapter 3") that expand into the resource's history, and filters
mean the same at both levels. Undo, Activity and Replay read those two levels;
nothing keeps a third log of the same changes.

## The shell

The shell's surfaces (launcher, top bar, overview, quick settings,
notifications, onboarding, sheets and dialogs) are apps on exactly the same
toolkit and resource/action/view model as every other app. The workspace,
its layout and the shell's own state are resources like any other. The
compositor stays thin: windows, input, outputs and scanout. Rendering
capabilities the shell needs (retained layers, damage tracking, interactive
animation) belong in the toolkit, so every app has them.

## Records that outgrow a snapshot

Chat keeps its messages in a chain of `ChatPart` records
(`tilo_app::framework::chain`), because one record outgrows what a snapshot
can carry: a snapshot folds a channel only up to 16 MiB, and a fold always
carries the record's whole history.

The chain stays in the SDK, as one resource. Segmenting one record's history
in the storage layer would need what Automerge does not have (a snapshot
without the history before it) and a compaction checkpoint every device
agrees on, and it would not help: a chat's current state grows with it, so a
newcomer would still load all of it. Every layer below already treats a
record as the unit and serves whole records on demand.

So the chain's consumers see one resource and never parts: `Chain::publish`
presents every held part as one resource, and the framework, not the app,
holds an earlier part when a reader scrolls back to it. Headless readers
(bots, moderation, the Discord bridge, agents) read and page the chain through
that resource instead of enumerating parts themselves.

## Where the code differs

The code still carries models this page does not have. Each goes, not beside
the model but instead of it:

- **Rooms.** `RoomId` and the room/workspace split, `(app, room, record)`
  routes, per-room app directories and the package document keyed by room.
  The workspace is the key.
- **Governance by one device.** The governor, the pinned governance writer,
  the separate governance document with its receipts, custody and succession,
  and the host-served `tilo.host` / `tilo.member` namespaces. Membership is
  workspace state changed by actions.
- **Stewards.** A device that alone admits devices to a channel, steward
  takeover and genesis epochs, in the broker, the host and the browser. Any
  participant with the grant admits.
- **Per-person spaces.** The own room, the person room and the personal
  continuity shard, each derived from the root key under its own domain
  string, and the gossip-only personal fabric lane.
- **Settings outside the model.** `Settings<T>` with Personal, DeviceLocal and
  Shared scopes, the settings registry and `settings.json`. Settings are
  resources.
- **Engines outside the one peer.** The shell's own workspace engine and
  admission caller, the separately written browser peer with its own layout
  schema, and the device-replica wrappers in the shell and the standalone
  runtime.
- **Two send paths.** Every message sent both to iroh-gossip and to host
  sessions. One iroh path.
- **Hosts as a certificate class.** `can_act_as_host` and the other host
  flags, a host's second identity, and the parallel ideas of where work runs.
  Role and appetite are per device and per workspace.
- **A second platform in the shell.** Its own widget host, frame pipeline,
  persistence, collaboration, history, undo and action authority. The shell
  uses the app stack.
