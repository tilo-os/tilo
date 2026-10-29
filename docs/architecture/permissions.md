# Permissions

Who may do what is decided over the same tree everyone already sees: the
published tree of rooms, apps, resources, items and actions
([resources.md](resources.md)). A grant names a node of that tree, the kinds of
action it covers, and an answer. Grants are data, so people and agents can
read, explain and change them; granting and revoking are actions themselves.

There is no per-action permission declaration in apps, no ladder of verbs and
no mapping between ladders. An action needs exactly what its node and kind
are granted.

## 1. Grants

```rust
pub struct Grant {
    pub subject: Subject,     // a person, a role preset, an agent, a task, a Shortcut, an app, a bot,
                              // or a person on a device that is not theirs (ForeignDevice)
    pub node: TreePath,       // room › app › resource kind › resource › item › action
    pub kinds: KindSet,       // any of Read, Undoable, Outside
    pub answer: Answer,       // Allow, Ask, Deny
    pub lifetime: Lifetime,   // Once, Session, Until(time), UntilRevoked
    pub granted_by: Subject,
}
```

- **Hierarchical.** A grant on a node covers everything below it: "Snippets"
  covers every snippet and every action on them; "this document" covers only
  that document; "Snippets › Delete" covers one action. Field edits are actions
  in the tree (`Snippets › snippet › favorite`), so a grant can be as narrow as
  one field.
- **Filtered by kind.** Every grant names the kinds it covers. "Undoable only"
  is the usual agent grant. Kinds come from the framework
  ([resources.md](resources.md) §2.3), never from the app.
- **Three answers.** Allow runs. Ask pauses for the person's approval. Deny
  refuses. Ask is witnessed by the host on the person's device, never attested
  by the caller.
- **Privileged actions need the person's own Allow.** An action marked
  `privileged` exercises authority over the room or others in it (approving,
  publishing, trusting). Where the person's own grants only ask, it is refused:
  their confirmation is no authority they lack. A delegation's Ask on it still
  asks the person. Every door decides it at the moment it runs, so a person
  whose access was narrowed cannot carry an earlier decision past the change.
- **New actions are covered safely.** A grant on a node covers actions an app
  adds later, but only within its kinds: a new Outside action does not slip
  under an "Undoable only" grant.

A grant set reads as a sentence, which is how people tell an agent what it may
do:

> Mail: read everything, undoable changes allowed, sending asks me.
> Snippets: everything. Project room: comment only.

## 2. Deciding

`decide(subject, node, kind) -> Answer`:

1. Collect the live grants whose subject matches and whose node is the action's
   node or an ancestor of it, and whose kinds include the action's kind.
2. The most specific node wins.
3. At the same node, Deny beats Ask beats Allow.
4. No grant: Deny.

So "Mail: Allow; Mail › Send: Ask" asks before sending and allows the rest,
and anyone can predict an outcome without a table.

Expired grants and `Once` grants after use are not live. One function in one
crate (`tilo-action-policy`) decides for every door, every host and every
receiver.

## 3. Roles and governance

Roles are presets of grants on a room, kept as named grant sets:

| Role | Grants on the room |
|---|---|
| Viewer | Read: Allow |
| Commenter | Read: Allow; comment units and actions, reactions: Allow (Undoable) |
| Editor | Read, Undoable: Allow; Outside: Ask; moderation: Deny |
| Moderator | as Editor; moderation: Allow; `remove-member`, `ban`, `unban`: Allow on members below Moderator |
| Owner | everything: Allow, and governance |

A built-in role's rules have one definition, the code's (`roles::role_rules`).
A room stores its roles' grants (`role:<role>:<index>`) so older builds read
them, but every reader of a stored policy replaces them with the current
definition (`RoomPolicy::with_current_roles`), so a role's change reaches
rooms written before it. A room does not edit a built-in role: governance
refuses to revoke or give a `role:` grant (`Refusal::BuiltInRole`). It adjusts
a role with grants of its own on the role's subject, and those stay as given.

The room is itself a resource in the tree. Inviting, removing members,
changing roles and room settings are the room's actions; a role preset or a
grant covers them like any other node. Ownership stays structural: the owner
cannot be removed by a grant.

**Moderation.** Some actions act on other people's content: removing someone's
message, locking a channel. An app marks them `#[action(moderation)]`, and the
framework places them under a `moderation` field node of their resource
(`room › app › kind › resource › moderation › action`). Role presets can then
cover every app's moderation with one grant on `room/**/moderation`, with no
list of verbs. Editors are denied there and Moderators allowed. An app still
checks authorship for ordinary edits and deletes ("only your own message"),
because that rule depends on the item, not on the tree. A Moderator's
governance reaches only members ranked below Moderator. `RoomPolicy::check_action`
refuses a Moderator who removes or bans a Moderator or the Owner. `ban` and `unban`
are new governance actions: a ban is a removal plus the `block:<person>` Deny
(`RoomPolicy::block`) that keeps the person out until `unban` revokes it.

A room's shared desktop follows the same rule. It keeps every strip and
window as an entry of its own, with who made it
(`tilo_desktop_model::shared_record`), so concurrent changes merge; people
close, move and rename what they made as an ordinary edit, and doing it to
what someone else made (or to a strip the room itself lists) is moderation on
that item's `moderation` node (`tilo_desktop_model::ownership`). The relay
decides every write of the desktop against it before keeping it, and a
desktop asks the room (`RoomsRequest::Permits`) before it offers a close or a
drag on someone else's window.

A room's administrators moderate it too: beside their governance grant they
hold one on `room/**/moderation/*`, which outranks an Editor's Deny there, so
an Owner or administrator need not also be named a Moderator.

## 4. Delegation

An agent, a task, a Shortcut, a bot or an app acting for a person acts with
the person's authority, bounded by the delegation's grants:

```
answer = min(decide(person, node, kind), decide(delegation, node, kind))
```

with Deny < Ask < Allow. An agent never does more than the person who started
it, and a person's restriction always reaches their agents.

- **Agent default.** A new agent delegation grants Read and Undoable within the
  scope the person gives it (rooms, apps, a task), and Ask for Outside. People
  are not discouraged from granting more.
- **Issues.** An agent's run of an issue in the Tasks app is the agent and
  the issue: the room's grants naming the issue (`Subject::Task` with the
  issue's address) decide for the run as the agent's own do, the most
  specific node first, and the approver's grants bound both. "Allow for this
  issue", offered on the host's confirmation sheet when a run asks, is such a
  grant for the one action asked about, given as the person, shown and
  revoked like any other; like an agent's answer for one action it lifts
  that action's confirmation. It needs no lifetime of its own, as it decides
  nothing while no run is going. A confirmation of one action grants
  nothing.
- **Shortcuts.** A Shortcut's grant set is stored with it and shown on import.
  An unattended run turns Ask into a refusal unless the set holds a matching
  Allow.
- **Bots.** A bot is an installed Shortcut acting under its own identity
  (`Subject::Bot(installation)`). Its grant set is derived from its steps,
  reviewed once by the installer, and bounded by the installer's own authority.
  It never includes governance ([bots.md](bots.md) §3). Agents and bots hold the
  same kind of root-signed, expiring delegation for their own device keys.
- **Composed actions.** An effect calling another app's published action
  while it runs an invocation someone asked of it continues that
  invocation: the host decides the call as its caller, by their grants and
  under the custody it kept for them, and the app's own identity adds
  nothing. A call continuing an invocation the app made of itself through
  its own door is the app's own request, decided as one.
- **Attenuation.** What an effect asks for while it continues an invocation
  (another app's action, an edit of its own resource, an open) never reaches
  further than that invocation was admitted to: its kind (Read, Undoable,
  Outside, in that order) and whether it is privileged. Its room is bound
  already, as the custody it continues holds only there. A child that goes
  further is not refused outright, nor run because the caller's grants
  would allow it: it asks first, put to the person on their own device as a
  request of its own, and runs only once they confirm that one child. So
  approving an agent's read approves reading, not whatever edit its effect
  goes on to make. The cap carries down a chain: what a child was admitted
  to reach is its own reach narrowed by its parent's, so a child that
  needed only reading (an edit of the person's own record, say) passes no
  more than reading on. A child the person confirmed reaches what it is. A
  child is never the person's own gesture: it cannot confirm itself, and
  an ask in their grants goes to their device like anyone else's
  (`tilo_action_policy::Reach`, `BrokerActor::verdict_in`).
- **History.** Every entry records the actor and the person it acted for
  ([resources.md](resources.md) §5.1).

## 5. Enforcement

Grants are checked twice, by the same function.

### 5.1 At the door

Every door that invokes actions (the app's own dispatcher, the broker, MCP,
the agent participant, remote action hosts, the shell palette) calls
`tilo-action-policy` before dispatch, with the caller's subject and delegation.
Discovery uses it too: a catalogue lists exactly the actions the caller may
invoke, with Ask marked, so nobody learns "not allowed" only after trying.
A lookup that cannot resolve an action refuses.

### 5.2 On receipt

Every peer checks every incoming change before merging it.

- A change is signed by its author and names its app package (by content
  hash), its action (`ActionKey`, or the field path for a field edit) and the
  storage units it writes. The package is named in the change itself because
  in a sealed room the relay cannot read the record. Kinds, footprints and
  tree positions come from that package's `surface.toml`, fetched by hash;
  until it is available the change is held, never trusted. Builds that are not
  packaged serve their committed surface by hash through the same path. The
  committed file is the shipped one, byte for byte, so its hash is the one a
  change names; `tilo-app surface` writes it from the code and keeps only what
  this check and the storage check need (kinds, verb ids, kinds, footprints,
  tree fields and parameters; field paths, units, layouts and types). Labels,
  descriptions and field titles stay in the code.
- **The relay** checks, without app code and without reading content, that the
  action is granted to the author (and their delegation) and that the change
  writes only units that action may write. Storage units come from the model:
  each record, and each `#[model(unit)]` field or item. This is the boundary a
  modified app cannot get around.
- **Devices** that have the app replay the action and check that it produces
  exactly this change. That catches an author who is granted an action but
  wrote something else inside its units.
- Where every participant holds the same grants over a record (everyone an
  Editor), replay can be skipped; the relay check still runs.
- In an MLS-sealed room the relay sees the envelope (author, action key,
  units) but not the change; replay happens on devices, which hold the keys.

### 5.3 Reading

Read is a kind like any other, so a member can be denied Read on one app or
one record of a room they read: "Finance: Deny Read; Finance › summary: Allow
Read" hides every finance record but the summary. Every per-record read
decision asks the same function: the room peer's record read check
(`StatePeer::check_document_read`) and the joined room's document reads
through it, and so does channel admission, below. A device whose person is
denied any record is refused the room's whole legacy delivery rather than given
a filtered one, because an automerge change graph cannot be served in part
([workspace.md](workspace.md), "State").

What carries a read decision to the bytes is the record's channel key, and
a channel's participants admit devices per channel, that is per record
(`ChannelDirectory::allowed_devices(room, channel, route, now)`). A device is
admitted to a record's channel only while its person reads the room and the
same read decision allows the record (`Workspace::answer_at` on a desktop,
`PeerGovernance::channel_readers` over `check_document_read` on a relay or a
standalone runtime). So a Read Deny on a record, on its app, or on any
pattern of records covering it withholds that record's key: no participant
admits the denied person's devices, every participant refuses a commit that
would, and a device that already held the key is removed in
a new epoch of that channel alone, while every other channel of the room keeps
its epoch and its readers. As with any removal, the device keeps what it could
read before. Lifting the Deny admits it again when it asks.

A channel id is a hash of its record's route, so a participant has to be
told which record a channel carries. It learns it two ways, and neither asks it to
trust anyone:

- A grant naming the record (`room › app › resource`, or below it) gives the
  route itself (`collab_types::named_record_of_channel`).
- A device asking for a channel's key sends the record's route beside its
  request (`NeighbourMsg::MlsRoutedAccessRequest`, `collab_types::ChannelRoute`),
  as does the participant's own endpoint. The route proves itself: a
  participant keeps it only if it hashes, with the room, to the channel's id, and refuses
  a request whose route does not. It keeps the route beside the channel's
  group and decides every later change of the channel with it, after a restart
  too. A kept route is checked again when it is read back, so a damaged file
  names no record rather than a wrong one.

A channel whose record a participant cannot name may be any record, a
member's own record (`member:<root>:<kind>`, read by that person alone)
included, and its id does not say whose. It is withheld from every device
until a device names its record, so a plain request that reaches a participant
before its routed copy admits no one. The
record of a channel nobody has written yet is named by its first request, so a
new record admits its author and every other reader the decision allows from
its first key. Nothing a participant keeps can let a denied device back in: a lost
or unreadable route makes the channel one whose record is unknown, which
withholds more, never less.

A Deny below a record, on an item or a field, binds every honest device and
door, but not a member's own modified client: a record has one channel and one
key, which opens all of it, and splitting a record's channel per field would
be key management that narrows nothing a separate record does not. Against a
modified client the record is the boundary. To keep a part of a record from
someone, keep it in a record of its own and deny that record, by name or by a
pattern, or keep it in a workspace they are not in
([workspace.md](workspace.md), "State").

The sandbox's capabilities ([security.md](security.md)) keep the opposite
rule: there `Read` is the floor, and a restriction makes a path, device, host
or record read-only without hiding it.

## 6. What an app does

Nothing. An app does not declare permissions, roles or verbs. It publishes its
tree ([resources.md](resources.md)); the framework derives kinds and storage
units; the OS decides. `destructive` on a verb asks a person to confirm; it is a
safety prompt, not a permission.

## 7. What the sandbox does

The sandbox bounds what an app's code may touch (files, network, camera); it is
granted to the app at install and through portals, and it is never what a
person grants an agent. "May the agent send mail" is a grant on Mail's send
action. Whether the mail app may reach its server is the sandbox's business.
See [security.md](security.md).

A shell environment's file commands meet both and stay one model each: `mv`
typed in a guest, or an agent's `move`, is the environment record's Outside
action, which the door decides for the actor like any other; the verb then
admits the paths it names only within the environment's own `FsPath` grants,
read with the same `decide_capability`, and refuses the whole invocation
otherwise ([ush.md](../app-model/ush.md)).
