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

## Gaps in the design

- No descending `sort_by`, no `list_set`, no reference-valued maps.
- A removed note leaves a dangling reference that a view cannot test for (no existence check).
- The authority chain ignores the authors of resource-reference arguments.
