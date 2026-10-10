# Building an app on the resource + action model

Every collaborative operation in a Tilo app is an **action**: a small serializable
value that deterministically mutates the shared Automerge document. Actions are
the single path by which app state changes — so undo, multi-peer replay, and the
Ctrl+K command palette all use the same implementation.

The action is the event handler. A button, a context-menu item, a swipe, and a
Ctrl+K row that all do the same thing dispatch the **same** action.

The reference implementation is `apps/ebook/src/actions.rs` (Books).

## Classify each interaction

Classify every interaction before implementing it:

- **Document-mutating** → an `Action`. It changes the shared CRDT (creating a
  highlight, editing a note, renaming a track, deleting a row). It must replay
  byte-identically on a peer, so it crosses the executor.
- **Presentation / view state** → plain app code, *never* an action. Which book
  is open, the scroll position, the current selection, an overlay being open.
  Nothing to replicate; dispatching it as an action would be wrong.
- **A reusable resource view** → a `ResourceViewSpec` bound to explicit
  `OperationKind::View` actions. “Editor”, “Reader”, “Map”, and “Timeline” are
  stable presentations another workspace surface can discover and open; a view
  may also declare how a host mounts it. The declaration is shared; caret,
  scroll, zoom, selection, and which view is locally selected remain local
  presentation state.

An operation that only opens a panel belongs to presentation state.

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

When the resource has a reusable presentation, declare it independently from
its ordinary actions. The stable view id is not the action id:

```rust
root = root.with_view(
    ResourceViewSpec::new("reader", "Reader", "open-reader")
        .with_primary(true)
        .with_mount(ResourceViewMountSpec::new(
            "mount-reader",
            "slot_capability",
            "slot_view",
        )),
);
```

`open-reader` and `mount-reader` are still permission-checked actions, but the
view catalogue is the authority for what counts as a composable view. Do not
infer views from `OperationKind::View`, parameter names, or an action named
`open`. Sibling views need distinct, localised human labels.

- `ActionSpec::immediate(id, label, type_id, &action)` — fires a pre-encoded action.
- `ActionSpec::prompted(id, label, arg_label, placeholder, locus, |text| …)` —
  the palette collects one argument, then the closure builds the action. The
  closure is presentation-side; only its `EncodedAction` output crosses a boundary.

### 5. Expose it from the widget

The app is a `tilo_app::ResourceApp`; its tree comes only from
`tilo_resource::publish`, which the framework's resource root serves through
`ui::resources::PublishesResources`. Do not hand-build a second
`tilo_actions::Resource` tree for the same document state.

`Widget::resources()` is the hook the shell's one Ctrl+K palette
(`ui::ActionPalette`) walks. Return an **empty** tree when nothing is open — a
steady, level-triggered value, recomputed each frame.

## Wiring existing UI to the same actions

A toolbar button, a context-menu row, and
the Ctrl+K entry must all resolve to the **same** `EncodedAction`, dispatched the
same way (the host records the undo boundary and attributes the author). Don't
leave the button calling `self.delete_annotation(id)` while only Ctrl+K goes
through the action — route the button through the action too.

**You do not write the dispatch plumbing.** The toolkit owns it. Embed one
`ui::ActionHost` field and hand it your domain action; it brackets the undo
boundary (captures `doc.get_heads()` once, on the event's first action), encodes
under your `type_id`, remembers the last action for the covenant test, and
applies it through the executor (pinned actor + committed change, so it replays
byte-identically on peers). There is no per-app `perform_action`, no
`DiscardTransport`, no `action_commit_before` / `last_dispatched` fields — those
were identical in every app and now live once in `ui::ActionHost`.

```rust
struct MyApp {
    actions: ui::ActionHost,   // the ONLY action-dispatch state an app holds
    // …
}

// At every affordance (button, swatch, context-menu row) that mutates the doc,
// dispatch the SAME action its ActionSpec exports — one source of truth:
self.actions.dispatch(doc, MY_ACTION_TYPE, &action);

impl Widget for MyApp {
    // The host drains this after each event and records exactly one undo
    // boundary bracketing it — the same path a Ctrl+K palette dispatch takes.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}
```

`ActionHost::last_dispatched()` exposes the last
`EncodedAction` for the covenant test below (affordance == `ActionSpec`).

Content *search* is a separate hook: `Widget::search_providers()` folds a
`SearchProvider`'s hits into the same palette as their own group, and
`Widget::activate_search_result()` handles the chosen row as **navigation**
(presentation), not a dispatched action. Use it to make records findable
(contacts, tracks, files); use actions for things that *change* the document.

## Verification requirements

Assert production behavior without tautological helpers, `#[ignore]` tests, or
weakened assertions:

- Dispatch the action through the envelope and assert the doc changed **and** it
  is recorded as one undo boundary.
- Replay the same `EncodedAction` on a second, independent doc and assert the two
  converge byte-identically (this is the collaboration guarantee).
- Assert the toolbar/menu affordance constructs the **same** `EncodedAction` the
  palette does — one source of truth, proven.

## Adoption status (2026-09-01)

The `tilo_actions` spine, unified Ctrl+K palette/action door, and framework
`ActionHost` are broadly adopted across the app catalogue. Generic dispatch
retains the document actor, infers app ownership when a caller omits it, and can
inherit ambient purpose/work-group context from a task or agent run while
preserving explicit per-action fields.

Native co-edit editors are the deliberate exception to coarse action replay:
splitting an atomic text splice into a second `RemoveBlocks`-style action can
duplicate or reorder collaborative edits. `Widget::take_commit_context()` lets
Writer and Typst stamp the native commit boundary with the same durable
`ActionContext` without changing the edit grain or actor. Presentation caches
and transient settings remain non-semantic; durable settings controls attach
pane-specific context instead of blanket-labeling mixed state.

## Where the model does not reach yet: text editing

An app's *content* is not addressable the way the shell's surface is. There is
no externally reachable "insert this text at the caret" — `ActionHost`
dispatches in-process — so anything driving an app from outside has to place a
caret by coordinate and send keystrokes.

That is the intended direction of travel, not a permanent shape: **text editing
should become paragraph- (sub-resource-) addressed actions**, so that changing a
document is an invocation against a named part of it rather than a gesture aimed
at a pixel.

This distinction matters because driving failures have landed in the coordinate
path rather than the action path: during demo work, a click at a
remembered point landed on the style panel and moved focus off the page, so a
"passing" proof proved nothing for hours; a caret was aimed at a coordinate that
stopped being the page once a second window opened beside it; tiles turned out
to be registered under two names, so an exact-name lookup found nothing while
the window was plainly on screen. The enumerate-and-invoke path — a model
choosing from an app's exported catalogue — worked first time and never regressed.

Pointer gestures are a different case and are legitimately coordinate-based: a
drag *is* a path across the screen, and clicking a file row *is* pointing at it.
An edit to a paragraph is not.

This work is not scheduled. The limitation and rationale are recorded here for
future implementation.
