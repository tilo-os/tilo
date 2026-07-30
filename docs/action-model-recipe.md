# Building an app on the resource + action model

Every collaborative operation in a qOS app is an **action**: a small serializable
value that deterministically mutates the shared Automerge document. Actions are
the single path by which app state changes — so undo, multi-peer replay, and the
Ctrl+K command palette all work from one implementation, for free.

This is not optional plumbing layered on top of a "real" event handler. It is the
event handler. A button, a context-menu item, a swipe, and a Ctrl+K row that all
do the same thing dispatch the **same** action.

The reference implementation is `apps/ebook/src/actions.rs` (Books). Read it
alongside this doc; everything below is drawn from it.

## The one rule that decides everything

Split every interaction into exactly one of two buckets:

- **Document-mutating** → an `Action`. It changes the shared CRDT (creating a
  highlight, editing a note, renaming a track, deleting a row). It must replay
  byte-identically on a peer, so it crosses the executor.
- **Presentation / view state** → plain app code, *never* an action. Which book
  is open, the scroll position, the current selection, an overlay being open.
  Nothing to replicate; dispatching it as an action would be wrong.

If you find yourself wanting an "action" that opens a panel, it is not an action.

## The five pieces (all in `apps/<app>/src/actions.rs`)

### 1. A `type_id` and an action enum

```rust
pub const EBOOK_ACTION_TYPE: &str = "ebook.action";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EbookAction {
    EditAnnotationNote { id: String, note: String },
    DeleteAnnotation   { id: String },
    AddNote            { book_id: String, id: String, text: String },
}
```

One variant per document-mutating operation. Payloads carry **everything** the
mutation needs — including any freshly-minted id (see determinism below).

### 2. `impl Action` — the deterministic mutation

```rust
impl Action for EbookAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> { /* mutate doc */ Ok(()) }
    fn locus(&self) -> ExecutionLocus { ExecutionLocus::ClientLocal }
}
```

**`apply` must be pure w.r.t. the payload.** No `Uuid::new_v4()`, no
`SystemTime::now()`, no RNG, no reading global state — those make replay diverge.
Mint ids and timestamps at *authoring* time and put them in the payload
(`AddNote { id, .. }`), so the same envelope replays identically on every peer.
A malformed payload is a **no-op**, not an error, so a bad replay can't wedge.

`locus`: `ClientLocal` for personal marks/settings; `ServerPreferred` for
operations a server should order authoritatively (fall back to local).

### 3. Registration

```rust
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<EbookAction>(EBOOK_ACTION_TYPE);
}
```

Called once at app start so the palette and remote replay can decode the type.

### 4. The resource tree (`provide_resources`)

A `Resource` is something the app currently has *open* — and resources nest. The
book owns its annotations; a highlight owns its comments. Each exports `ActionSpec`s.

```rust
let mut root = Resource::new(format!("ebook.book:{book_id}"), "ebook.book", title);

// book-level action that needs an argument → prompted (palette shows an input step)
root = root.with_action(ActionSpec::prompted(
    "add-note", "Add note", "Note", "your note…", ExecutionLocus::ClientLocal,
    move |text| encode(&EbookAction::AddNote { book_id: bid.clone(),
                                               id: Uuid::new_v4().to_string(),  // minted HERE, once
                                               text: text.into() }),
));

for a in annotations_for(doc, book_id) {                    // nested sub-resources
    let child = Resource::new(format!("ebook.annotation:{id}"), "ebook.annotation", label)
        .with_action(ActionSpec::prompted("edit", "Edit note", …, move |t| encode(&EditAnnotationNote{…})))
        .with_action(ActionSpec::immediate("delete", "Delete annotation",
                     EBOOK_ACTION_TYPE, &EbookAction::DeleteAnnotation { id }).unwrap());
    root = root.with_child(child);
}
vec![root]
```

- `ActionSpec::immediate(id, label, type_id, &action)` — fires a pre-encoded action.
- `ActionSpec::prompted(id, label, arg_label, placeholder, locus, |text| …)` —
  the palette collects one argument, then the closure builds the action. The
  closure is presentation-side; only its `EncodedAction` output crosses a boundary.

### 5. Expose it from the widget

```rust
impl ResourceProvider for MyApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        match self.open_thing() { Some(x) => provide_resources(&x, doc), None => Vec::new() }
    }
}
```

`Widget::resources()` is the hook the shell's one Ctrl+K palette
(`ui::ActionPalette`) walks. Return an **empty** tree when nothing is open — a
steady, level-triggered value, recomputed each frame.

## Wiring existing UI to the same actions

The point is a single source of truth. A toolbar button, a context-menu row, and
the Ctrl+K entry must all resolve to the **same** `EncodedAction`, dispatched the
same way (the host records the undo boundary and attributes the author). Don't
leave the button calling `self.delete_annotation(id)` while only Ctrl+K goes
through the action — route the button through the action too.

Content *search* is a separate hook: `Widget::search_providers()` folds a
`SearchProvider`'s hits into the same palette as their own group, and
`Widget::activate_search_result()` handles the chosen row as **navigation**
(presentation), not a dispatched action. Use it to make records findable
(contacts, tracks, files); use actions for things that *change* the document.

## Proving it (the covenant applies)

Assert on the real path — never a tautological helper, never `#[ignore]`, never a
weakened assertion:

- Dispatch the action through the envelope and assert the doc changed **and** it
  is recorded as one undo boundary.
- Replay the same `EncodedAction` on a second, independent doc and assert the two
  converge byte-identically (this is the collaboration guarantee).
- Assert the toolbar/menu affordance constructs the **same** `EncodedAction` the
  palette does — one source of truth, proven.

## Adoption status (2026-07-30)

Landed: `qos_actions` spine; the unified Ctrl+K palette on it; adopters
`ebook` (annotation actions — notes; highlight/comment nesting still to come),
`tube`, and `contacts` (as a search provider). Every other app still mutates its
CRDT directly and exports no resources — those are the migration targets.
