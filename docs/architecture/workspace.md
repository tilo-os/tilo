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

### Membership is a resource

A workspace's members, their roles, their grants and the devices each member
has in it are one resource of the workspace, `workspace.members`. Inviting,
admitting, changing a role, granting, removing and leaving are its actions.
Each is a signed op: its author's device key, certified by the author's root,
signs it, and the op names the ops it was written on (its causal heads), as
every op does.

Every participant replays the resource and checks every op under the action
policy against the membership **as the op's author saw it**: the state at the
op's own heads, not the receiver's current state. An op its author was not
granted at that point is kept, so replicas stay byte-identical, but has no
effect. No device decides for the others; every participant reaches the same
answer from the same ops.

**Concurrent changes merge by one rule.** Ops that do not depend on each
other are applied in a fixed order: the author's role first (owner, then
administrator, then the rest), then the op id. Each op is checked at its place
in that order, so a change that a concurrent, earlier-ordered op made
unauthorised has no effect. Two administrators removing each other: the one
ordered first wins, and the other's removal comes from someone no longer
granted. A role change racing a removal of the same member: the removal is
kept if it orders first, the role change is moot if it orders later.
Concurrent edits that do not conflict (two people invited at once) both
apply.

**Admitting.** An invitation is an op naming the role it grants and, when
narrowed, the person it is for. Redeeming it is the joiner's op, written on
the invitation's ops and signed by the joiner's device; any participant that
holds the invitation admits it, with no particular device needed online. A
member's own further devices need no invitation: a device certified by a
member's root is that member's device, and any participant admits it when it
first asks. Which role the device takes in the workspace (participant or
store) is the member's choice, recorded in the same resource.

### Keys follow membership

Content is sealed under content keys (`channel-crypto`), one per key epoch.
The key epoch's group is an MLS group whose members are exactly the
workspace's participant devices: a projection of `workspace.members`, never a
second record of who belongs.

- **Any participant commits.** When the membership resource admits a
  participant device or removes one, any participant that sees the group
  differ from the projection writes the MLS commit that brings it in line:
  `Add` with a `Welcome` and the bundle of every past content key for an
  admitted device, `Remove` and a fresh key epoch for a removed one. A
  participant checks every commit it receives against the projection at the
  commit's place and refuses one that adds or removes anyone the resource
  does not.
- **One commit per epoch.** MLS orders commits linearly. When two
  participants commit on the same epoch, every device keeps the commit with
  the lower hash; the other committer applies it and, if the group still
  differs from the projection, commits again on top. Commits only enact what
  the resource decided, so a lost race loses nothing.
- **Removal does not wait for a commit.** From the op that removes a member,
  every participant refuses that member's further ops, whatever key epoch
  they are sealed under. The commit that rotates the key comes from the next
  participant online, and protects content written from then on.
- **Late joiners read everything.** The bundle in an admission carries the
  key of every past epoch, sealed to the joiner's first epoch, so a joiner
  reads the whole history locally. It grows with membership changes, never
  with content.

A store never enters the group and never holds a key. It keeps and forwards
sealed ops, and refuses ones that are not signed by a member's device. To
tell, it reads the one part of `workspace.members` that is not sealed: the
device keys of the current members (no names, roles or grants), carried in
the signed envelope of each membership op.

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

**Continuing on another device.** Beside its windows, a device notes in its
part of the layout how each window it runs resumes (the app's small,
path-free resume state and the record it shows) and which window it is on.
Only the window's maker writes its note, and only a device's person its
focus. The person's other devices hold the layout already, so they offer to
continue there and open the window where it was left, with no channel of
their own. The note never holds a surface id, local path, file contents or
credentials.

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
- **Channel creation by the governor.** Only the governing device creates
  a channel's group; any participant admits and removes devices
  (`channel_crypto::ChannelPeer`). The browser follows and checks commits
  but writes none, since it does not hold record-level Read grants.
- **Per-person spaces.** The own room, the person room and the person's
  relay workspace (`personal_continuity_workspace_id`), each derived from the
  root key under its own domain string. The relay workspace is left only to
  the person's hosts: they keep person-room records on it for devices that
  are away, and ush environment sessions run on it. The person's devices
  already meet without it, on the session between two of them
  (`tilo-relay-sibling/1`), and learn the person's workspaces from the hosts
  holding them (`room.list`).
- **Settings outside the model.** `Settings<T>` with Personal, DeviceLocal and
  Shared scopes, the settings registry and `settings.json`. Settings are
  resources.
- **Engines outside the one peer.** The shell's own workspace engine and
  admission caller, the separately written browser peer with its own layout
  schema, and the device-replica wrappers in the shell and the standalone
  runtime.
- **Hosts as a certificate class.** `can_act_as_host` and the other host
  flags, a host's second identity, and the parallel ideas of where work runs.
  Role and appetite are per device and per workspace.
- **A second platform in the shell.** Its own widget host, frame pipeline,
  persistence, collaboration, history, undo and action authority. The shell
  uses the app stack.
