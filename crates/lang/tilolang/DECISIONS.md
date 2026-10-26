# tilolang compiler: decisions

Choices made where implementation.md and spec.md are silent or ambiguous, and where the compiler
departs from them. The runtime agent should mirror the ABI additions.

## ABI additions and interpretations (host side)

- **`space_i(field: h) -> i64`, `space_f(field: h) -> f64`** (layout.md section 4). `space.width_class`
  and `space.orientation` read through `space_i` as the variant index (`width_class`: compact 0,
  medium 1, expanded 2, wide 3; `orientation`: portrait 0, landscape 1, ultrawide 2).
  `space.compact` and `space.wide` are compiled as `space_i("width_class") == 0` / `== 3`.
  `space.width` and `space.height` (rem) read through `space_f`. The field name is a string
  literal handle, like record fields. The host records each read for the read recorder.
- **Reading a container field** (`res_get_h` on a `Collection`, `Set` or `List` field) returns a
  list handle that `list_len` and `list_get_h` accept: the item references of a Collection, the
  elements of a Set. A `Map` field is not read as a whole (E080); it is read with `.get(key)` (`map_get_h`, an `Option` of the value, scalars boxed like `some_i`) and `.keys()` (`map_keys`, a `List<Text>`).
- **Scalars through handles.** Booleans use the `_i` imports (0/1 as i64). `invoke` returns a handle;
  a scalar action result is unboxed with `unwrap_i`/`unwrap_f`, so the host must box scalar results
  like `some_i`/`some_f` (`Option<Int>`). A unit result is any handle (dropped).
- **Boxed scalars as payloads.** `outside`, `model_call`, `set_add` and `map_put` receive scalars
  boxed with `some_i`/`some_f`.
- **Tuple ops.** `tuple_new` takes an `i32` count and `tuple_set_*` an `i32` index. `tuple_get_*` is not
  emitted: the grammar has no tuple access.
- **Imports are emitted on use.** The import section lists only the imports a module calls, in the
  order of the ABI table. A host must provide a superset. `memory` is always exported and holds the
  string data segments, starting at offset 0.
- **Foreign views.** The ABI has no cross-module call, so applying another package's view (in a
  block or a view) emits `ui_node("view:<app>:<view id>", key)` with each argument as a property named
  `arg.<param>` (`ui_prop_*`). The host resolves such nodes. Calling a foreign `fn` is E081.
- **View calls** of own views are wrapped in a `fragment` node keyed by the call site and the loop
  item, because the callee's root key is the same at every call. Per-node layout arguments on a view
  call (`fill`, `min:`, `at:`, ...) go on that wrapper node.
- **Keys.** A node key is `<view id>.<ui_call node id>`, or the anchor's id when it has an anchor
  (`as name`).
  Inside `for` the key is `str_concat(key, suffix)` where the suffix accumulates `"/"+ref_id(item)`
  for record items and `"/"+int_to_str(index)` for other values.
- **Layout arguments** (layout.md): unit literals `11rem`, `4dp`, `1fr` are `Length` values; a prop
  with a length is `ui_prop_f(n, "<name>.<unit>", value)` (`"min.rem"`). Options that take one of
  the primitives' enums (`gap: .section`, `at: .bottom_trailing`, `collapse: .drawer`, button
  `style: .primary`) are `ui_prop_h` props holding the variant's name, so hosts read the same
  strings as before (see "Syntax revision"). Bare flags (`truncate`, `page(full)`) are `ui_prop_i(n, "<flag>", 1)`; `fill` is
  `ui_prop_f(n, "fill", 1.0)` and `fill(2)` carries the weight. `aspect: 16/9` is constant-folded to a
  Float. `primary`/`secondary` are primitives (slots of `split`); the compiler does not check they sit
  in a `split`. `column` stays as an older name of `stack`. `;` separates statements on one line.
- **`embed(source, author: p, invocation: i)`** (implementation.md 7.8) is a UI primitive:
  `source` is `Text`, `author` a `Principal` and `invocation` an
  `Option<host.Invocation>`. `source` and `author` are required (E022; E020 for a wrong
  type). It emits `ui_node("embed", key)` with props `source`, `author` and `invocation`.
  The host fills in the node's children (see the runtime's DECISIONS).
- **`Principal`** (spec deviation, replaces "model the principal as a Text id"): a type whose
  values come only from the host. It can be a field (register), a `fn` or view parameter, and
  compared with `==`; `.name()` and string interpolation give its id as `Text`. There is no
  constructor and no conversion from `Text`. Actions and flows may not take one as a
  parameter (E048): they are what callers invoke, and a caller could pass anyone.
- **`invoker`** (E049 outside actions) reads the principal the runtime authenticated for the
  running invocation: for a person's press, that person; for a call through the door, the
  caller; for an approved suggestion, the principal who first invoked it.
- **`run_block(part, source)`** (actions only, E047 elsewhere): queues a host step that runs
  after the action commits. `part` must be one of the module's own records with a field
  `invocation: Option<host.Invocation>` (E020, E045). The host evaluates `source` as a block
  written by the invoker and, when it applies an action, records an `Invocation` and stores
  it in that field.
- **The `host` package** (`tilo.host`, `tilolang::host_meta()`) is imported implicitly by
  every module and block as `host`. It declares `Invocation` with the register fields
  `action`, `name`, `app`, `args` (JSON text), `author` (Principal), `state`
  (`done|suggested|declined|failed|denied`), `result` and `run`. Only the host writes them.
- **New imports:** `invoker() -> h`, `principal_eq(a, b) -> i32`, `principal_name(p) -> h`,
  `queue_block(part, field, source)`.
- **Nested resource references.** A reference literal may carry the steps of a nested
  resource: `@todo.Todo:<doc>/<field>:<item>`, exactly `ResourceRef::id_string`. Without it a
  block could not name a note or a draft, which live inside a collection. Ids still use only
  letters, digits, `_` and `-` between the separators.
- **`list(items) { x -> ... }`** (spec example) is parsed as `column { for x in items { ... } }`.

## Language choices

- Declaration ids come from `app.ids` by qualified name: `Album`, `Album.title`, `play`,
  `view#anchor`. Types start uppercase, everything else lowercase, so names never collide. UI call
  keys are not in the lock; they derive from the parse-order node id.
- Ids are 8 base32 characters from an injectable `Entropy`. A name that disappears is retired with
  its id and a name that comes back gets a fresh id. `rename` moves everything under the old name.
- A record-typed field is a reference (register). Nested structural records are not distinguished.
- Fields without defaults take the host's zero value; defaults are recorded in metadata, not emitted.
- `fn`s may read resources through their parameters.
- A flow can only invoke actions. In a view or a block, applying a flow (or an action) yields an
  `Action` value (`act_ref`), so a flow can be exposed as an action. Flows cannot call flows.
- `sort_by(x -> key)` compiles to a loop that collects the keys into a parallel list and one call of
  `list_sort_by_key(items, keys) -> list`: ascending and stable, keys Int, Float, Text or Bool
  (E020 otherwise). There is no descending order and no `list_set`.
- `try act(args)` (flows only, E044 elsewhere) invokes an action and gives a `Result<T, Text>`
  (`Result<(), Text>` for an action without a result): `.ok(result)`, or `.err(reason)` when the
  invocation was denied, declined or trapped. It compiles to one import, `try_invoke(act) -> h`,
  which invokes like `invoke` and builds the `Result` value in the host. Without `try`, a failed
  invocation yields an error value that traps when its scalar is unwrapped.
- Sets and maps of Int and Float take their elements and values boxed with `some_i`/`some_f`
  through the existing `set_add`, `set_remove` and `map_put`. Set elements of Int or Float are
  stored under their text form, so a set iterates in text order.
- A record-typed field must be `Option<T>` unless every value is always set: reading an unset
  reference traps (there is no value to return). A removed target stays a readable reference
  (it compares equal); reading its fields fails.
- Pinned references (`@kind:id@version`) parse but compile to E070: `ref_lit` has no version.
- Integer division and remainder trap on zero, as in wasm.
- `proposal view` may also be written `proposal view(a, b)`, naming action parameters.
- Blocks: an expression is checked as a value in view-like context (actions and flows become
  `Action` values); a UI primitive or foreign view call, or a view body, is a `View`. The block module
  is `app agent.block.<run id>` (`main` for `compile_block`) and exports `memory` and `b_main`.
- `module_hash` is the SHA-256 of the module without the `tilolang.meta` section.

## Packages of several files

- **A package is a directory.** Every `.tl` file directly in it is part of the one module
  (`PackageSource`, `compile_package_source`, `check_package`, `parse_package`); the single-text
  API (`compile_package`, `check`, `parse`) is a package of one file whose diagnostics name no
  file. Exactly one file starts with the `app` header: none is E010, a second one (or a header
  that is not first in its file) is E017. Declarations may use each other across files in any
  order.
- **Files are read in path order.** Node ids, and with them UI call keys, follow that order, so
  the order files are given in does not change the module. A package whose files concatenate to
  one file compiles to the same module as that file.
- **Spans.** Each file is parsed at its own offset in one span space (the next file starts one
  past the previous file's end); diagnostics are then moved back into their file
  (`Diagnostic::file`, the span an offset in that file) and ordered by file and position.
- **One ids lock per package** (`app.ids`), as before.
- **Imports on disk.** `use` names an app id; the command line finds it among the package's
  sibling directories, or in `--path` entries (a package directory or a directory of packages),
  and compiles imports first (`source::dependency_order`, `source::compile_dirs`). A cycle is
  refused.
- **`fmt` keeps what people wrote.** The canonical form drops plain `//` comments (only `///`
  doc comments are part of the tree), so `fmt` leaves a file that has any unchanged and names
  it, rather than deleting them.
- **The command line is a library module** (`cli::main`), so the host app's binary can run the
  same commands and add `run`, which needs the platform. The names: `tilolang` is the full build
  tool (the host binary, with `run`); the compiler crate's own binary is `tilolangc`, also
  built as `tlc`, from the same `main.rs`, so neither clashes with `target/<profile>/tilolang`.

## Deviations to report

- E072 (a block with no value) is defined but cannot be reached from source today.
- `layout-check` (layout.md section 6) is not part of this crate.

## Syntax revision

The language as implementation.md section 2 wrote it, revised: sum types, one `if`, blocks as
expressions, dot literals for every built-in option, `as` anchors, no `do`, view state, and the
package-level `root` and `default view` of workspace-model.md. The README has the reference.

- **Enums.** `enum Name { a, b(x: T, y: U), c }`. Enum and type names share one namespace;
  variants are lowercase. Ids: the enum `Name`, each variant `Name.v`, each payload field
  `Name.v.f`, all in `app.ids`. A payload field holds a value type (not `Action` or `View`).
  Payload fields are positional or named at construction (`.b(1, "z")`, `.b(x: 1, y: "z")`).
- **Values.** `.v` / `.v(args)` resolve against the type the context expects (a parameter, a
  field, an annotation, the other side of `==`, a branch's agreed type, a UI option); with none,
  E037 asks for `Name.v`. A qualified `pkg.Name.v` names another package's enum.
- **Host representation.** A declared enum value, and a `Result`, is a host handle holding the
  type id, the variant id and the payload by field id. New imports: `enum_new(ty, variant) -> h`,
  `enum_set_i/f/h(e, field, x)`, `enum_is(e, variant) -> i32`, `enum_get_i/f/h(e, field)`.
  Storing by ids keeps stored values readable when variants or fields are reordered. In a
  register an enum value is the JSON `{"enum": ty, "variant": v, "fields": {id: value}}`.
- **`Option` and `Result` are built-in enums.** `Option<T>` keeps the host's option values
  (`some_*`, `none`, `is_some`, `unwrap_*` are now only what `.some`/`.none` and `match` compile
  to). `Result<T, E>` uses the enum imports with the fixed ids `tilolang::RESULT_TYPE`
  (`result`), variants `ok`/`err`, fields `value`/`error`. The surface language has no `none`
  literal, no `is_some`/`is_none`/`unwrap`/`unwrap_or`; a `T` is still accepted where an
  `Option<T>` is expected, because wrapping cannot fail.
- **Equality.** `==` on enums, options, results, tuples and lists compiles to
  `value_eq(a, b) -> i32`: structural, payloads included, references by target only.
- **`match`** is an expression, a statement and a UI statement. It must name every variant
  (E035 lists the missing ones), each once, with binders for all payload fields or none (`_`
  skips one; E036). It lowers to an `if` chain over the variant tests; the last arm is untested.
  A wildcard arm does not exist: the variants are always spelled out.
- **Fields of enum type** are registers holding the whole value. Without a default they start
  as the first variant, which must then carry no payload (E019 otherwise); a default is a dot
  literal with literal payload values. The default's JSON is the register encoding above, so the
  platform's record initialises it like any register.
- **Metadata** lists `enums` (variants and payload fields with ids). A type that names an enum
  is written `enum Name` (`enum so.tilo.notes.NoteOrder` when foreign) in metadata only, so a
  reader of the metadata tells it from a record without the declaring package at hand; source
  and diagnostics write `Name`. `Ty::meta_text` writes it, `Ty::parse_meta` reads it.
- **Built-in enums of the platform** (`Ty::Choice`): the options of UI primitives (`Gap`, `Align`,
  `Distribute`, `Collapse`, `Side`, `Position`, `Axis`, `ButtonStyle`, `TextStyle`) and the
  `space` enums (`WidthClass`, `Orientation`). Their values are the variant index (an `i64`),
  so they compare with `==` and can be chosen at run time (`style: if on { .primary } else {
  .plain }`); a UI option becomes `ui_prop_h(node, name, <variant name>)`, a constant for a
  literal and an `if` chain otherwise. They have no names in source. A bare word where an option
  is expected is E065 with the dot form; `space.width_class == wide` is now `== .wide`.
- **One `if`.** `if c { a } else { b }` is an expression (`Ek::If` over two block values); `if ...
  then ... else` is gone. As an expression statement (and in a view body) the branches run as
  statements and `else` is optional. An `if` used as a value needs `else` (E038). `else if`
  chains in views too.
- **Blocks are expressions.** `{ stmts; expr }` has the last expression's value; a block that
  returns on every path has any type (it lowers to `unreachable` after the return). A body whose
  last statement is an expression returns it, so `fn f() -> Int { 1 }` needs no `return`.
- **Lines that start with `.`.** A line starting `.name` continues the previous line as a method
  call, as before, except after a line that ended with `}` and except for a line that is a match
  arm (`.name(..) =>`), so arms need no commas. The printer writes a comma after every arm that
  is not a block.
- **Keywords.** `enum` and `match` are new keywords; `then`, `none` and `do` are no longer.
  `window`, `lens` (at the start of a view statement, before `name:`), `root` and `default`
  (before `view`, at the start of an item) are contextual, so they stay usable as names. A
  keyword may name a call argument (`open(r, view: v)`).
- **Flows** invoke an action by calling it. `do act(args)` is a syntax error that says so.
- **Anchors** follow the call: `button("+1", on: inc(c)) as plus`. `#plus` is a syntax error
  that says so. An anchored node keeps the anchor id as its key, so an anchor belongs on a node
  that appears once in its view's tree, not on a row of a list.
- **View state.** `window name: T = default` and `lens name: T = default` are declared at the top
  of a view body (E066 elsewhere, or outside a view); their ids are `view.name` in `app.ids`, and
  views list them in metadata (`state`: id, name, layer, type). Without a default, Int, Float,
  Bool, Text, options, lists and payload-less first variants start empty or first; other types
  need one. The view reads a slot with `state_get(layer, id, default) -> h` (layer 0 window, 1
  lens; scalars boxed like `some_i`), which returns the slot's value or the default; the host
  records the read. `bind: name` on a control is `ui_bind_state(node, layer, id)` (Int, Float,
  Bool or Text state, E062 otherwise). `set(name, value)` in a view is
  `act_set(layer, id, value) -> h`, an action reference the host applies to the slot; it carries
  no capability and is never a change to a resource. State is the declaring view's own: other
  views and nested view calls do not see it (pass values or `Action`s as parameters).
- **`open(r)`** and `open(r, view: v)` in a view are `act_open(r, view id or "") -> h`, an action
  reference the host turns into a request to go to the window of `r` (E067: `r` is a record, `v`
  a view that takes exactly an `r`). Flows and blocks cannot open windows yet.
- **`root T` and `default view v for T`.** `T` is one of the package's record types (E018); a
  root needs a default view, which takes the resource and nothing else (E018). A type has at
  most one default view and is a root at most once (E011). Metadata: `roots` (type ids, in
  order) and `default_views` (type id and view id); `PackageMeta::default_view(ty)` reads them.
  `view main(root: T)` is no longer a convention: hosts open `roots[0]` through its default
  view.
- **New error codes:** E018 package declarations, E035 non-exhaustive match, E036 unknown or
  repeated variant, wrong binders, E037 a variant with no expected type, E038 an `if` value
  without `else`, E065 a bare word where a dot literal is expected, E066 view state, E067
  `open`.
- **Removed from the compiler's import table:** `is_error` (the runtime still provides it).

## Outcomes and deltas

Convergence design section 1.1.

- **Parameter defaults.** An action parameter may have a default, `done: Bool = not item.done`.
  It reads only the parameters before it (E028 otherwise), in the discipline of a `fn`: no
  actions, no `invoker`, no `now`. Only an action's parameters take one (E028 for a fn, flow,
  view or variant field). Each default is a function of its own, exported as
  `d_<action id>_<param>` and taking the parameters before it; metadata lists it on the
  parameter (`default`: the export and the default as written, for descriptions).
- **When it is computed.** When the action is invoked without the argument, on the invoker's
  state, and the value is recorded as the argument: the runtime completes an invocation's
  arguments before it decides, journals or runs it (`Runtime::complete_args`), so the change
  note, a suggestion and the platform's recorded invocation all carry it. An application from a
  view, a flow or a block may leave out only its last parameters (E028 for a gap), which the
  runtime computes when the application is invoked: a button computes the default when it is
  pressed, not when the view renders. An action inlined into another action computes it at the
  call. An explicit argument always wins.
- **Presentation.** A `label:`, `icon:` or `on:` may read a defaulted parameter whose default
  follows from the subject: the presentation view binds it from the subject first. The static
  label (and icon) of an `if` is the branch a subject at rest takes, every Bool field at its
  default and every parameter at its default (`if done` with `done: Bool = not item.done` is
  the `then` branch); when that cannot be told, the `else` branch, as before.
- **E039.** A write of a register whose value is `not` of that same register, or that register
  plus or minus an amount, is refused: it is computed again wherever the change is applied.
  `let`s are followed (a variable reassigned anywhere is no longer followed). The message names
  both fixes: an outcome, a parameter with a default; a delta, a counter. Other computations from
  the same register (`t.tag = tidy(t.tag)`, `t.n = t.n * 2`) are legitimate replays. E030's
  advice no longer suggests writing the read and the write out.
