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
    pub subject: Subject,     // a person, a role preset, an agent, a task, a Shortcut, an app
    pub node: TreePath,       // room › app › resource kind › resource › item › action
    pub kinds: KindSet,       // any of Read, Undoable, Outside
    pub answer: Answer,       // Allow, Ask, Deny
    pub lifetime: Lifetime,   // Once, Task(id), Session, Until(time), UntilRevoked
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
| Commenter | Read: Allow; comment units and actions: Allow (Undoable) |
| Editor | Read, Undoable: Allow; Outside: Ask |
| Owner | everything: Allow, and governance |

The room is itself a resource in the tree. Inviting, removing members,
changing roles and room settings are the room's actions; a role preset or a
grant covers them like any other node. Ownership stays structural: the owner
cannot be removed by a grant.

## 4. Delegation

An agent, a task, a Shortcut or an app acting for a person acts with the
person's authority, bounded by the delegation's grants:

```
answer = min(decide(person, node, kind), decide(delegation, node, kind))
```

with Deny < Ask < Allow. An agent never does more than the person who started
it, and a person's restriction always reaches their agents.

- **Agent default.** A new agent delegation grants Read and Undoable within the
  scope the person gives it (rooms, apps, a task), and Ask for Outside. People
  are not discouraged from granting more.
- **Tasks.** A task carries its own grant set (lifetime `Task`), shown on the
  task and editable as actions; it ends with the task.
- **Shortcuts.** A Shortcut's grant set is stored with it and shown on import.
  An unattended run turns Ask into a refusal unless the set holds a matching
  Allow.
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
  packaged serve their committed surface by hash through the same path.
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
