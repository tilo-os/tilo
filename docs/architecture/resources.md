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
| `Set<T>` | map keyed by the value | adds and removes merge per element; saving and comparing a copy visit only the chunks changes copied |
| `Vec<T>` | list, by position | for short lists that are replaced whole |
| `BTreeMap<String, T>` | map | per key; copied whole when a view holds the state, so for small maps |
| `Map<T>` | map, as `BTreeMap<String, T>` | per key; a change copies only its chunk of a held state, and saving it visits only the chunks changes copied |
| `Collection<T>` | items with stable `Id`s and order keys | per item; moves keep identity; one id made on two replicas reads as one item, field by field |
| `Grid<T>` | rows and columns with stable ids, sparse cells | per cell; inserting a row keeps every cell's identity; saving and comparing a copy visit only the rows of cells changes copied |
| `Bytes` | content-addressed blob reference; small values inline | last writer wins |
| `Reactions` | map of each reaction, keyed by emoji and who reacted | per person and emoji; changed only by `react`/`unreact`, which record whoever invokes them |
| `Outbox<T>` | queue of entries settled by the owning device | see §2.4 |
| `Option<T>`, `Box<T>`, nested `Model` structs | as the inner type | as the inner type |
| enum with fields (`#[derive(Model)]`) | map: the variant's name under `$variant`, its fields beside it | per field of one variant; a change of variant is last writer wins |
| `serde_json::Value` | an object as a map, an array as a list, everything else a scalar | per key of an object, by position in an array; only for JSON another party shapes (an account type's settings, a binding's literal value, a parameter's JSON Schema) |

A type with a layout the table does not cover implements `Model` by hand. That
is the only place storage code is written by hand, and the framework ships the
shared ones (`RichText`, `Grid`). An app does not implement `Resource` by hand.
A type defined below the resource layer, or one that holds itself (a
condition made of conditions), is stored as its serde form's maps, lists and
scalars through `value_model!` in the crate that defines it: a Shortcut step's
bindings, an action's key and parameters. An app never stores a value as JSON
text it wrote itself.

### Field attributes

- `#[model(label)]` names the item: history, search, agents and item resources
  use it ("Deploy script"). One per type. `#[model(label = "first last")]` on
  the type names it by several fields joined ("Ada Lovelace"), each named once,
  falling back to the `#[model(label)]` field when they are empty.
- `#[model(readonly)]` publishes the field for reading but offers no field edit;
  only verbs change it.
- `#[model(unit)]` stores the field as its own storage unit (for a
  `Collection`, each item is one), so the relay can enforce who may write it
  without reading content ([permissions.md](permissions.md) §5). A comment
  thread inside a document is a unit; the document body is another.
- `#[model(whole)]` reads all of a value that is otherwise read as views ask
  for it (a `RichText`'s paragraphs), so verbs, search and `read_content` see
  every word: for short texts such as a note's body or a slide's text box.
- `#[model(default = expr)]` is what the field reads as when the document holds
  none: a required field with a sensible derivation (`#[model(default =
  Id::from_raw("home"))]`), so an item written without it still reads. The
  next whole write of the item stores it.
- `#[model(rename = "key")]`, `#[model(skip)]` as before.
- `#[model(check = path)]` on a type names an invariant,
  `fn(&Self) -> Result<(), Refusal>`. It runs after every change that touched a
  value of that type, from any path: a field edit, a verb, a group, an agent's
  `apply`. A refused check refuses the whole change. One place covers every way
  in. A field edit reads the record again to check it, so a record holding a
  lot (a sheet's cells) says what its check reads:
  `#[model(check = Sheet::valid, check_reads = "name color charts")]`, and
  `check_items = "sheets"` for a collection whose items it only counts. An edit
  inside none of them (typing into a cell) does not re-read the record; a verb
  still checks the record it changed. A field left out that the check does
  read goes unchecked on field edits.

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
| `Reactions` | `react`, `unreact`: the invoker's own, so a `readonly` field or collection takes them too; the Commenter role may make them |
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
  a diff. A verb that only touches one item is an item method: it takes the
  item instead of `&mut self` and reads and writes only that item.

  ```rust
  /// Star a snippet.
  #[action(item = snippets)]
  pub fn star(snippet: &mut Snippet, on: bool) { snippet.favorite = on; }
  ```

  It is published with the item's id as its first parameter (`snippet`), and
  on each item. A record with an invariant of its own (`#[model(check)]` on
  the record) or a `queue` hook is still read whole for it.
- `destructive` marks a deletion or overwrite; it does not change the kind.
  An undoable one (the document alone, or a compensated outside change) runs
  at once and offers Undo; only one Undo cannot take back asks people first.

The resource attribute covers what used to need a hand-written `Resource`:
`initialize` (deterministic genesis), `seed` (first content, by the creator,
nobody's undo step), `available` (state-dependent availability of verbs),
`children` (derived child resources, §2.5), `search` and `scopes`,
`read_content`, `facts` (a compact summary to check the record against a
request, published as the `facts` view), `views`.

### 2.3 How the kind is derived

- A field edit and a verb that takes only `&mut self`, `ActionCx` and plain
  parameters can only change the document: **Undoable**, decided when the
  macro expands.
- A verb that takes `&mut Outbox<T>` records an outside consequence: **Outside**.
- An effect (`#[derive(Effect)]`) is **Outside**, or **Read** when it is a view
  effect (`#[effect(view)]`).
- The `read_content`, `facts` and view operations are **Read**.

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

```rust
#[derive(Model, Default, Clone, PartialEq)]
pub struct Files { pub ops: Outbox<FileOp, Vec<Change>> }   // settled with what changed

#[resource(kind = "files")]
impl Files {
    /// Move a file to the trash.
    #[action(undo = Self::untrash)]
    pub fn trash(&mut self, cx: &mut ActionCx, ops: &mut Outbox<FileOp, Vec<Change>>, path: String) {
        ops.push(cx.id(), FileOp::trash(path));
    }
    fn untrash(undoing: Undoing<'_, Self, files::Trash>) -> Option<Invoke<Self>> {
        Some(Self::REVERT.with(undoing.minted.first()?.clone()))
    }
    /// Put back what an operation changed.
    #[action(item = ops)]
    pub fn revert(&mut self, cx: &mut ActionCx, ops: &mut Outbox<FileOp, Vec<Change>>, op: Id) { .. }
}

// the view, on every device; only the owner carries entries out
cx.outbox(Files::paths().ops, move |_, op| op.device == me, |id, op, cx| Ok(Effected::value(run(op))));
```

- An `Outbox<T, O>` entry settles as `Settled::Done(O)` or `Settled::Failed { reason, partial }`:
  `O` is what the handler reports back (where a trashed file went). A verb reads it with
  `outcome(&id)`, may `settle` an entry and `forget` or `prune` settled ones.
- The settle edit is content from outside and forgetting is housekeeping: neither is
  anyone's undo step, so an undo never sends a mail twice. Both are recorded in history
  (`$state`, `$reason`).
- `#[action(undo = path)]` names the verb that compensates an Outside verb, from the record
  before and after it and the ids it minted. Undo applies that verb (which reads at that
  time how the entry settled), redo runs the original verb again. Without `undo`, undo
  reverts the document, which forgets an entry nobody carried out yet.
- `#[action(item = ops)]` on a verb whose first parameter is an `Id` names an outbox entry;
  such a verb is not offered on collection items.
- `#[resource(queue = path)]`, `fn queue(&mut self, before: &Self, cx: &mut ActionCx)`, runs after
  every verb and field edit of the record and may push to its outboxes: an edited event of a
  remote calendar queues its upload, a changed flag its IMAP command. Every action of such a
  record is Outside, and undo applies the field edits that restore the record (or the verb's
  declared `undo`) through `apply`, so the hook queues the compensation.
- `cx.outbox_awaited(..)` runs an entry an invocation on this device queued under that
  invocation, which completes once the entry settled.
- A handler's `Err` leaves the entry pending, tried again after the next change or when the
  `Projection` it returns is told the outside changed.

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

### 2.6 Child records

A child that can need its own permissions, sharing or history (a chat's
channel, a category) is a record of its own, not a field: `Children` in the
parent lists each one by id, in order, with its kind, the sibling it is
grouped under and whether it is in the room's trash
(`tilo_resource::children`). Its record is `RecordId::child` of the parent,
under an id minted once that never changes, so moving it moves only its
reference and every reference to it keeps working.

A type names where it is listed (`impl ChildRecord for Channel`), the app
declares it (`records.children::<Channel>()`) and reads it from a view
(`cx.children::<Channel>()`). The framework holds every listed child,
publishes the container beside the app's resource (each child with its own
actions, carried out on its record) and gives every container the same
lifecycle, which the app does not write: `create` with the child's first
fields, `delete` to the trash (record and id kept), `restore`, and `move` to
another group or place. All of it is checked against the room's roles and
recorded in history like the app's own actions.

### 2.7 References to actions

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

A view edits another record than its app's own the same way. The person's
own record (`cx.person::<P>()`) and a settings section that is a model
(`#[derive(Model)]` with an empty `#[resource]`, read with `cx.settings::<T>()`)
are records of their own: `cx.record(&record, |cx| ..)` builds a part of the
view over it, whose controls bind its paths and verbs, and a surface submits
through `cx.record_dispatcher(&record)`.

```rust
let display = cx.settings::<Display>();
cx.record(&display, |cx| Box::new(cx.checkbox("High contrast", Display::paths().high_contrast)));
```

Each is published beside the app's resource (`person:<kind>`, `settings:<kind>`)
with its field edits and verbs, so the palette and agents edit it by path too.
Only its person changes it (or software acting for them), whatever their role
in the room; its `check` holds on every way in; each change is recorded in the
record's history (a person record's own history and undo, the settings
document's log of what changed which setting and who).

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

### 3.5 Opening a reference

A reference to what another app keeps (a `tilo_reference::ResourceRef`
beside the app and record that keep it) opens with one action: the
resource's `open` in that app, `ResourceRef::open_target`, an application
target naming the workspace, the app and the record. The host delivers it
to the window showing that record or launches the app on it, and admits it
as the person who asked, under their grants, as it admits any action; an app
showing a reference needs no `[actions] invoke` of its own, since opening
grants nothing. `ViewCx::reference` shows a reference as a chip that does
this on a click, Enter or assistive technology's activate, and tells a
refusal like any refused action. Text that names a resource the way its
app reads it (`#12`) is that app's to resolve, through its root `open` with
the text as the anchor. Copying a link (`share-link`) is for leaving Tilo;
opening never needs one.

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
  whose behalf it acts ("Claude, for ada"), shows where it works and what it
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

The entry travels in its change's message as `tilo.entry/3 ` followed by the
entry in a compact form (`tilo_actions::compact`): one-byte tags instead of
JSON punctuation, the keys and values every entry uses (`did`, `field-edit`,
`path`, …) as two-byte words, a string repeated within the entry as a
back-reference, UUIDs and gestures as 22 digits, numbers six bits a byte. It
stays UTF-8, decodes to exactly the entry that was written, and writing that
again gives the same bytes, so a device replaying the action writes the same
change. A keystroke that continues its gesture writes only what differs
(`tilo.cont/1 <gesture> <path> <payload>`); readers fold it into the
gesture's first entry.

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
  the app crate (ids, footprints, paths, layouts and types, one line per
  field; labels and descriptions stay in the code); `tilo-app check` and CI fail when code and file disagree. A
  rename is allowed and shows up in review. The packaged surface is also what
  a receiver without the app decides its changes against.
- **Storage changes are migrations.** `#[resource(version = N, migrate = path)]`
  declares the current schema version; `migrate` converts a document from any
  older version, reading old shapes with the model helpers. It runs once, as a
  housekeeping action with an actor and seed derived from the record and the
  versions, so two devices migrating concurrently produce the same change.
  `tilo-app check` fails when the schema in `surface.toml` changed
  incompatibly without a version bump.
- **An unreadable item is quarantined; only an unreadable record fails.** One
  item of a `Collection` that does not read as its type (a required field
  absent, a value of another shape) never makes the whole app unopenable: it
  is left out of the collection, which reads and shows the rest, and
  `Collection::quarantined()` lists it with why. It stays in the document as
  it is, through every write, until it is repaired: pushing a whole item under
  its id stores every field, `Collection::discard` (or the remove-item edit)
  deletes it, and a migration may rewrite it. A record that does not read
  outside its collections (a field of the record itself, a newer storage
  version) shows an error state in the view and its actions report why they
  are unavailable; stale state is never shown as current.
- **The same item made twice is one item.** Two replicas pushing one fixed id
  concurrently each make an entry map, and the document keeps both under the
  key. A read that finds the winning copy incomplete reads the copies as one
  item, each field from the first copy holding it, and the next write stores
  it whole over the winning copy. A compared write never stores half an item:
  an item whose entry or value is gone from the document is written whole, and
  a field edit into an item holding no value is refused. An app that creates
  the same items on every replica (a starter) makes them under fixed ids with
  the same values, idempotently, so every replica's copy is the same item; or
  in its genesis (`ResourceApp::genesis`), one identical change on every
  device.
- **Old history keeps rendering.** An entry naming a field or verb that no
  longer exists renders through the framework fallback.

## 7. Testing

- `Replica<R>` applies actions through the same door as the host: permission,
  kind, confirmation and history entries included.
- `TestApp` drives the real component runtime: invoke, confirm, undo, roles and
  grants, actors with kinds (`as_agent`, `for_person`), and asserts on
  structured history entries.
- `tilo-app check` runs the surface, binding, Fluent and lint checks.

## 8. The old APIs are gone

Apps and examples once held code written against the APIs this model
replaced: hand-written `impl Resource for`, `ActionDecl::new`, `ActionAddress::new`,
untyped argument parsing (`args.text(..)`, `args.get(..)`), `ActionSpec::`
descriptors, direct Automerge writes (`put`, `put_object`, `insert_object`,
`splice_text`, `update_text`, `increment`) and `Json<T>` fields, each one
JSON string. None is left, `Json<T>` itself is gone,
and `tools/policy/check_app_resource_action_contract.py` refuses each one in
every source file of an app crate (`src`, `tests`, `examples`, `benches`) and
of an example: there is no ceiling to raise. What to write instead:

- **A resource** is a `#[derive(Model)]` record with `#[resource]`; a layout
  the storage table does not cover is a `Model` written by hand in the SDK
  (`RichText`, `Grid`), never an app's `impl Resource`.
- **An action an app addresses** is named through its typed reference:
  `R::VERB.on(resource)`, `effect.address_on(resource)`, `invoke.address()`.
  One a caller holds a published resource or tree for comes from it:
  `resource.action_address(id)`, `door::published(&tree, resource, id)`. A test
  invokes through the door the way an agent does (`call_on`).
- **Arguments** are a verb's typed params, an effect's fields, or a serde
  struct the bound arguments are read into; a registered read publishes a
  typed `JsonSchema` parameter struct (`semantic_document_read::read_action`).
- **A stored value** is a model type chosen for its merge (§1): a struct, an
  enum (a data enum is a map naming its variant), `Collection`, `Map`, `Set`,
  `RichText`, and `serde_json::Value` for JSON another party shapes. A value
  that is genuinely opaque — written whole, never merged, such as a retained
  import tree compared whole on retry — implements `Model` through
  `json_model!` with a comment saying why.
- **A document change** is a field edit or a verb. A migration
  (`#[resource(version, migrate)]`) and a test's fixture of an older shape
  write through the model's storage helpers: `write_key`, `Slot`,
  `evolution::retype_json` (an old JSON-string field rewritten as its model
  type), `evolution::collection_items` and `evolution::fixture`.

Three apps act on other apps' published actions as data rather than through a
resource of their own: the agent (it plans and invokes what the broker's
catalogue offers), Shortcuts (its steps name other apps' actions) and the web
search service (it publishes its actions straight to the broker). They build
action addresses; every other rule holds for them as for any app.
