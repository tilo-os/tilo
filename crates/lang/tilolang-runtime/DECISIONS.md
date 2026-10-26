# Decisions where the design was open

Each entry is the simplest choice consistent with `docs/designs/app-language`.

- **Store trait.** A transaction is an owned token (`Txn`) and the operations are store methods
  (`begin/apply/commit/rollback/read`), not `Txn<'_>` borrowing the store, so the host keeps the
  store while guest code runs. The store speaks in `Mutation`s and `FieldValue`s keyed by field
  id and merge kind; it never sees types. Documents join a transaction when first mutated.
- **`AmStore` is a handle.** Clones share the documents (`Rc<RefCell>`), so a runtime and a test
  both hold one, and `sync_with` syncs the union of both stores' documents.
- **Structural fields exist from creation.** Counters, text, collections (`$order`), sets, maps
  and the `$outbox` are created with the document or item, so concurrent first writes merge.
- **Registers.** Int, Float, Bool and Text registers are native scalars; every other runtime value
  (option, list, tuple, reference) is JSON bytes. A float counter is an Automerge counter in
  millionths.
- **References.** A field of record type holds a reference (a register); records are nested
  inline only as `Collection` items. A reference is `doc` plus `/field:item` steps. References
  read from storage or `ref_lit` are checked against the stored `$type`.
- **ABI suffixes the design left open.** `tuple_set_*/tuple_get_*(t, idx: i32, ..)`,
  `rec_set_*(rec, field, x)`; `res_set_h` on a text field is `res_text_set`. Bool fields use the
  `_i` imports; the host converts by the field's declared type.
- **Metadata** is the compiler's `tilolang::PackageMeta`, read through `meta.rs` (which adds the
  merge-kind and wasm-representation helpers). Type names resolve by app: an unqualified record
  name belongs to the package's own app, a qualified one to the loaded package of that app.
- **Module identity.** The module hash is the blake3 of the wasm bytes; `module_hash` in the
  metadata (the compiler's SHA-256 of the module without metadata) is not used for identity.
- **Instances.** One per module, but a call into a module that is already running (a flow
  invoking an action of its own module) takes a second instance from the pool. Guests keep no
  state between calls.
- **Handles.** A handle encodes its frame's serial, so a number from another invocation or module
  is refused. Retained handles (action references in a view tree) live in a separate table.
- **Authority chain.** Code authors only: for a press, the author of the action reference; for a
  flow, the chain it started with plus each action reference's author, consecutive duplicates
  removed. A direct call has no author, so is `own`. For an own action the declaring module
  counts as trusted, other authors still need trust (minimum along the chain).
- **Flows.** `invoke` returns an error value for Deny and for an action that trapped (journaled).
  A standalone suggestion (a press that Asks) is a one-entry journal with no flow;
  `resolve(run, approver, Approve|Decline)` handles both. Approval runs the action with the
  original invoker and chain, plus `approved_by`.
- **Edits.** `ViewInstance::edit` goes through `Authority` as a pseudo action `$edit` (own when
  the view's module declares the resource's type) and returns `NeedsApproval` on Ask.
- **Outbox.** `outside` queues an entry on the document of the action's first resource argument;
  only the `"post"` capability exists, and only an action classed `outside` may call it.
- **Reactivity.** Commits and syncs notify with the exact fields that changed (a `FieldKey` is
  the resource, an item path included, and the field id); a notification with no fields means
  the whole document. A boundary re-runs when a field it read, or a `space` value it read,
  changed; no other boundary runs. See "Performance by default" below.
- **`space`.** Read through `space_i` (classes, flags, enums as ints) and `space_f` (width and
  height in rem). Every view call is a boundary, and each is offered the root's space, so
  `set_space` consults only the root boundary.
- **Projection** needs the store to read fields, so it is `Runtime::project(value)`, not a free
  function of `(value, meta)`.
- **Text diff.** Prefix and suffix trimmed, then LCS while the middle is under about a million
  cells, else one splice of the middle.

## Compiler and runtime together

- **Boxed scalars.** `invoke` returns a scalar action result boxed like `some_i`/`some_f`; `set_add`,
  `map_put`, `outside` and `model_call` accept scalars boxed the same way. A real `Option<Int>`
  passed as a payload is indistinguishable from a boxed `Int`, so payloads unbox it.
- **New imports** (compiler and runtime agree, pinned by `tests/abi_parity.rs`): `map_get_h`,
  `map_keys`, `list_sort_by_key`, `is_error`, `space_i`, `space_f`.
- **Foreign view nodes.** A node `view:<app>:<view id>` with `arg.<param>` props is rendered as its
  own boundary by the package that declares the view (so its action references are authored by it)
  and replaced by a `fragment` holding that tree. Its reads and `space` reads join the outer
  view's. Nesting deeper than 32 is refused.
- **Blocks.** `Runtime::run_block(principal, &CompiledBlock)` registers the block's module and
  returns a rendered view, an invocation (so a block's action asks unless its module is trusted) or
  a value.
- **Flows as actions.** An action reference to a flow runs the flow; deciding is by the flow's class.
- **Journaled reads.** A flow journals every read of a resource and map, and a replay returns the
  journaled values. This closes the earlier v1 deviation: a replayed flow takes the branches it
  took the first time, even though approved actions changed the resources meanwhile.
- **Dangling references.** A reference read from a field whose target is gone is still a reference
  (it compares equal); only a target of another type is refused. `ref_lit` stays strict.
- **Set of Int/Float order.** Elements are keyed by their text form, so they iterate in text order.

## Embedded blocks and apps

- **Principals.** `Value::Principal` is a host handle tagged like a resource reference. The
  host makes one in `invoker` (the invoker of the running action frame) and when reading back
  a field it wrote; no guest import builds one from text, and a `Text` handle does not fit a
  `Principal` field or parameter. A principal's authorship identity is
  `ModuleHash::principal(id)` (`principal:<id>`); only the host resolves one (an embed's
  `author` prop, an invocation's author). `SimpleAuthority::trust_principal` grants every block
  the principal wrote; `deny_invoker` removes a principal's authority altogether.
- **`embed` nodes** are resolved while the tree is built, like foreign views. The host
  compiles the source with `tilolang::compile_block` against the newest loaded package of
  every app (block modules are never visible), caches the result by the blake3 of the source
  and the set of loaded package hashes, and registers the block module. Diagnostics are cached
  too. The node's `state` prop is `view` (the block's tree, keys prefixed with the embed's
  key), `invocation`, `data` or `error`. Nothing an embed does fails the surrounding view.
  View and data blocks are pure and evaluate at render, under the author's identity; nested
  foreign and proposal views are their own package's code.
- **Action blocks run when posted, never when rendered.** `run_block` queues a step in the
  action frame. After the action commits (`run_action`, so also when it runs as an approved
  suggestion or inside a flow) the host compiles the block, and when it applies an action runs
  it through the authority as the invoker with `[principal:<invoker>]` as the authorship
  chain: it runs, or becomes a suggestion, or is denied. The outcome is created as an
  `Invocation` resource (a host-registered type, `tilo.host`) and stored in the part's
  `invocation` field as two further changes attributed to the poster. A block that does not
  compile, or is not an action, records nothing; the embed shows the diagnostics or renders it.
  The runtime keeps no run-once memo: the invocation resource is the memory, and it syncs.
- **Invocation cards.** An `embed` of an action part only reads the resource: an `invocation`
  child with `state` (`pending` before the host recorded one, then `done`, `suggested`,
  `declined`, `failed`, `denied`), `action`, `app`, `author`, `result`, the `invocation`
  reference, and for a suggestion `run`, `can_resolve` and the action's proposal view. Its
  reads join the view's, so a change to the resource, local or synced, refreshes the card.
  `Core::resolutions` is gone.
- **Deciding a suggestion** is `Runtime::resolve_invocation(invocation, approver, resolution)`.
  It needs no journal, so it works on any replica that has the documents. The authority must
  not deny the approver (`decide(approver, [], action)`; an Ask is the decision being made);
  `Denied` otherwise. Approving runs the action once, on that replica, with the poster as its
  invoker, `[principal:<poster>]` as chain and the approver recorded as `approved_by`, then
  writes `state`, `result` and `run` back. Declining writes `declined`. A flow whose steps
  suspend again keeps `suggested` with the new local `run`.
- **`outside` payloads are plain JSON** (lists and tuples as arrays, a resource as
  `{ref, type}`), not the tagged form registers use; a sink reads them as is.
- **Keys of nested trees are scoped.** A view call (`fragment`) and an embed prefix the keys
  below them with their own key, except anchored nodes, so a key names one node in the tree.
- **Apps** (`apps/tl/<name>/app.tl` and a complete `app.ids`): counter, todo, notes, chat,
  letters and board, each a `root` type with a `default view`. Lists and their items are two
  windows: the notebook, the chat and the letters desk list their items, each row runs
  `open(item)`, and the item type's default view edits it (`note_editor`, `thread`, `editor`);
  no root keeps an "open" item. Filters and sort orders are `window` state (notes' order and
  filter, todo's `Show`, letters' `Shelf`); which note row shows its actions is `lens` state.
  Chat's `post(c, text, block)` records `author = invoker`; a message is a
  `Collection<Part>` (`kind` `text` or `block`, plus the part's `invocation`). Letters' `send`
  has a proposal view and asks as an Outside action. Board holds `Option` references to the
  other apps' root resources and a flow, `morning_cleanup`, that composes foreign actions.

## Gaps in the design

- Compiled modules and blocks stay in the process's module cache: no eviction.
- Two replicas can approve the same suggestion at once: each runs the action before the
  other's state change arrives, so it runs twice. Exactly-once needs a claim that merges
  (for example a register the approver sets before running, resolved deterministically).
- A principal is a plain string id from the runtime's caller. Binding it to a verified
  identity (device keys, the platform's authenticated session) is the platform's job.
- A block that applies a flow records one invocation; later suspensions inside the flow are
  decided with `resolve(run)` on the device that has the journal, and do not update the card.

- No descending `sort_by`, no `list_set`, no reference-valued maps.
- A removed note leaves a dangling reference that a view cannot test for (no existence check).
- The authority chain ignores the authors of resource-reference arguments.

## Development reloads

- **`Runtime::reload_package`** loads a new version of a package and removes every loaded
  version of the same app, so the new one takes over their declaration ids
  (`Registry::replace`). Resources keep their state: they are stored by declaration id. Views
  opened over the old version keep its module until they are opened again. A refused version
  leaves the old one loaded. `load_package` still refuses a second package that claims the same
  ids.

## Enums, view state and windows

- **Enum values** are `Value::Enum(EnumValue { ty, variant, fields })`: the type id, the
  variant id and the payload by field id, as the compiler's enum imports build them
  (`enum_new`, `enum_set_*`, `enum_is`, `enum_get_*`). In registers, journals and change
  messages they are `{"enum": ty, "variant": v, "fields": {..}}`; references inside a payload
  are adopted by the reading module like any stored reference. An enum field the store never
  wrote reads as its declared default (the first variant, or the written default). The built-in
  `Result` is an enum value with the ids `tilolang::RESULT_*`; `try_invoke` builds one from an
  invocation's outcome (the result unboxed, or the failure's reason as text).
- **`value_eq`** compares by value (`value::same_value`): payloads included, references by
  target, never by the module that made the handle.
- **View state lives in the runtime** (`Core::view_states`), keyed by window, by person for a
  lens, and by declaration id; each change bumps the slot's generation. A view records the
  generation of every slot it read and is stale when one moved, so a `set` from one person
  re-renders every view of that window that read the slot, and nobody else's lens. Slots hold
  the guest's handle form (Int, Float and Bool boxed); `ViewInstance::bound_value` unboxes for
  the host. View state is not synced and has no history: it is the window's and the person's,
  not the resource's. Making it part of the workspace resource (shared windows across devices,
  lenses that follow a person) is the workspace window model's job.
- **Windows.** A `ViewInstance` runs in a window: `Runtime::open_window(principal, window,
  view, args)`; `open_view` uses the window of its first resource argument (a window is its
  resource's), and `open_resource(principal, r, view)` opens `r`'s window through `view` or the
  default view of its type (`Runtime::default_view`). A foreign view or a block rendered inside
  a view runs in the outer view's window, so its state is kept with that window.
- **Bindings** say what they edit: `BindTarget::Field` (an attributed change, as before) or
  `BindTarget::State` (the slot, its name and its declared type). An edit of state writes the
  slot, reading text typed into a number's input as that number; it goes through no authority,
  since it changes nothing anyone else owns.
- **Platform actions.** `set(name, value)` and `open(r)` are action references with the ids
  `$set` and `$open` (`host_ui::SET_ACTION`, `OPEN_ACTION`), which no package can declare.
  `ViewInstance::fire` applies `$set` to the slot rendered into the reference (window and
  person included, so a reference only ever changes its own person's lens) and turns `$open`
  into a `ViewEvent::Open { target, view }` the host takes with `ViewInstance::poll_events`.
  Neither goes through the authority or into a journal.
- **`open(r)` on the platform, for now.** Until the workspace window model lands, the host app
  shows the opened window as the next column of its shell, through the default view of `r`'s
  type: beside the root's list, or in place of the root's content with Back; a narrow window
  pushes it (`apps/tilolang/src/window.rs`). Opening never creates a second window for a
  resource: there are no windows yet.
- **Roots.** The host opens `roots[0]` of a package through its default view, over the per-room
  resource of that type (`tilolang_tilo::host::Package::main`), replacing the old `view main`
  convention.

## Performance by default (native-apps.md §4)

What a host relies on. A tilolang app that misses a budget is a bug in the runtime.

- **Every view call is a boundary.** The compiler emits each view call as a `view:` stub; the
  runtime keeps one memo per call (its arguments, the fields and `space` values it read, its
  subtree). A change re-runs the boundaries that read it, and a boundary whose arguments and
  reads are unchanged keeps its subtree without running. Every row of a UI `for` is a
  boundary keyed by its item, whatever its body (below). `ViewInstance::stats()` counts renders, boundaries and unrendered rows.
- **Loop rows.** The compiler lowers a UI `for` (and nothing else) to one node of kind
  `for:<app>:<view>` with the props `items` (the list), `each` (the parameter the item goes to),
  `suffix` (the key suffix of the rows around it, for anchored keys), `arg.<name>` (what the body
  captures) and the sizing props of a one-node body (`fill`, `min`, `max`, `aspect`, `span`,
  `at`, hoisted when they do not use the item). A body that is one call of the app's own view
  with plain arguments calls it directly; any other body becomes a view of its own,
  `<view>~row<n>`, taking the item, the captures and `$key`. The runtime puts one `fragment`
  per item in the loop's place among its parent's children, keyed `<loop key>/<item key>`
  (the item's resource id, else its index), each a boundary (lazy in a `list`) with the loop's
  sizing props. Hosts see fragments, never the loop node.
- **Patch stream.** `ViewInstance::poll_patches()` returns the patches that turn the last tree
  into the new one, in the order they apply, empty when nothing changed (an idle view costs
  nothing: no runtime timer, no polling of its own). Patches address nodes by path, the keys from
  the root down to the node (`parent` for structural patches): `SetRoot(node)`,
  `Insert{parent, index, node}`, `Remove{parent, key}`, `Move{parent, key, to}`,
  `SetProp{path, name, value}` (`None` removes it), `SetBinding`, `SetEvent`, `SetAnchor`.
  Keys are stable across renders, so a host keeps a key-to-widget map and applies each patch to
  one widget. `ViewInstance::tree()` is always the tree after the last patch, for a host that
  rebuilds instead.
- **Virtual lists.** A `list { for ... }` whose rows are view calls renders no row until the host
  shows it. The list node always has one keyed child per row; a row not shown is an empty
  `fragment`. `list_len(list)` and `row_key(list, i)` cost nothing; `row(list, i)` renders the
  row if needed and returns it (the host draws what is returned: showing a row is not patched).
  From then on changes to a shown row come as patches, and rows inserted, removed or moved come
  as patches on the list. `hide_rows(list, keep)` forgets rows outside `keep` and returns the
  patches that empty them; a forgotten row no longer runs. Showing and hiding cost the rows
  concerned, not the list's length. A host that draws the whole tree calls
  `set_lazy_rows(false)` before its first poll and reads `tree()` after it.
- **A card is a summary.** A `list` inside a `card` is the card's preview: the compiler gives it
  `preview` (`tilolang::prims::PREVIEW_ROWS`, 5) and the runtime builds only those rows, with
  `len` holding how many the list has (`UiNode::items_total`). The card's `count:` says how many
  there are and pressing the card opens its resource for the rest. Before, a board's cards built
  and drew every row of three 10k lists.
- **The native host** (`apps/tilolang`). The list column of a `list` frame is a `VirtualList`
  over the list's row keys: it asks for the rows it shows (visible and overscan) with
  `row(list, i)` and forgets the rest with `hide_rows`. A poll's patches are sorted by where
  they show: rows inserted, removed or moved are the list's model changes; a change inside a
  shown row reads that row again and rebuilds it alone; anything else is reconciled in place.
  The `list` of a frame with a composer (a conversation) is a transcript: a `VirtualList` that
  opens at its newest row and follows it while it shows it (`following_end`), its rows the
  view's own, shown and forgotten as the list column's are, so a conversation of 10k messages
  costs the messages on screen. A list inside a page is drawn whole, its rows rendered as they
  appear; one in a card shows its preview. What the room publishes (`Publications`) and what
  search finds (`SearchIndex`) are kept by the live window and updated from the store's
  changes, read off the frame the first time.
- **Patches outside a list apply to single widgets.** The reconciler (`apps/tilolang/src/
  reconcile.rs`) updates a widget in place where it can take the change (a text, a heading, an
  image, a holder, a button's enabled state, an input keeps its own) and reconciles a
  container's children by key through the toolkit's slot operations: `Arranged` (`replace`,
  `insert`, `remove`, `move_slot`, `set_placement`), `Container` and `Scrollable`
  (`replace_child`). A prop change marks the widget alone; an insert, remove or move reports a
  `StructureChange::Described` that the containers above forward (`Card`, `Section`,
  `NavShell`, the form parts), so the surface compares boxes by identity and repaints where
  they moved instead of rebuilding the window. Only a content swapped whole (`Pane::set`, a
  container's own `set_slots`) is a `Rebuild`.
- **Publication follows a change to its depth.** `Publications` republishes where a change
  reached: an item whose own field changed has its label read again, and so do the items
  holding it; a collection whose membership or order changed has its children brought in line
  at the resource holding it, wherever it is; only an item created, replaced or removed as a
  whole is built anew. Before, a change anywhere in a top-level item rebuilt it with
  everything in it: typing a conversation's draft republished its 10k messages (556 ms a
  keystroke). Labels are the store's `first_text` through its read cache, and naming skips a
  collection whose items cannot hold a text (`reads::may_hold_text`).
- **The store keeps what it read** (`tilolang-tilo/src/cache.rs`). Reading a field of an item
  used to walk from the document's root to the item (three map lookups), then to the field: 3
  to 4 µs per lookup in automerge's op set, about 20 µs a field, so a view that filters 10k items
  spent 200 ms. `TiloStore` now keeps per resource where its map is and the fields read, and a
  collection read lists every item with its registers and counters in one walk of each item's
  map, on up to 8 threads for 1k items or more (`collection_entries`/`read_entry` in
  `tilo-resource` split the read so that it can be spread). Every write the store makes forgets
  the keys it touched (a local write, a rollback or rewind of local writes, an incremental sync
  forgets the changed fields; a sync that cannot say clears all), so a read never sees an older
  value than the mirror holds. The runtime's side of a read copies nothing it can borrow:
  `res_get` reads the reference, field and author where the frame holds them, a reference value
  shares its `ResourceRef` (`RefValue::target` is an `Rc`), and a boundary's reads are deduplicated
  by hash and indexed per resource.
- **Store changes for the host.** `TiloStore::watch()` gives a cursor; `Watch::take()` returns the
  `Changes` since the last take: changed fields per document, documents changed anywhere
  (`whole`), or `everything` after the mirror was copied anew. Planning an action (a trial run
  that is rolled back) tells nobody: its notifications are dropped with its writes.
  `TiloStore::stats()` counts incremental and full syncs; a full sync happens only when the
  mirror held a change of its own when the record moved, and should stay at 0.
- **Search and publication are incremental.** `SearchIndex::new(&host)` then
  `update(&host) -> SearchDelta { upserted, removed }` each frame, `hits()` for the index;
  `Publications::new(&host)` then `update(&host, &planner) -> Vec<DocId>` (the documents whose
  published tree changed) and `trees()`. Both consume a `Watch`, so a frame costs what changed.
  A record that appears (every record, on the first frame) is read whole off the frame: from a
  `TiloStore::snapshot()` (one copy of the mirror, shared by every reader until the mirror
  changes) on a thread of its own. Until it ends, `update` returns nothing and takes no changes;
  `set_waker(waker)` gives them the window's redraw request to call when it ends, and the next
  `update` takes the result in. `is_reading()` / `is_building()` say whether that is under way;
  `settle(&host, ..)` waits for it, for a host without frames and for tests. An action without
  parameters is published deferred (`ActionSpec::deferred_on`): planned when it runs, so
  publishing plans nothing. A record's label (its first text, fields in id order, items in order) is read
  again only when a change is at or before what names it. These replace re-indexing and
  re-publishing every record per frame (`search::refresh` and `published()` in the window), which
  cost 322 ms and 393 ms at 10k items.
- **No needless commit.** The host commits the room record when the executor wrote something.
  An empty `commit()` on automerge used to add and remove the actor (6.6 ms at 10k items); the
  vendored patch makes it free, but the call is still not needed.
- **One engine per process.** All runtimes share one `wasmi::Engine` and a module cache keyed by
  the module hash, so a package loaded again (a second window, a reload of unchanged code)
  compiles nothing. Each runtime keeps its own linker, limits and instance pools; an instance
  caches its export functions and their result types, so a call looks nothing up by name.
  `set_translation(Lazy|Eager)` picks how modules are translated, once, before the first
  runtime. `Engine::profile(true)` (via `Runtime::engine()`) counts calls, time in wasm, time
  in the host's imports and compile time.
- **No compiling at launch.** `Host::load_wasm(bytes)` loads a package from its module alone,
  with the metadata the module embeds; a launcher ships built-in packages compiled with Tilo.
  Compiling a package's source takes 0.8 to 1.5 ms (notes, chat; release, aarch64),
  so this saves little time; it removes the compiler from the launch path. wasmi cannot
  serialize translated code, so there is no on-disk cache, and none is needed (below).
- **Engine: wasmi, lazy translation.** Measured with `tilolang bench --profile` (release,
  aarch64, a notes list of 10k items, 20 samples). Time inside wasm, imports excluded, is 28 ms of
  a 2035 ms open (search 454 ms, publication 1429 ms, imports 773 ms) and 0.013 to 0.017 ms of
  a 0.55 ms keystroke (imports 0.10 ms); a peer edit has 0.007 ms of wasm in 0.46 ms. Compiling
  the module is 0.10 ms lazy and 0.17 ms eager; lazy and eager give the same open (2035 vs
  2034 ms) and keystroke (0.54 vs 0.55 ms). Even an engine that ran guest code in no time would
  take at most 1.4% off open and 3% off a keystroke, while every budget the guest's share
  touches is met with wasmi; wasmtime AOT would add a compiler and code cache to the platform
  for that. The time is in the host (store reads behind imports, search, publication), which is
  where the work goes. Revisit when a profile shows guest time as the larger share.
- **Finding a node.** `ViewInstance::edit`/`press`/`fire` find a node in its boundary when its
  key carries one; any other node (the root view's own, an anchored one) by walking the tree
  once, after which where it was is kept and checked before use. Typing into an input of a view
  that also shows 10k rows went from 0.4 ms of lookup per keystroke to none.
- **Bench and budgets.** `tilolang bench <package> [--sizes] [--samples] [--idle SECONDS]
  [--translation lazy|eager] [--profile]` prints one JSON record per scenario and size: open,
  keystroke, peer edit, scroll (packages with a virtual list), idle wakeups and patches, and the
  cost of re-indexing and re-publishing everything for comparison, and how long compiling the
  package's source takes. Records are `tools/perf/record.py`'s JSON.
  Seeding follows the package's root: its collection, or the roots of the packages it refers to
  (a board); a root with neither has one size, of no items. `keystroke` types into an item's
  title in the item's window, or, when items have no window, into the main window's first text
  input (a conversation's draft, when the window edits no title); a package with no text input
  gets `press` (its first button). `peer_edit` retitles an
  item, or bumps the root's counter. `memory_anon` is measured in a process of its own that loads
  the saved room and opens it, as an app process does (the bench's own process keeps what
  seeding allocated). `cargo test --release -p tilolang-tilo --test budgets` holds section 4.1 of
  the design on the apps in `apps/tl`: notes, chat, todo and the board at 10k (open p95 150 ms,
  half the section's 300; keystroke and peer p95 2 ms; scroll p50 1 ms for the lists; no idle
  wakeups) and memory at 1k (60 MiB); and the counter.
- **Where it stands** (release, aarch64, 10k items, 20 samples, p95; the runtime's share, the
  bench drives no window). notes: open 48 ms, keystroke 0.48 ms, peer edit 0.29 ms, scroll p50
  0.09 ms, idle 0 wakeups; ready (search and publication done) 421 ms. chat: open 27 ms,
  keystroke 0.44, peer 0.24, scroll p50 0.21. todo: open 68 ms, keystroke 0.44, peer 0.29,
  scroll p50 0.08. board (notes, todo and chat of 10k each): open 126 ms, keystroke 0.90, peer
  0.53. One conversation of 10k messages in its window: open 52 ms (p50 25), a keystroke in the
  draft 0.4 ms (582 ms before publication followed changes to their depth), scroll p50 0.33 ms,
  a peer's message 1.4 ms (below). Memory at 1k: notes 21 MiB, chat 19, todo 17, board 39. counter: open 0.28 ms, press
  0.12, peer 0.07. Open was 280, 50, 497 and 1265 ms before the store kept what it read and a
  card showed its preview. The board's open is where the time is: three listings of 10k items
  (30 ms on eight threads), 40k field reads from the store's cache (20 ms, mostly the memory
  the reads reach), 30k references built for the items (10 ms) and freed with the frame
  (10 ms), the view's own work (20 ms). On a shared t worker the same open measured 154 ms, and
  more under load: the board meets the budget with little to spare off the laptop. The first
  change of a window costs 5 to 7 ms: automerge renumbers its columns for the new actor (about
  2 ms per document at 400k ops), inherent to its sorted actor table.
- **A change costs the change, not the collection.** One insert, remove or move in a
  collection of N items is work in the items it touched, at every layer:
  - *Store.* `TiloStore` keeps a collection's listing (`Store::listing`: its item ids in order,
    shared as an `Rc<Listing>`) with each item's order key, and brings it in line with the items
    a change touched (their entries put, deleted or given a new key) by binary search over
    (order key, id), instead of forgetting it and listing again. Each change says how its
    listings moved: `ChangeSummary::listings`, steps `Splice::Insert{at, item}` and
    `Splice::Remove{at, item}` (a move is both) in order; a collection named in `fields` without
    steps changed in a way the store could not say. `AmStore` tells the steps of its own
    mutations. A reader checks the steps as it goes (a removal names its item, an insertion
    must not find its item there) and its count against the listing at the end, and rebuilds
    from the listing when they do not fit. The read cache keeps the items cached within a
    resource beside it, so a resource created or removed forgets its own items, not by a look
    at every resource of the document.
  - *Views read collections lazily.* In a view a collection field is a `Value::Coll` (the
    field), not a list of its items: `len()`, an index, `first()` and `last()` (`list_end`, so
    `first()` does not depend on the length) read the store's listing, and each read is
    recorded as what it saw (`CollSeen`: the length, the ends, the item at each index read). A
    change to the collection re-runs a boundary only if something it saw is no longer so, which
    is checked against the listing by value: a conversation's row showing its last message runs
    again for a new message, not for a move among the older ones, and builds one reference, not
    10k. A loop that reads every item from the first to the last depends on the whole
    collection (a read of its field). Wherever the items are needed as values (stored, sorted,
    compared, put into a list, a tuple or a record, given to an action) the collection is read
    whole into a list first, and the view depends on all of it. No author opts in.
  - *Rows follow their collection.* A loop over a collection that is alone among its parent's
    children (a `list`'s rows, a section's) hands its rows to the live tree (`LoopRows`): the
    boundary holding it no longer depends on which items there are. A change's steps insert,
    remove or move rows in place, one `Insert`, `Remove` or `Move` patch each, rendering only an
    inserted row that is not waiting to be shown; the paths of the rendered rows after them move
    along (for a `list`, only the shown ones). Steps that do not fit, or rows then not as many as
    the items, rebuild through the holding boundary as before. The native host keeps where each
    row key was last found and looks around there, instead of indexing every key again after a
    change, and tells the list widget a single change without copying its keys.
  - *Publication and search take the steps.* An inserted item is built, a removed one dropped,
    a moved one kept, in the published children and in the indexed items, checked the same way,
    with the whole membership read as before when the steps do not fit. A published item is
    found where its listing puts it (its order key), not by walking its siblings.

  A peer's message in a conversation of 10k messages, in its window, went from 88 ms to 1.4 ms
  (p95; p50 85 to 1.1 ms): the store's sync 0.4 ms (automerge applying the change and naming
  what it wrote, the same at 1k), the views 0.2 (the row of the conversation in the chat list
  and the new row), publication 0.2, search 0.1. The draft keystroke in that window went from
  1.2 to 0.4 ms. Open, keystroke, peer edit and scroll of notes, chat, todo and the board at 10k
  are where they were, within the run-to-run spread (board open p95 135 ms against 188 in the
  run before; on the shared arm t worker it measures 140 to 168 ms with or without this work,
  around the budget's 150). Memory at 10k is 4 to 6 MiB more for notes, chat and todo and 6 to
  13 MiB for the board (the listings keep each item's order key), 3 to 5%; at 1k, within
  a MiB either way (board 24.3 MiB against 23.5).
- **An action is a type bound to a target.** `tilo-actions` splits what an action is
  (`ActionType`: id, label, description, params, contract, kind, operation type, builder,
  classifiers) from what it acts on (`Target`). An `ActionSpec` is an `Arc<ActionType>` and a
  target, 24 bytes; the builder and classifiers read the target when the action runs. A package
  keeps one `ActionTypes` (its record types' edits and actions, described once), and every item
  binds those types to its id; it is described again when the module or the planner changes.
  Native apps keep their bound, rename, edit and share types per thread, by declaration. Before,
  every item carried owned specs (about 1.7 KB each; an item carries `open`, its edits and its
  type's actions). Memory, before and after (release, aarch64, `memory_anon`): notes 19.9 to
  12.1 MiB at 1k and 175.6 to 112.8 MiB at 10k; todo 16.7 to 12.9 and 147.5 to 114.5; chat
  19.3 to 12.7 and 167.2 to 105.1; the board 49.9 to 34.9 and 486.6 to 327.2. What is left per
  item is the view (1.9 KB per row even unrendered), the room and the mirror (1.5 KB), search
  (0.4 KB), the published resources themselves (a 24-byte spec per action) and what the
  allocator keeps after the first read.
- **Both together** (the split, the store's read cache, card previews and republication at its
  depth; release, aarch64, laptop, 20 samples, p95). Open at 10k: notes 52 ms, todo 67, chat
  31, board 131; a conversation of 10k messages 57 (p50 31). Memory at 10k: notes 128 MiB,
  todo 129, chat 114, board 230; at 1k 14, 13, 12 and 25. Keystroke p95 0.4 to 0.8 ms and
  peer edit 0.2 to 0.5 ms at 10k; a keystroke in a 10k draft 1.4 ms, a peer's message in it
  90 ms; no idle wakeups. Against the split alone, notes, todo and chat hold 9 to 16 MiB more
  at 10k and the board 97 MiB less; where that sits is not measured yet.
