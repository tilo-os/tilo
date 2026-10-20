# tilolang v1 — implementation design

This document describes how to build the first version of the language specified in
[spec.md](spec.md). It is the contract for the people and agents implementing each part. Where it
departs from the spec, the departure is called out as a **v1 deviation**, with the reason.

## 0. Crates

| Crate | Path | Owns | Depends on |
|---|---|---|---|
| `tilolang` | `crates/lang/tilolang` | Lexer, parser, AST with ids, canonical printer, resolver, type and effect checker, capability sets, IR, wasm codegen, metadata. | `wasm-encoder` only (plus `serde`/`serde_json` for metadata). No Tilo crates. |
| `tilolang-runtime` | `crates/lang/tilolang-runtime` | The wasm host built on wasmi. Handle table, host ABI, the `Store` and `Authority` traits, action transactions, the flow journal, view trees and their diff. Includes an Automerge-backed reference store and multi-replica tests. | `tilolang`, `wasmi`, `automerge` (via the workspace's path dependency). |
| `tilolang-tilo` | `crates/lang/tilolang-tilo` | Integration with the Tilo platform: records and room sync, the action door and grants, rendering through ui-core, presence and provenance. | `tilolang-runtime`, Tilo SDK crates. Specified in §7, after the platform research. |
| host app | `apps/tilolang` | One binary that runs any tilolang app package. | `tilolang-tilo` |
| apps | `apps/tl/<name>/` | App packages written in tilolang. | — |

**v1 deviation: wasmi instead of wasmtime.** wasmi is already in the tree (via
`format-provider-abi`), is pure Rust, runs inside the browser build without JavaScript glue, and
keeps compile times down. The runtime hides the engine behind one module (`engine.rs`), so moving
to wasmtime for native builds later is local to that module.

## 1. Source layout of an app package

```
apps/tl/counter/
  app.tl          # the module: types, actions, views, flows, fns
  app.ids         # id lock file; generated, committed
```

The first item of `app.tl` names the app:

```
app so.tilo.counter "Counter"
```

Other packages are imported by app id: `use so.tilo.todo as todo`. Imported declarations are then
reached as `todo.add`, `todo.Item`, and so on.

### 1.1 Ids (spec §2.4)

Every declaration has a stable id: types, fields, actions, flows, views, fns and anchors.

- **v1 deviation.** The canonical AST editor doesn't exist yet. Ids therefore live in a lock file,
  `app.ids`, a sorted text file of `qualified.name = id` lines.
- The compiler assigns new ids for declarations that aren't in the lock. Ids are 8 characters of
  base32 from a random draw at assignment time.
- `tilolang ids --rename old.path new.path` renames a declaration and keeps its id.
- A declaration that disappears keeps its line under a `# retired` section. Its id is never
  reused.
- Storage keys, action addresses, grants and compatibility checks all use ids. Names appear only
  in source, errors and display.

## 2. Grammar (canonical text projection)

The grammar is written as EBNF. `NL` stands for a newline, which separates statements and fields.
Commas are optional at line ends.

```
module     = "app" app_id string NL { item } ;
app_id     = ident { "." ident } ;
item       = use | type_decl | action | flow | view | fn ;
use        = "use" app_id [ "as" ident ] ;

type_decl  = "type" Ident "{" { field } "}" ;
field      = ident ":" type [ "merge" merge_kind ] [ "=" literal ] ;
merge_kind = "register" | "counter" | "text" ;          (* containers merge structurally *)
type       = "Int" | "Float" | "Bool" | "Text" | Ident | qualified
           | "List" "<" type ">" | "Set" "<" type ">" | "Map" "<" type ">"
           | "Collection" "<" type ">" | "Option" "<" type ">"
           | "(" type { "," type } ")" | "Action" | "View" ;

action     = "action" ident params [ "->" type ] [ "proposal" ident ] block ;
flow       = "flow"   ident params [ "->" type ] block ;
view       = "view"   ident params ui_block ;
fn         = "fn"     ident params "->" type block ;
params     = "(" [ param { "," param } ] ")" ;
param      = ident ":" type ;

block      = "{" { stmt } "}" ;
stmt       = "let" ident [ ":" type ] "=" expr
           | place ( "=" | "+=" | "-=" ) expr
           | "if" expr block [ "else" ( block | if_stmt ) ]
           | "for" ident "in" expr block
           | "return" [ expr ]
           | expr ;
place      = ident { "." ident } ;

expr       = lambda | or ;
lambda     = ident "->" expr ;                              (* only as an argument, see §3.6 *)
or         = and { "or" and } ;
and        = not { "and" not } ;
not        = [ "not" ] cmp ;
cmp        = add [ ( "==" | "!=" | "<" | "<=" | ">" | ">=" ) add ] ;
add        = mul { ( "+" | "-" ) mul } ;
mul        = unary { ( "*" | "/" | "%" ) unary } ;
unary      = [ "-" ] postfix ;
postfix    = primary { "." ident [ args ] | args } ;
args       = "(" [ arg { "," arg } ] ")" ;
arg        = [ ident ":" ] expr ;
primary    = literal | ref | ident | "(" expr { "," expr } ")" | "[" [ expr { "," expr } ] "]"
           | "if" expr "then" expr "else" expr ;
ref        = "@" app_id "." ident ":" ref_id [ "@" version ] ;  (* @so.tilo.music.Album:01J8… *)
literal    = int | float | string | "true" | "false" | "none" ;
string     = '"' { char | "{" expr "}" } '"' ;              (* interpolation *)

ui_block   = "{" { ui_stmt } "}" ;
ui_stmt    = ui_call [ ui_block ] [ "#" ident ]
           | "for" ident "in" expr ui_block
           | "if" expr ui_block [ "else" ui_block ]
           | "let" ident "=" expr ;
ui_call    = postfix ;                                      (* call of a UI primitive or of a view *)
```

`//` starts a comment that runs to the end of the line. Doc comments (`///`) attach to the item
that follows and are kept in the metadata (§5); agents read them.

## 3. Semantics and checking

### 3.1 Types

- **Scalars:** `Int` (i64), `Float` (f64), `Bool`, `Text`.
- **Records:** declared with `type`. A record type is a resource type (spec §2.1). As a parameter
  type, a record means a reference to a resource of that type.
- **Containers:**
  - `List<T>` and tuples are plain values. When stored in a field, they are registers.
  - `Set<T>`, `Map<T>` (keyed by `Text`) and `Collection<T>` are only valid as field types.
  - `Option<T>`, with the literal `none`.
- **`Action`:** the type of an action reference, which is an action applied to arguments
  (spec §3.4).
- **`View`:** the type of a view application.
- Generics exist only for the built-in containers. Users can't declare generics in v1.
- Inference is local: `let` without an annotation takes the expression's type. There is no `any`.

### 3.2 Merge kinds (spec §2.2)

| Field type | Default merge | Allowed explicitly |
|---|---|---|
| `Int`, `Float` | `register` | `counter` |
| `Bool`, `Option`, `List`, tuples, references | `register` | — |
| `Text` | `text` (splice) | `register` (an atom, e.g. a tag name) |
| `Set`, `Map`, `Collection`, nested records | structural | — |

Mutations are checked against the field's merge kind:

| Merge | Allowed | Rejected |
|---|---|---|
| `register` | `=` | `+=` / `-=` (error: "+= on a register field loses concurrent updates; use `merge counter` or write the read and the write out"). Writing the read and write out (`x.n = x.n + 1`) is allowed: the intent is then explicit. |
| `counter` | `+=` / `-=` | `=` |
| `text` | `=` (the host diffs and splices) | — |

Container fields are mutated through methods: `.insert(record)`, `.remove(item)`,
`.move(item, index)`, `.add(x)`, `.put(k, v)`.

### 3.3 Effects

Each form has a fixed effect discipline, checked by the compiler:

| Form | May | May not |
|---|---|---|
| `fn` | Compute. Call `fn`s. | Mutate. Call actions, flows or views. |
| `view` | Read resources. Call `fn`s and other views. Build UI. | Mutate. **Inside a view, writing `act(args)` creates an `Action` value. It does not call the action.** |
| `action` | Read and mutate resources of **its own module's types**. Call `fn`s and its own module's actions; that call is inlined into the same transaction. | Call flows, call foreign actions, wait. Actions are synchronous (spec §3.2). |
| `flow` | Invoke any action, own or foreign, with `do act(args)` or plain `act(args)`. Call `fn`s. Read resources. | Mutate fields directly. Every change goes through an action. |

- **v1 deviation:** a foreign action can't be called from an action. One atomic change across
  apps' documents isn't available, so composition across apps happens in flows.
- **Effect class of an action** (spec §3.2):
  - *Read* if it mutates nothing;
  - *Outside* if it calls a platform outside capability (§4.6), directly or through own actions;
  - *Undoable* otherwise.
- The class of a flow, exposed as a capability set, is the union of the classes of every action
  it can reach.

### 3.4 Own and foreign (spec §4.3)

An invocation is **own** when the action is declared in the same module as the code that wrote
the action expression. Otherwise it is **foreign**. The compiler records, for each `Action` value
it creates, the id of the action. The runtime records the *authoring module* on the handle
(§4.3).

### 3.5 Capability sets (spec §4.4)

For every view, flow and action, the compiler computes the set of action ids it can reach, own and
foreign, each with its effect class. These sets go into the metadata. The runtime and the
platform use them for grants and for adoption prompts.

### 3.6 Lambdas and higher-order built-ins

- Lambdas are allowed **only** as direct arguments of built-in methods: `map`, `filter`, `any`,
  `all`, `sort_by`, `find`, `count`. In a view, `for`/`list` takes a ui_block.
- The compiler inlines these as loops, so there are no closures at runtime.
- This is a **v1 restriction**. It keeps codegen free of closure conversion.

### 3.7 Errors

- Errors carry a span, a stable code (`E0xx`) and a message written for an agent to act on. For
  example, "`Album` has no field `chapter`; fields are: title, tracks, plays".
- The checker reports every error it can find, not only the first.
- `tilolang check <dir>` prints errors as `path:line:col: E0xx message`. With `--json`, it prints
  structured output for agents.

## 4. Execution model and host ABI

### 4.1 Values

| Language type | Representation in wasm |
|---|---|
| `Int` | `i64` |
| `Float` | `f64` |
| `Bool` | `i32` (0 or 1) |
| Anything else: `Text`, lists, tuples, `Option` of non-scalars, records-as-references, `Action`, `View`, UI nodes | An `i32` **handle** into the host's value table for the current invocation |

The guest does control flow and arithmetic. Everything structured lives in the host. This is
deliberate:

- codegen stays simple;
- resources never live in a guest heap (spec §6.4);
- authority-bearing values can't be forged (spec §6.2), since a module can only name handles the
  host gave it.

`Option<Int>` and other optional scalars are boxed as handles.

Handles are valid for one invocation. The host clears the value table after every call into the
guest, except for handles the host chooses to keep (resource references and action references
stored in a view tree).

### 4.2 Module shape

The compiler emits one wasm module per app package.

- **Imports:** functions from module `"tl"` (§4.4).
- **Exports:**
  - `memory`, holding string literals as data segments and nothing else;
  - one function per declaration, named by its id: `a_<id>` for actions, `f_<id>` for flows,
    `v_<id>` for views, `n_<id>` for fns. Parameters and results use the wasm types from §4.1.
- **Custom section `tilolang.meta`:** JSON metadata (§5).
- **Flows:** each `do`/action invocation in a flow is a call to the host import `invoke`. Replay
  is implemented in the host (§4.5). The guest needs no state machine.

### 4.3 Handle table and authorship

- Every handle in the host table carries its type tag.
- Resource reference handles and action reference handles also carry the **authoring module**:
  the content hash of the module whose code created the handle.
- A handle passed into another module keeps its original author (spec §4.3, point 1).

### 4.4 Imports (`"tl"` module)

All imports trap with a message on misuse: a wrong tag, an unknown field, a mutation outside an
action. The compiler never emits a call that can trap from a well-typed program, except for the
`ref_lit` resolution failures listed below.

```
;; text
str_lit(ptr: i32, len: i32) -> h            ;; from a data segment
str_concat(a: h, b: h) -> h
str_eq(a: h, b: h) -> i32
str_len(a: h) -> i64
int_to_str(x: i64) -> h
float_to_str(x: f64) -> h
str_cmp(a: h, b: h) -> i32                  ;; -1 / 0 / 1

;; lists, tuples, options (values)
list_new() -> h
list_push_i(l: h, x: i64)    list_push_f(l: h, x: f64)    list_push_h(l: h, x: h)
list_len(l: h) -> i64
list_get_i(l: h, i: i64) -> i64   list_get_f(l: h, i: i64) -> f64   list_get_h(l: h, i: i64) -> h
tuple_new(n: i32) -> h ;; then tuple_set_*(t, idx, x) / tuple_get_*(t, idx)
some_h(x: h) -> h   some_i(x: i64) -> h   some_f(x: f64) -> h   none() -> h
is_some(o: h) -> i32  unwrap_i(o: h) -> i64  unwrap_f(o: h) -> f64  unwrap_h(o: h) -> h

;; resources: field is the field id, as a string literal handle
res_get_i(r: h, field: h) -> i64   res_get_f(r: h, field: h) -> f64   res_get_h(r: h, field: h) -> h
res_set_i(r: h, field: h, x: i64)  res_set_f(r: h, field: h, x: f64)  res_set_h(r: h, field: h, x: h)
res_add_i(r: h, field: h, d: i64)  res_add_f(r: h, field: h, d: f64)        ;; counter
res_text_set(r: h, field: h, s: h)                                         ;; host diffs and splices
coll_insert(r: h, field: h, init: h) -> h      ;; init = a record value; returns the new item's reference
coll_remove(r: h, field: h, item: h)
coll_move(r: h, field: h, item: h, index: i64)
set_add(r: h, field: h, x: h)  set_remove(r: h, field: h, x: h)
map_put(r: h, field: h, k: h, v: h)  map_remove(r: h, field: h, k: h)
rec_new(type_id: h) -> h  ;; then rec_set_*(rec, field, x): builds the init value for coll_insert
ref_lit(app: h, type_id: h, id: h) -> h        ;; resolves @ref literals; traps if the type doesn't match
ref_eq(a: h, b: h) -> i32
ref_id(r: h) -> h                              ;; the reference as text, for display or keys

;; actions
act_ref(action_id: h, args: h) -> h            ;; args = a list handle; the author is the calling module
invoke(act: h) -> h                            ;; flows only; returns the action's result, or a unit handle
outside(capability: h, payload: h) -> h        ;; platform outside capability (§4.6); actions only
model_call(kind: h, input: h) -> h             ;; nondeterministic and journaled; flows only

;; ui (views only)
ui_node(kind: h, key: h) -> h                  ;; kind: "column", "row", "text", "button", ...; key = a stable id
ui_prop_h(n: h, name: h, v: h)   ui_prop_i(n: h, name: h, v: i64)   ui_prop_f(n: h, name: h, v: f64)
ui_child(parent: h, child: h)
ui_bind(n: h, r: h, field: h)                  ;; two-way binding: an input or checkbox edits this field
ui_on(n: h, event: h, act: h)                  ;; e.g. "press" -> an action reference
ui_anchor(n: h, anchor_id: h)

abort(msg: h)                                  ;; runtime error with a message
```

Fields are passed as id handles from string literals, which keeps the ABI small. The host caches
id lookups.

### 4.5 Invocation lifecycle (host)

**Action** (`a_<id>`):
1. The host opens a transaction over the documents of the resources passed as arguments.
2. It calls the export. All `res_*`, `coll_*`, `set_*` and `map_*` calls go into that transaction.
3. If the export returns, the host commits **one change per document**. Each change carries the
   action id, arguments, invoker, the authorship chain and the module hash (spec §3.2).
4. If the export traps, the host rolls back.

The host also enforces that an action only mutates resources of its own module's types. That
enforcement is defense in depth: the compiler already guarantees it.

**Flow** (`f_<id>`):
1. The host keeps a **journal** per flow run: the ordered results of `invoke` and `model_call`.
2. On `invoke`, the host asks `Authority` (§4.7) for a decision:
   - **Allow:** run the action, append its result to the journal, return it.
   - **Ask:** record a pending suggestion and **trap with a suspension marker**. The run is now
     suspended.
   - **Deny:** append the denial to the journal and return an error value.
3. When a suggestion is resolved, the host **re-runs the flow from the start**. While replaying,
   `invoke` and `model_call` return the journaled results instead of executing. This is correct
   because flows are deterministic (spec §3.3): there are no clocks, no randomness, and every
   read goes through handles. Reads during replay see current state.
4. **v1 deviation:** a flow does not record a snapshot of what it read. Reads during replay
   therefore return current state, so a replayed flow can take a different branch than it did the
   first time. Recording what the flow read is follow-up work.
5. The journal is a `Vec<JournalEntry>` that implements serde. It is persisted by the store
   (`Store::save_journal`), so a suspension survives restarts.

**View** (`v_<id>`):
1. The host calls the export with a **read recorder** active. It records every `(resource, field)`
   read.
2. The result is a UI tree of nodes, each with a key.
3. When any recorded field changes (a local action or incoming sync), the host re-runs the view
   and **diffs the new tree against the old one by key**.
4. The diff is a list of patches (insert, remove, move, set property), which the renderer applies.
5. Keys:
   - a node's key is the ui_call's AST id;
   - inside `for`, it is combined with the item's identity (a collection item id, or the index for
     plain lists).
6. **v1 deviation:** the whole view re-runs, not just the reactive parts. Views are cheap, and the
   diff keeps rendering incremental. Finer-grained reactivity is follow-up work, behind the same
   interface.

### 4.6 Outside capabilities

- An Outside effect is a call to `outside(capability, payload)` from an action. The host doesn't
  run it during the action. Instead, it **queues it in the action's change as an outbox entry**,
  which the platform settles on the device that holds the capability.
- v1 provides one capability for tests and demos: `"post"`. It delivers a payload to a named
  external sink. Tests use an in-memory sink; the platform logs to a sink file.
- Mail and similar capabilities come later. Each is one more host implementation of the same
  import.

### 4.7 Runtime traits

```rust
pub trait Store {
    // documents
    fn open(&mut self, r: &ResourceRef) -> Result<DocHandle, StoreError>;
    fn transaction(&mut self, docs: &[DocHandle]) -> Result<Txn<'_>, StoreError>;
    fn create(&mut self, ty: &TypeId, app: &AppId) -> Result<ResourceRef, StoreError>;
    fn subscribe(&mut self, r: &ResourceRef, f: Box<dyn FnMut(&ChangeSummary)>);
    fn save_journal(&mut self, run: &RunId, j: &Journal) -> Result<(), StoreError>;
    fn load_journal(&mut self, run: &RunId) -> Result<Option<Journal>, StoreError>;
}

pub trait Authority {
    /// `chain` is the authorship chain, outermost first: the viewer, then each code author.
    fn decide(&self, invoker: &Principal, chain: &[ModuleHash], action: &ActionInfo) -> Decision;
}
pub enum Decision { Allow, Ask, Deny }
```

`ActionInfo` carries:
- the action id;
- the app;
- the effect class;
- `own: bool`, true when the author module equals the action's module.

The reference `Authority`, `SimpleAuthority`, implements spec §4.3:
- own actions are allowed;
- foreign Read and Undoable actions are allowed when every author in the chain is trusted, and
  otherwise ask;
- Outside actions ask, unless an explicit grant covers them;
- deny wins over everything.

### 4.8 Reference store (Automerge)

`AmStore` is the reference store used by tests and the CLI.

- One Automerge document per resource.
- The document root holds:
  - `$type`: the type id;
  - `$app`: the app id;
  - every field, keyed by its **field id**.
- Mapping from merge kind to Automerge:

| Merge kind | Automerge representation |
|---|---|
| `register` | scalar value |
| `counter` | Automerge `Counter` |
| `text` | `Text` object, updated by diff and splice |
| `Collection<T>` | a map of `item id -> record object` plus an `$order` list of item ids. Moves rewrite only `$order`. |
| `Set<T>` | a map of `key -> true` |
| `Map<T>` | a map |
| nested record | a map |

- Item references are `doc ref + item id`.
- **Sync for tests:** `AmStore::sync_with(&mut other)` runs Automerge's sync protocol between two
  stores over every shared document. The multi-replica tests use it to prove convergence.

## 5. Metadata (`tilolang.meta`)

The metadata is JSON. Tools, the runtime, the platform and agents all read it. Example:

```json
{
  "app": "so.tilo.counter", "title": "Counter", "module_hash": "…",
  "types": [{"id": "k3f9…", "name": "Counter", "doc": "…",
             "fields": [{"id": "x1…", "name": "value", "type": "Int", "merge": "counter"}]}],
  "actions": [{"id": "…", "name": "increment", "params": [{"name": "c", "type": "Counter"}],
               "result": null, "class": "undoable", "proposal": null,
               "reaches": [{"action": "…", "class": "undoable", "own": true}]}],
  "views": [{"id": "…", "name": "counter", "params": […], "anchors": [{"id": "…", "name": "plus"}],
             "reaches": […]}],
  "flows": […], "fns": […]
}
```

The platform derives three things from the metadata:
- the agent's discovery catalogue;
- the model projections (§6.2);
- adoption prompts.

## 6. Agent support (runtime level)

### 6.1 Blocks

A block is a source expression evaluated in an ad-hoc module.

- The ad-hoc module is `app agent.block.<run-id>`. It implicitly imports every app the task can
  see.
- Its author principal is the agent's task.
- `tilolang::compile_block(src, visible_apps)` returns:
  - a module with one export, `b_main`;
  - the block's value type, which is one of `View`, `Action`, or data.
- **Rendering:**
  - a `View` value renders directly;
  - an `Action` value invokes or suggests through `Authority`, and renders as an invocation;
  - a data value renders through the default view for its type.

### 6.2 Model projection

`tilolang-runtime::project(value, meta) -> serde_json::Value`:
- records become `{type, id, label, fields…}`, truncated;
- lists are capped to N items, plus a count;
- references become `{type, id, label}`.

Flows run by agents return projections to the model.

### 6.3 Discovery

`tilolang-runtime::catalogue(apps) -> Vec<Signature>` is built from the metadata and shown to
agents as source-like signatures, doc comments included:

```
/// Add one to the counter.
action so.tilo.counter.increment(c: Counter)
```

## 7. Platform integration (`tilolang-tilo`)

This section is filled in from the platform research. It covers records and room sync, the action
door, grants, rendering, presence and provenance.

## 8. Tests that pin the design

These tests live in the crates and are required for v1.

**Compiler:**
- Parse and print round-trips.
- One test per error code.
- Merge-rule tests: `+=` on a register is rejected; `=` on a counter is rejected.
- Effect tests: a view can't mutate; an action can't call a foreign action.
- Capability-set tests.
- The ids lock: rename keeps the id; a retired id isn't reused.

**Runtime:**
- The counter end to end: compile, instantiate, `increment` twice, read 2.
- **Concurrency:** two `AmStore` replicas each increment once, sync, and both read 2. The
  equivalent text-splice case and the collection insert/move case converge too.
- **Authority:**
  - a button in a module authored by a stranger that invokes a foreign action yields Ask;
  - the same action from the app's own view yields Allow;
  - revoking an author's trust changes the decision at the next invocation.
- **Flows:** an Ask suspends the run; resolving it replays the run, and the journaled results are
  not executed again.
- **Views:** changing one field re-renders, and the diff touches only the affected nodes.
