# Decisions where the design was open or the platform differed

Each entry is the closest working design to `docs/designs/app-language` (implementation 7, layout).

## State and actions

- **`DynEdit`, not a bare `Edit<DynRecord>`.** A `DynAction` carries `DynEdit`s: `Create` (a
  resource's JSON form), `Edit { id, edit: Edit<DynRecord> }` and `Remove`. The edit's path starts
  at a *field* of the record; the resource id is the edit's address. `tilo_resource`'s
  `Edit::apply` addresses the document root, and a room record holds many resources under
  `tl/<app>/<id>`, so the resource must be named next to the edit.
- **Schemas come from metadata, not from the document.** Every field edit is dispatched by merge
  kind, which only the schema knows. The registry (`schema.rs`) is filled from a package's metadata
  by `MetaExt::register_schemas`. A peer without the wasm module still needs the package's
  *metadata* (it ships with the surface), never its code.
- **Composite registers.** A register whose type is not Int, Float, Bool, Text or an enum (an
  option, a list, a reference) holds its runtime value as structured JSON in the document; the
  store reads it back as `Scalar::Json` because the schema marks the field `composite`. Scalars
  are native automerge scalars. An unset register (`null`) reads as absent.
- **The runtime plans on a mirror.** The runtime expects its commits to be visible at once, but
  the room record belongs to the framework. `TiloStore` applies a transaction's mutations to a
  copy of the record, queues the recorded `DynAction`, and the host commits it through the door.
  When the record's heads move, the mirror is replaced by a copy and subscribers are told. One
  transaction may be open at a time (a rollback discards the mirror's open change).
- **Press goes through a `commit` action.** A press runs in the runtime (`ViewInstance::press`,
  whose action reference is not reachable from outside), which queues a `DynAction`; the window
  then invokes the host's `tl:host/commit(ticket)`, whose build closure encodes the queued action.
  An agent calling a published record action takes the other path: the build closure plans with the
  runtime and the door commits what it returns. Both end in the executor, so both write the
  change's `ActionContext`.
- **Closures reach the host through a thread-local.** The door's closures are `Send + Sync`; the
  runtime is not. They hold a `HostHandle` (an id), which finds the host in a thread-local on the
  thread that built it, and answer an error from any other thread.
- **Per-room roots have a fixed id** (`root-<type id>`), so two replicas that open an app for the
  first time concurrently create the same resource.
- **Ids.** Items inserted by a planner take explicit ids, so replays never mint any.
- **Authorship.** The chain and module go into the `DynAction`'s arguments and into history through
  `Action::describe` (the verb is named by the action's id and the resource kind). A dedicated
  `ActionFacet` would need a hook in the executor's context; `Authorship::facets` builds the
  facets for when a host can attach them.

## Authority

- The invoker's own verdict is `Allow` in the window: permission to change the room is decided
  where the change is committed (the door refuses a viewer's `commit`). The seam is
  `TiloAuthority::new(invoker_verdict)` for a host that wants the verdict earlier.
- A foreign author in the chain asks (the runtime's rule); the Ask bar offers Allow, Decline and
  Always allow (trust the code that asked). The bar shows the action and who asked. A proposal
  view in the bar is follow-up.

## Not yet stored

- **Outside effects.** `outside` queues an entry in the document in the runtime's reference store.
  The room record has no outbox field for tilolang resources yet, so `TiloStore` refuses
  `OutboxPush`.
- **Flow journals** are kept in memory, so a suspended flow does not survive a restart.
- **Transaction history** per resource (`Store::history`) is the in-memory list of this session.

## Surface

- One `surface.toml` for the host's app id, generated from the loaded packages' metadata: the
  `tl.host` kind, one `tl.<app>.<Type>` per declared type (verbs: the actions whose first
  parameter is that type, plus `edit-text` and `edit-flag`), and one `tl.<app>` per package for
  its record-less actions. Verb ids are the declarations' ids; labels are their names.
- `run-block` and `run-flow` are declared Undoable (the strictest class a block may have short of
  Outside); the runtime's authority narrows them per invocation. `catalogue` is Read.

## The host app and the layout primitives (`apps/tilolang`, `ui-core`'s `widget::arrange`)

- **Package name.** The app crate is `tilolang-app` (binary `tilolang`, app id `so.tilo.lang`): the
  compiler crate owns the name `tilolang`.
- **No UI graph.** The view tree is rendered straight onto the shared layout primitives and the
  toolkit's controls by a keyed reconciler (`reconcile.rs`); the UI graph's `Column`/`Row`/gap
  nodes cannot express `grid`, `split`, `page` or `layers`. A node with the same key and kind keeps
  its widget (focus, scroll position, text state); text, input and checkbox values live in shared
  state, so a changed value rebuilds nothing.
- **`min:` and `max:` bound a node's inline size** (its width); `fill` takes the container's main
  axis by weight and stretches across; `span:` and `at:` are read by `grid` and `layers`.
- **`split` collapse.** `stack` is complete. `drawer` and `navigate` hide the other pane (the drawer
  opens inline under the primary pane; there is no overlay drawer yet) and keep their state in the
  split (`show`, `set_drawer_open`); the affordance that changes it (a back control, a drawer
  button) is not drawn yet.
- **`space`.** The host measures the body's offered size after layout, applies the class
  hysteresis, and tells the open view (`ViewInstance::set_space`); a view that read a crossed
  class re-runs on the next frame, one frame after the layout that crossed it, and only the root
  boundary is fed (the runtime inlines nested views). Re-running inside the layout solver would
  change topology there, which the toolkit forbids.
- **Not yet drawn or honoured:** `lines:`/`truncate`, `image` (its alt text shows), a proposal
  view in the Ask bar, the `embed` block and the hover disclosure of provenance. Keyed position
  and size changes jump instead of animating on the motion springs: the toolkit has no retained
  transition for a re-parented widget, so that is follow-up work.
- **Presses are published view actions** (`Button::on_view`), so an agent or a screen reader
  reaches them like any control. The press runs in the runtime, which plans the change; the
  window commits it through the `tl:host/commit` action.
- **Run block, run flow and catalogue are not published.** They are effects, and the toolkit's
  operation handler cannot be named outside it, so an app cannot handle them. The runtime has the
  operations (`Runtime::run_block`, `run_flow`, `catalogue`); publishing them needs the toolkit to
  export the handler or a typed effect for component apps.
- **The resource/action audit exempts the host** by a checked dependency on this crate
  (`DYNAMIC_RESOURCE_HOSTS`): its resources and actions are described at run time from package
  metadata, so its own source holds none of the values the audit looks for.
- **Unmeasured:** the release size of the host (`tools/binary-budget.toml`, the tapp budget) and
  the first-frame time of compiling the built-in packages.
