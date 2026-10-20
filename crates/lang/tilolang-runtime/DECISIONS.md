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
- **Reactivity.** Local commits notify with the exact fields; sync notifies the whole document.
  A view re-runs when a field it read, or a `space` value it read, changed.
- **`space`.** Read through `space_i` (classes, flags, enums as ints) and `space_f` (width and
  height in rem). In v1 nested view calls are inlined by the compiler and inherit the root's
  space, so `set_space` consults only the root boundary.
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

- Compiled blocks stay in the engine for the runtime's life: no eviction.
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
  shows the opened window in place, through the default view of `r`'s type, with a back button
  that returns to the view it was opened from (`apps/tilolang/src/page.rs`). Opening never
  creates a second window for a resource: there are no windows yet, only that trail.
- **Roots.** The host opens `roots[0]` of a package through its default view, over the per-room
  resource of that type (`tilolang_tilo::host::Package::main`), replacing the old `view main`
  convention.
