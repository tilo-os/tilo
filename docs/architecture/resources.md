# Resources, actions and views

Every app is written one way. Its state is a typed model stored in a CRDT
document, its changes are actions on that model, and its pixels are views that
read the model and submit actions. People, collaborators, agents, Shortcuts,
the palette, search and accessibility all reach an app through the same
published tree of resources and actions. Nothing is declared twice, nothing
is addressed by a string an app writes by hand, and nothing happens without
someone being able to see who did it.

This document is the contract for app developers and for the framework
(`crates/sdk/tilo-resource`, `crates/sdk/tilo-resource-macros`,
`crates/sdk/tilo-app`). Who may invoke what is in [permissions.md](permissions.md).

## 1. The model

An app's state is a struct with `#[derive(Model)]`. The derive is the whole
storage contract: how each field lives in the Automerge document, how it is
read incrementally after a change, and how a write touches only what changed.
Apps never call Automerge.

```rust
#[derive(Model, Default, Clone, PartialEq)]
#[model(check = Snippet::valid)]
pub struct Snippet {
    #[model(label)]    pub title: String,
                       pub body: String,
                       pub favorite: bool,
                       pub folder: String,
                       pub language: Language,          // unit enum
                       pub tags: Set<String>,
    #[model(readonly)] pub uses: Counter,
}

#[derive(Model, Default, Clone, PartialEq)]
pub struct Snippets {
    pub snippets: Collection<Snippet>,
}
```

### Storage types

Each type declares how concurrent edits merge. Pick the type for the merge you
want, not only for the value you hold.

| Type | Stored as | Concurrent edits |
|---|---|---|
| `bool`, numbers, unit enums, `Atom`, `Id`, `Uuid` | scalar | last writer wins |
| `String` | Automerge text | character splices merge |
| `RichText` | Automerge text with marks and blocks | splices and marks merge |
| `Counter` | Automerge counter | increments add up |
| `Set<T>` | map keyed by the value | adds and removes merge per element |
| `Vec<T>` | list, by position | for short lists that are replaced whole |
| `BTreeMap<String, T>` | map | per key |
| `Collection<T>` | items with stable `Id`s and order keys | per item; moves keep identity |
| `Grid<T>` | rows and columns with stable ids, sparse cells | per cell; inserting a row keeps every cell's identity |
| `Bytes` | content-addressed blob reference; small values inline | last writer wins |
| `Outbox<T>` | queue of entries settled by the owning device | see §2.4 |
| `Option<T>`, `Box<T>`, nested `Model` structs | as the inner type | as the inner type |
| `Json<T>` | one JSON string | last writer wins; an escape hatch, not a default |

A type with a layout the table does not cover implements `Model` by hand. That
is the only place storage code is written by hand, and the framework ships the
shared ones (`RichText`, `Grid`). An app does not implement `Resource` by hand.

### Field attributes

- `#[model(label)]` names the item: history, search, agents and item resources
  use it ("Deploy script"). One per type.
- `#[model(readonly)]` publishes the field for reading but offers no field edit;
  only verbs change it.
- `#[model(unit)]` stores the field as its own storage unit (for a
  `Collection`, each item is one), so the relay can enforce who may write it
  without reading content ([permissions.md](permissions.md) §5). A comment
  thread inside a document is a unit; the document body is another.
- `#[model(rename = "key")]`, `#[model(skip)]` as before.
- `#[model(check = path)]` on a type names an invariant,
  `fn(&Self) -> Result<(), Refusal>`. It runs after every change that touched a
  value of that type, from any path: a field edit, a verb, a group, an agent's
  `apply`. A refused check refuses the whole change. One place covers every way
  in.

### Paths

The derive generates a typed path for every field:

```rust
let favorite: Path<Snippets, bool> = Snippets::paths().snippets.item(&id).favorite;
```

`Snippets::paths()` returns a struct with one field per model field; a
`Collection` path has `.item(&Id)`, a `Grid` path `.cell(&row, &col)`, a map
path `.key(k)`. A path is plain data (field names and item ids), so it can be
stored, compared and sent. In a row of a list view the paths are already rooted
at that item (`row.paths().favorite`).

## 2. Actions

An action is a typed change to a resource. There are three kinds, derived by
the framework and never declared by the app:

| Kind | What it does | Undo |
|---|---|---|
| **Read** | shows or reads (views, `read_content`, view effects) | nothing to undo |
| **Undoable** | changes the document and nothing outside it | by construction |
| **Outside** | reaches outside the document: sends, writes a device, calls a server | not undoable for permission purposes |

[permissions.md](permissions.md) grants by kind, so the kind must be impossible
to get wrong: it follows from what the code is able to do (§2.3).

### 2.1 Field edits

Editing a field is a built-in action. Every path offers the edits its type
supports:

| Type | Edits |
|---|---|
| scalars, enums | `set` |
| `String`, `RichText` | `splice`, `replace`; `RichText` also marks and blocks |
| `Counter` | `add` |
| `Set<T>` | `insert`, `remove` |
| `Collection<T>` | `insert`, `remove`, `move_to` |
| `Grid<T>` | `set_cell`, `insert_row`, `insert_column`, `remove_row`, `remove_column` |
| `Option<T>` | `set(None)`, and the inner type's edits when present |

```rust
dispatcher.submit(Snippets::paths().snippets.item(&id).favorite.set(true));
```

A field edit is a value of `Edit<R>`: a path plus an operation. It is part of
the resource's action type, so it replays, syncs, undoes and is named in
history like any action. It applies at its path: the framework does not read
the whole record to change one field. `#[model(readonly)]` fields offer none;
`check` runs after every one.

Consecutive edits of one path by one actor within one gesture (typing a word,
dragging a slider) are one undo step. The gesture ends when the control settles
or the actor does something else. No app writes coalescing code.

### 2.2 Verbs

Operations that are more than setting a field are methods:

```rust
#[resource(kind = "snippets.library")]
impl Snippets {
    /// Create a snippet.
    #[action]
    pub fn new_snippet(&mut self, cx: &mut ActionCx, title: String, folder: String) {
        self.snippets.push(cx.id(), Snippet::titled(title, folder.trim()));
    }

    /// Delete a snippet.
    #[action(destructive)]
    pub fn delete(&mut self, snippet: &Id) -> Result<(), Refusal> {
        self.snippets.remove(snippet).map(drop).ok_or_else(|| Refusal::new("no such snippet"))
    }

    /// Record a use.
    #[action]
    pub fn record_use(&mut self, snippet: &Id) {
        if let Some(s) = self.snippets.get_mut(snippet) { s.uses.add(1) }
    }
}
```

- A verb's public id is its method name in kebab case, unless `#[action(id = "...")]`
  says otherwise; parameters are published under their Rust names unless
  `#[param(name = "...")]`.
- A verb whose first parameter is `&Id` is also published on each item of the
  collection it acts on. When the collection is not the only one, or the
  parameter name does not equal the field name, `#[action(item = field)]` names
  it; anything else is a compile error.
- The verb's parameters are a generated struct in a module named after the
  record (`snippets::NewSnippet { title, folder }`) with a JSON schema derived
  from the parameter types themselves.
  Agents, the palette and forms (§3.3) all use it.
- Verbs run against the record the framework reads for them, and write back as
  a diff. A verb that only touches one item reads only that item.
- `destructive` asks for confirmation from people; it does not change the kind.

The resource attribute covers what used to need a hand-written `Resource`:
`initialize` (deterministic genesis), `seed` (first content, by the creator,
nobody's undo step), `available` (state-dependent availability of verbs),
`children` (derived child resources, §2.5), `search` and `scopes`,
`read_content`, `views`.

### 2.3 How the kind is derived

- A field edit and a verb that takes only `&mut self`, `ActionCx` and plain
  parameters can only change the document: **Undoable**, decided when the
  macro expands.
- A verb that takes `&mut Outbox<T>` records an outside consequence: **Outside**.
- An effect (`#[derive(Effect)]`) is **Outside**, or **Read** when it is a view
  effect (`#[effect(view)]`).
- The `read_content` and view operations are **Read**.

A lint covers what types cannot: no clock, randomness, file, network or process
access inside a verb, because verbs run again on every replica. The kind
protects people from honest mistakes and agents from wrong information; a
malicious app is bounded by its sandbox, not by kinds.

### 2.4 Outside consequences

An action that must reach outside writes an entry into an `Outbox<T>` in the
document. The framework drains it on the device that owns the outbox (the mail
account's device, the calendar's sync host, the device whose disk a file
operation targets), runs the app's handler for each entry, and records the
result with an action. Peers replay the document; only the owner drains. Undo
of the action is its inverse document action run through `apply`, so a
compensating entry is queued the same way. This one pattern replaces the
per-app queues (file operations, CalDAV, IMAP flags).

Effects remain for work with no document entry: starting a sync job, choosing
a file, a view change. They are typed (`#[derive(Effect)]`), long work is a job
with progress and cancellation, and a job changes the document only through
the resource's actions.

### 2.5 Child and derived resources

A field whose type is itself a resource model (a `RichText` body, a nested
record) is a child resource: its edits are field edits under the parent's path,
published on the child, permissioned and undone like the parent's. No parent
action wraps child edits by hand.

Things computed from state that people act on (folders derived from snippets,
tags, smart lists) are derived resources: `#[resource(children = path)]`
returns typed `Derived` entries whose actions are the parent's verbs with
arguments bound, e.g. `Derived::new(folder_label).with(Snippets::NEW_SNIPPET.bind(snippets::NewSnippet::paths().folder, name))`.
No string ids.

### 2.6 References to actions

`Snippets::NEW_SNIPPET` is an `ActionRef<Snippets, snippets::NewSnippet>`. It
carries an `ActionKey` (app id, resource kind, public action id), and building
its arguments is total: a value that does not serialize, or arguments that do
not match the schema, is an error at the call site, never `null` or an empty
map. A control bound to an action that the resource does not publish fails the
`TestApp` and `tilo-app check`, never only a greyed-out button.

An `ActionKey` is also the stored form when a reference is data: a Shortcut
step, an agent's plan, a palette entry. JSON appears only at the edge (agents,
the wire), validated against the published schema.

## 3. Views

### 3.1 Binding

A view reads the model and submits actions. `ViewCx` binds controls by path:

```rust
fn view(cx: &mut ViewCx<Snippets>) -> Element {
    let rows = cx.each(|s| &s.snippets, |row| {
        let p = row.paths();
        Box::new(ui::row![row.checkbox("snippets-favorite", p.favorite), row.input(p.title)])
    });
    Box::new(ui::column![cx.button("snippets-new", Snippets::NEW_SNIPPET.with(..)), rows])
}
```

A hand-written surface (`ResourceSurface`) submits the same typed values through
its dispatcher: `Edit<R>` from paths, verb values from `ActionRef`s. No app
writes an `ActionAddress`, an action id string or an argument map.

### 3.2 Reactivity

A path binding declares what the control reads. After a change, the framework
wakes only the controls whose paths the change touched. A control bound with
an arbitrary closure re-runs on every change, so correctness never depends on
tracking. The published tree is built when someone looks at it (a person, the
palette, an agent), subtree by subtree, and cached by item id.

### 3.3 Forms

A form is a local instance of a verb's parameter struct: never synced, never
undoable, invisible to collaborators. Its fields bind with the same paths; the
button submits it as one action.

```rust
let form = cx.form::<bookmarks::Add>();
let url = cx.input(form.paths().url);
let add = cx.button("bookmarks-add", form.submit());
```

### 3.4 Search

Every item with a `#[model(label)]` field is its own search entry by default,
anchored at its item resource. A hit invokes the item's `open`, which shows the
view at that item (§4). The index follows the same change deltas, so a large
collection is not re-indexed per keystroke. `#[resource(search = path)]`
replaces or extends the default.

## 4. Actions happen in views, visibly

Nothing is drawn if no one is looking, but no action is performed out of
sight. Invoking an action does whatever is needed to make its resource's view
live, and the action runs through that view, exactly as a person's click does.

- **Placement.** The view opens on the strip where the invoking task runs,
  next to where that actor already is, so everyone keeps their place. It may be
  a strip the person is not looking at. A person invoking from the palette gets
  it where they are.
- **Presence.** Whoever acts is a participant with presence in the view: a
  person, a collaborator, an agent, a Shortcut. Presence names the actor and on
  whose behalf it acts ("Claude, for Quinten"), shows where it works and what it
  just changed.
- **Me and others.** The person's own changes, a collaborator's and an agent's
  look different in every view and in history.
- **Hosts without a screen.** An agent host or relay that performs actions runs
  the app's view attached but undrawn, and publishes presence and placement.
  The person's devices draw that strip from synced state when someone looks;
  pixels are never streamed.
- **The one exception.** Replaying someone else's change to verify it
  ([permissions.md](permissions.md) §5) performs nothing new and needs no view.

A closed app's actions stay discoverable: the catalogue of a closed app is its
committed public surface (§6) plus its last published tree, and invoking one
launches the app into place.

## 5. History

### 5.1 Entries are data

Each committed action records a structured entry on its change:

- the actor (person, agent, Shortcut or app, with its kind), and the person it
  acted for;
- the task or run it belongs to;
- the resource and item it acted on, with the item's label at the time;
- what it was: a field edit (path, value before and after for scalars) or a
  verb (`ActionKey` and its scalar arguments), or a group of those;
- its kind (§2) and whether it is content, housekeeping or external.

Agents, scripts, logs and Shortcuts read entries as data. No finished sentence
is stored.

### 5.2 Sentences are rendered by the reader

The history view renders each entry in the reader's language from the app's
Fluent catalogue, by convention:

```ftl
snippets-snippet-favorite = { $new ->
    [true] Starred { $item }
   *[false] Unstarred { $item } }
snippets-snippet-folder = Moved { $item } to { $new }
snippets-new-snippet = Created { $title }
```

- Field edits use `<app>-<type>-<field>`, verbs `<app>-<action>`.
- Arguments: `$item` (the recorded label), `$old`, `$new`, each verb parameter by
  name, `$count` for groups, `$actor` and `$for`.
- Without a message, the framework's own messages render "Changed *Folder* of
  *Deploy script*" from the field's doc comment, never a raw id or path.
  `tilo-app check` lists missing messages.

### 5.3 One undo

There is one undo engine: per actor, and per task or run for agents, with
gestures coalesced (§2.1). Ctrl+Z and the published `undo` / `redo` actions use
it, and it survives a restart. Document changes undo by inverse patches;
actions that queued an outside consequence undo through their inverse action.

## 6. Evolution

- **The public surface is committed.** `tilo-app surface` writes the app's
  verbs, their parameters, its field schema and its kinds to `surface.toml` in
  the app crate; `tilo-app check` and CI fail when code and file disagree. A
  rename is allowed and shows up in review. The packaged surface is also what
  the OS knows about a closed app.
- **Storage changes are migrations.** `#[resource(version = N, migrate = path)]`
  declares the current schema version; `migrate` converts a document from any
  older version, reading old shapes with the model helpers. It runs once, as a
  housekeeping action with an actor and seed derived from the record and the
  versions, so two devices migrating concurrently produce the same change.
  `tilo-app check` fails when the schema in `surface.toml` changed
  incompatibly without a version bump.
- **An unreadable document fails loudly.** A document that does not read as the
  model shows an error state in the view and its actions report why they are
  unavailable; stale state is never shown as current.
- **Old history keeps rendering.** An entry naming a field or verb that no
  longer exists renders through the framework fallback.

## 7. Testing

- `Replica<R>` applies actions through the same door as the host: permission,
  kind, confirmation and history entries included.
- `TestApp` drives the real component runtime: invoke, confirm, undo, roles and
  grants, actors with kinds (`as_agent`, `for_person`), and asserts on
  structured history entries.
- `tilo-app check` runs the surface, binding, Fluent and lint checks.

## 8. Leaving the old APIs

Apps and examples still hold code written against the APIs this model
replaced: hand-written `impl Resource for`, `ActionDecl::new`,
`ActionAddress::new`, untyped argument parsing (`args.text(..)`,
`args.get(..)`), `ActionSpec::` descriptors and direct Automerge writes
(`put`, `put_object`, `insert_object`, `splice_text`, `update_text`,
`increment`). `tools/policy/check_app_resource_action_contract.py` counts each
per source file, comments and literals excluded, tests included, and holds
every count at its audited ceiling in
`tools/policy/app_resource_action_baseline/<app>.toml` (`example-<name>.toml`
for `examples/apps/<name>`), under `[ratchets.<rule>]`. A file with no entry
allows none.

A count that rises fails the check, naming the file and the first use past the
ceiling: write it with the model instead. A count that falls fails too, as a
stale ceiling: lower the number the message gives in the same commit, delete
the entry once it reaches zero, and delete the file once it holds no entries.
Raising a ceiling is an exception that needs review.
