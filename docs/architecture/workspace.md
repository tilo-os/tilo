# One workspace

A workspace is the one unit of collaboration. It is the same thing on the
native desktop, on a workspace host, in the standalone runtime and in the
browser. There is no second unit beside or under it: no personal space, no
continuity lane.

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
(`crates/collab/tilo-user-identity`). Membership, roles,
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

### A machine copied from another

A person sets up one machine and branches others off it by copying its disk
(`tilo-vm vm branch`). The copy is a **new device of the same person**, never
a second instance of the first one: two machines with one device key would be
one iroh endpoint, one CRDT author and one filesystem subject.

The device key belongs to a machine. Beside it, `device-private/machine.bin`
records the firmware's system UUID (`/sys/class/dmi/id/product_uuid`), which a
VM host sets per machine and keeps across that machine's reboots;
`/etc/machine-id` is on the disk, so a copy carries it and it proves nothing.
At every boot, before the filesystem, the relay, the network, SSH or a login
read anything, `tilo-device-branch.service` compares the two
(`tilo_user_identity::branch`):

- the recorded machine: nothing happens;
- nothing recorded (an install from before the record, or the first boot):
  the machine is recorded, nothing else. A copy of a machine that never booted
  a build with this unit cannot be told from its source this way, and both
  keep the one key: **boot the source once on such a build before branching
  it**;
- no firmware UUID (some hardware, any container): never re-keyed
  automatically;
- another machine: this disk is a branch, and it re-keys.

The re-key writes a root-only journal first (the keys the machine's stores may
be under, the new key, the machine it is for), and every later step can run
again, so a boot stopped anywhere resumes at the next one:

1. Every store sealed to the device key is re-sealed to the new key. Today that
   is each person's Secret Service vault, re-sealed by their own Secret
   Service running as them (`tilo-secret-service reseal-device`, keys on its
   standard input); root opens nothing in a home. Everything else a person
   keeps sealed — the accounts and passwords vault, model provider keys, Wi-Fi
   passwords, the MLS master key, the identity's own secret — has its key in
   that vault, so it opens as before. A vault under neither key (another
   machine's) is left untouched.
2. The new key replaces the device key.
3. The device is certified (below), and every certificate for the old key is
   removed from this machine.
4. A fresh `/etc/machine-id`; SSH host keys are removed and `sshd-keygen`
   makes new ones.
5. The machine is recorded and the journal, with the old key, deleted. A store
   that could not be re-sealed keeps the journal until a later boot re-seals
   it; the device acts with its new key meanwhile.

State that names the old device itself, rather than being sealed to its key,
cannot be re-sealed into the new one's, so it is dropped and rebuilt from the
device's peers:

- **Channel membership.** Each MLS group the copy holds names the source's
  device as its member. The relay's channel member files (sealed under a key
  its device key derives) are dropped at re-key; a session's (sealed under the
  person's own vault) are dropped where the channel service finds them naming
  another device (`channel_crypto::Error::OtherDevice`). Neither is ever used,
  so the branch takes no leaf of its source's and cannot act as it. The
  branch then asks for each channel as itself, and a participant admits it
  with a `Welcome` and the key history, as for any further device of a member.
  The source stays the member it was.
- **Workspace membership.** The workspaces on the copied desk are held as the
  source's device: a workspace whose governance holds no certificate for this
  device is entered at its host as the person's further device — at the host
  the person's list names, or for one joined at someone's site, the host its
  own state names — and stays on the desk meanwhile, also when an entry does
  not land. The membership the host answers replaces the source's enrollment
  the copy held, which was never this device's to act with.
- **The person's own store.** The store the source kept is set aside beside it
  (`documents.of-<source>`), and the person's records come back from their
  other devices.

The old device is **never revoked**: the machine it was copied from still is
it.

**What a branch keeps**: the person, everything on the disk, every secret, and
its workspaces. Membership names people, and a device certified by a member's
root is that member's device, admitted by any participant when it first asks
(§ Membership is a resource), so no workspace has to invite the branch.
**What it gets new**: its device key (so its endpoint id, CRDT author and
filesystem subject), its certificate, its name — the source's name with
`branch` and four hex digits of the new key, renamed like any device — its
machine id and its SSH host keys. A setting such as accepting agents from the
person's other devices is on the disk and comes along.

**Certification adds no trust.** Only the person's root signs a device
certificate:

- A machine whose system identity tree holds the root
  (`user-identity/privkey.bin`) certifies its branch at boot, granting exactly
  what the source's certificate granted, and no expiry the source did not
  have. It could sign any certificate for that person already.
- A person whose root is in their own identity record (the usual primary
  device) has it issued at their next sign-in, the same provisioning that
  issues a certificate to any of their devices that holds none; it carries the
  branch's name (`device/branch-label`).
- A device that joined without the root holds nothing that signs. It waits,
  uncertified, for one approval: `tilo identity pair` on the branch (its name
  is the default) and `tilo identity approve` on a device that holds the root,
  as any personal device joins. Its secrets stay usable meanwhile: the stores
  are sealed to the device key, not to its certificate. Nothing lets a device
  key, or a copy of one, certify another key on its own: a stolen device key
  revoked from the person would otherwise mint unrevoked successors.

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
(`crates/collab/action-policy`). A participant with the grant admits a
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

**The log starts from what the workspace already held.** A workspace made
before the log has its membership in the document's own rows; its owner's
device writes them in as the first op, a genesis naming the membership those
rows held and the heads it was read at. Every receiver checks the heads are
history it holds and the author is the workspace's owner, so nobody can take
the membership back to an earlier one. A genesis by anyone else's device, or
naming a history a receiver does not hold, starts nothing.

The rows stay, as a projection: every admission writes them from the ops it
holds, and refuses a batch that changes a row the ops do not change. An older
build, a store and a relay that keep only the document therefore read the
same membership every device computes, and the log is what decides it. A
batch may only add ops; a held op is never changed or dropped.

**Admitting.** An invitation is a signed grant naming the role it gives and,
when narrowed, the person it is for. Redeeming it writes the receipt, with
its signed invitation, into the workspace's rows; every participant folds the
receipts it holds into the membership, checking the invitation at the joining
time the workspace records for that person, so the invitation is judged once
and every participant judges it the same. Any participant that holds the
invitation admits it, with no particular device needed online. A member's own
further devices need no invitation: a device certified by a member's root is
that member's device, and any participant admits it when it first asks. Which
role the device takes in the workspace (participant or store) is the member's
choice, recorded in the same resource.

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

**One group per reader set.** A group belongs to a key scope, not to a
record: `workspace_scope(workspace)` is one MLS group and one content keyring
for every storage unit whose readers are the workspace's participants. A unit
— one record's channel — seals under its scope's keys while its payload still
names the unit, so content stays bound to the record and artifact it was
written for and every stored and wire format is unchanged. Membership is
still decided per record: a Read Deny on a record, on its app or on a pattern
over it narrows that unit's readers, and the unit is then carried by a group
of its own whose key no denied device ever gets.

**One genesis per channel, created once.** A group starts from one genesis,
and whoever holds its state and sees none creates it; everyone else waits,
asks, and joins by the Welcome that admits it — a page included, which never
creates a group it was not admitted to. The page learns which group carries a
unit from its replica of the workspace's governance, the same decision a
device makes: the workspace's scope, read by the workspace's participants, or
the unit's own group when a Read Deny narrows it. A scope's request names no
record — every participant may read it — while a narrowed unit's request names
the record, so its participants can decide Read on it. Two geneses made at
once reconcile like two commits: every device keeps the group whose genesis
has the lower digest, and a device in the other starts over and joins the
winner by Welcome, whose bundle brings the keys its writes were sealed under.
Learning it needs the scope's frame only: a page that held a per-record group
for the unit leaves it unused, for no participant seals under it any more, and
the scope's Welcome carries that group's older content keys along. A record's
cleartext writes stay the base its sealed writes are changes over: a device
joining after the seal takes that base in from the host's replay, while a live
cleartext write is refused.

What a scope buys is that membership costs what the *reader set* costs, not
what the workspace holds: one admission, one removal and one commit per scope,
however many units it carries. Before scopes, every record had a group, so a
workspace of a thousand records paid about two seconds and 3 MiB of
governance frames to admit one device, and repeated it on every change, on
every device.

Units sealed before scopes keep their groups. A device that holds one reads
its history as before, merging that group's keys with the scope's; new
content for those units seals under the scope, so a device has to hold both.
The holder of an old group hands its content keys to the scope — the scope's
next commit carries them to every member, and every admission after that
passes them on in the joiner's key history — so a device that never held the
old group reads the unit's whole history as soon as the scope has them, with
no holder of the old group online. A unit a Read Deny narrows keeps a group
of its own and is never handed to the broader scope.

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

## Branching a workspace

A person sets a workspace up once — a research environment, a development
workspace for a product — and branches it: one branch per question, one per
customer. A branch is a **new workspace of the person's own** that starts with
everything the source holds and goes on apart from it. It never merges back;
code comes back through git, not through Tilo.

One verb makes it, on the desk's chrome beside `new-workspace`:
`desktop:chrome` `branch-workspace` {`source`, `name`}, where `source` is the
workspace's name, its decimal id or the id it is shared under. The desk,
agents and automations use that action; the workspace menu and search offer it
as "Branch workspace", and the CLI is `tilo workspace branch <source> <name>`.
Tools that spell the workspace verbs `workspace.create` and `workspace.branch`
on the control socket reach the same two actions. The action answers the new
workspace's id at once; the copy runs off the session's loop, so the desk never
waits for it, and the branch goes on the desk — with the person in it — once
what it holds is there, so no app opens in it before its records are and founds
an empty record of its own. The person is told when it is ready, and what did
not come along if anything did not.

**Who may branch.** Whoever reads the whole workspace: this device holds it and
its governance names the person as its owner or as a member not removed
(a workspace the device holds no governance of is the desk's own, and the
person's unless it names someone else). A record sealed to readers that exclude
this device is one it cannot read, so it is not branched. An agent may branch
only the workspace it works in, and only as the person's grants let it take
the desk's actions; a branch of another workspace is refused at the agent door.

**What a branch copies**

- **Records, with their history.** Every record the source holds on this
  device is read whole and written into the branch as its founding write,
  sealed under the branch's own channel keys. Each document is forked under a
  writer of its own and gets one more change naming the branch as its
  workspace; its record id stays (an id names a document only inside its
  workspace, so references between the workspace's records keep working) and
  so does where it was minted. A record an app also keeps on this device has
  that copy's changes merged in first, and the branch's local copy is the very
  document the branch's engine holds. No document is shared between the two
  workspaces: an edit in either stays there, also after a restart.
- **What the apps keep for the workspace on this device**
  (`tilo-workspaces/<workspace>/<app>`): every file, with each document in it forked
  the same way and without the source's writer id, locks or load hints.
- **The workspace's own files** (`.workspaces/<workspace>`), into the branch's
  tree, each file sharing its source's extents where the file system can
  (`FICLONE`) and copied where it cannot (`tilo_files::native::tree`), so a
  large tree on Btrfs or XFS is a matter of seconds and no space.
- **Terminal environments.** The runtime that keeps them branches them
  (`ush`'s `branch-workspace`): each record forked with its history, naming the
  branch as its workspace, its home and its grants of the source's files moved
  to the same places in the branch's, and what it keeps outside its record (its
  `/var` and legacy home) copied. Its id (`TILO_ENVIRONMENT`, `<branch>/<key>`)
  and its `TILO_WORKSPACE` are the branch's, so its sessions and its agent door
  act in the branch only. File actions not carried out yet stay with the
  source. Images are the device's and shared already.
- **The desk.** The source's strips, a window for each of this device's
  windows in it (opened again by its app, in the branch), and what the
  workspace grants its apps, its own files named as the branch's. The branch's
  shared desk record is written from these windows, never copied from the
  source's record of everyone's.
- Secrets (the Secret Service, the passwords vault, provider keys) are the
  person's on the device, not the workspace's: a branch on the same device has
  them already.

**What stays behind** — what is tied to other people:

- Chat: every conversation, with each person's drafts and read marks. The
  workspace's own emoji and its agents, which Chat keeps for the workspace,
  come along.
- Each person's own place in the workspace's tilolang apps, one record for all
  of them.
- Membership and governance: the branch's are new — a new workspace id, its
  only member the person who branched it, as owner, its own channel keys. No
  other member, invitation, pending invitation or channel state carries over.
- Other people's windows, who is here (presence), and the source's history of
  effects and its shortcuts.

The source is read and never changed.

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

## What a unit and a key scope cost

Measured, not estimated: `channel-crypto`'s `per_channel` bench for a key
scope, `tilo-app-state`'s `record_cost` for a unit and its document, and
`tilolang-tilo`'s `index_units` for the index. Numbers are medians on a
x86_64 development container, three samples; instruction counts repeat on any
machine, times are that container's.

**One group per channel** (one MLS group per record route, three devices in
each channel), per channel — the cost a workspace paid before key scopes:

| channels | join | per channel | join bytes | removal | removal bytes | device state |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 1.99 ms | 1.99 ms | 3.1 KiB | 0.84 ms | 700 B | 757 B |
| 10 | 22.0 ms | 2.20 ms | 3.1 KiB | 0.77 ms | 700 B | 763 B |
| 100 | 197 ms | 1.97 ms | 3.1 KiB | 0.75 ms | 700 B | 760 B |
| 1000 | 1989 ms | 1.99 ms | 3.1 KiB | 0.77 ms | 700 B | 760 B |

The cost is flat per group and does not shrink as a workspace grows: 2.0 ms
and 3.1 KiB of governance frames per channel to admit one device, 0.8 ms and
700 B per channel for one removal, and 760 B of sealed peer state per channel
on the device. A user joining a workspace of 1000 records paid about two
seconds and 3 MiB before reading anything, and every membership change after
that repeated it — once per channel, on every device.

**One key scope per reader set** (one MLS group carrying every unit whose
readers are the same devices, three devices), by the units it carries:

| units | join | per unit | join bytes | removal | removal bytes |
| --- | --- | --- | --- | --- | --- |
| 1 | 2.02 ms | 2.02 ms | 3.1 KiB | 0.97 ms | 700 B |
| 10 | 1.95 ms | 195 us | 3.1 KiB | 0.77 ms | 700 B |
| 100 | 1.87 ms | 18.7 us | 3.1 KiB | 0.74 ms | 700 B |
| 1000 | 1.88 ms | 1.9 us | 3.1 KiB | 0.76 ms | 700 B |

Flat in the units carried: a workspace of 1000 records costs 1.9 ms and 3.1
KiB to admit a device and 0.76 ms and 700 B to remove one, instead of 1.99 s
and 3.1 MiB and 0.77 s and 700 KiB. Each unit's content still seals and opens
under the scope's keys, named for the unit: 4.8 us per unit at one unit, 2.4 us
at 1000 (`unit_content`), which is what it was per channel before.

**One storage unit** (one record: its own Automerge file, route and index
row), per unit:

| units | create | open | files | bytes | memory held open |
| --- | --- | --- | --- | --- | --- |
| 1 | 408 us | 119 us | 4 | 46 B | 20.6 KiB |
| 10 | 214 us | 40 us | 4 | 46 B | 19.1 KiB |
| 100 | 202 us | 51 us | 4 | 46 B | 19.4 KiB |
| 1000 | 207 us | 36 us | 4 | 46 B | 19.4 KiB |

A unit is cheap to have: 200 us to make (two syncs), 36 us to open, four small
files, 19 KiB of memory while it is held open. Per-unit fixed cost is what
keeps units small; what a unit costs to read is its document.

**One document**, one map of small items, one change per item:

| items | snapshot | history | save | load | memory after load | one change | appended |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1k | 6.4 KiB | 135 KiB | 0.34 ms | 3.6 ms | 276 KiB | 0.08 ms | 123 B |
| 10k | 71.6 KiB | 1.37 MiB | 2.1 ms | 31 ms | 3.1 MiB | 0.18 ms | 124 B |
| 100k | 730 KiB | 14.1 MiB | 28 ms | 325 ms | 27 MiB | 4.7 ms | 124 B |

Loading a document costs about a quarter of a millisecond per thousand items
and holds about 38 times its snapshot in memory (the change graph, kept for
merging). Its history is about twenty times the snapshot, and one more change
is about 120 bytes whatever the size.

A record on disk also keeps `state.hashes`, its own list of change hashes as a
load hint: 32 bytes per change, so 320 KiB beside a 70 KiB snapshot at ten
thousand items, and 3.2 MiB beside 730 KiB at a hundred thousand. The hint pays
for itself on open (a 10k-item record loads in 12 ms where the same document
takes 31 ms to load from its snapshot alone, because the hint skips re-hashing
every change), but it is the same order as the history, not a rounding error.

**One index**, one redb file per package and workspace, one row set per record:

| records | write one record | open | one lookup | file |
| --- | --- | --- | --- | --- |
| 1 | 42 us | 0.04 ms | 5.1 us | 1.0 MiB |
| 10 | 30 us | 0.04 ms | 0.7 us | 1.0 MiB |
| 100 | 28 us | 0.06 ms | 0.5 us | 1.0 MiB |
| 1000 | 36 us | 0.12 ms | 0.6 us | 1.0 MiB |
| 10000 | 36 us | 0.10 ms | 0.6 us | 2.1 MiB |

An index answers a lookup in under a microsecond and opens in a tenth of a
millisecond whether it holds ten records or ten thousand; the rows are about
100 bytes per record. What it costs per workspace and not per record is redb's
minimum file: about a megabyte before it has a record in it, which is why an
index is one per (package, workspace) pair rather than one per unit.

**A host opening a workspace** (`tilolang-tilo`'s `bench` example,
`--open-only`, p50 of three to five runs, taken on a sixteen-thread laptop while
other builds shared it, load average around fifty, so the times are inflated
several-fold; notes here, the board earlier, on a quieter aarch64 laptop):

| items | open (over the index) | of which building the view | load the record |
| --- | --- | --- | --- |
| 10k | 0.39 s (66 ms earlier) | 0.31 s | 0.45 s (412 ms earlier) |
| 100k | 3.6 s (1.04 s earlier) | 3.1 s | 4.3 s (6.7 s earlier) |

In both measurements loading the record costs as much as opening over the index
or several times more, so the index saves reading it but not having it: what a
host pays for a workspace is dominated by the record's own bytes and by the
view building its rows from them, and both grow with the number of items. A
chunk the views do not read in full is what brings either down.

What that implies for the layers above:

- **Key scope, not channel.** Per-channel keys are what make a membership
  change linear in the workspace: the 1.96 ms above is one commit and one
  delivered frame set per channel, on every member. Keys per reader set make
  the same change one commit for the whole workspace, and a device's sealed
  state one file per reader set instead of 760 B per record.
- **A chunk is a few thousand items, not a record per app.** A 10k-item chunk
  loads in 31 ms and holds 3 MiB; a 1k-item chunk in 3.6 ms and 276 KiB. A
  chunk that holds a whole collection of a few thousand small resources keeps
  open, save and memory at tens of megabytes for a workspace of a hundred
  thousand items, where one document per app at that size is 27 MiB and
  325 ms to load before anything is shown.
- **Units stay separate for what is written separately.** A record's fixed
  cost is small enough that units are not the thing to merge; what has to
  change is what is written and sealed together, so that a chunk's commit is
  one scope and one append.

## Where the code differs

The code still carries models this page does not have. Each goes, not beside
the model but instead of it:

- **The stored room spelling.** `RoomId`, the `room` key a desktop entry or an
  action row stores its workspace id under, `(app, room, record)` addresses and
  `tilo-workspaces` on disk. The workspace is the key and is written `workspace`;
  what is stored today keeps reading, and `RoomId`'s last uses are the old
  shell's.
- **Governance by one device.** The governor, the pinned governance writer,
  the separate governance document with its receipts, custody and succession,
  and the host-served `tilo.host` / `tilo.member` namespaces. Membership is
  workspace state changed by actions.
- **Per-person spaces.** The person's relay workspace
  (`personal_continuity_workspace_id`), derived from the root key under its
  own domain string. It is left only to the person's hosts: they keep the
  person's records on it for devices that are away, and ush environment
  sessions run on it. The person's devices already meet without it, on the
  session between two of them (`tilo-relay-sibling/1`), and learn the
  person's workspaces from the hosts holding them (`room.list`). The person's
  own records in particular are in no workspace: their store is
  one cluster per person, derived from the root key, and the readers of its
  channels are the identity's certified devices that are not revoked — the
  identity decides, and no governor or workspace takes part. Any of those
  devices creates a channel's group and admits the others
  (`channel_crypto::ChannelPeer`), as a workspace's participants do.
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
