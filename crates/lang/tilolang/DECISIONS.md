# tilolang compiler: decisions

Choices made where the design (`docs/designs/app-language`) is silent or ambiguous. The runtime
mirrors the ABI additions; `tilolang-runtime/tests/abi_parity.rs` checks that the two agree.

## ABI additions and interpretations (host side)

- **`space_i(field: h) -> i64`, `space_f(field: h) -> f64`** (layout.md, "Responding to space"). `space.width_class`
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
  `arg.<param>` (`ui_prop_*`). The host resolves such nodes.
- **Foreign `fn` calls.** `todo.finished(list, t)` emits `call_fn("<fn id>", args)` with the
  arguments as a list handle; the host finds the package that declares the `fn`, runs its `n_<id>`
  export as that package's own code, and boxes a scalar result like `some_i` so the caller unboxes
  it like an action's. Only `fn`s are called this way: they are pure by construction (no action,
  E043; no field changed, E040), so they are not decided — a call is as free as reading that
  package's resources — and what it reads counts as the calling view's reads and goes into the
  calling flow's journal. A foreign `action` still goes through `invoke` and its approval.
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
  in a `split`. `;` separates statements on one line.
- **`embed(source, author: p, invocation: i)`** is a UI primitive:
  `source` is `Text`, `author` a `Principal` and `invocation` an
  `Option<host.Invocation>`. `source` and `author` are required (E022; E020 for a wrong
  type). It emits `ui_node("embed", key)` with props `source`, `author` and `invocation`.
  The host fills in the node's children (see the runtime's DECISIONS).
- **`Principal`** is a type whose values come only from the host. It can be a field
  (register), a `fn` or view parameter, and compared with `==`; `.name()` and string
  interpolation give its id as `Text`. There is no constructor and no conversion from `Text`.
  An action may take one as a parameter: a person it acts on (`share(n, with: Principal)`,
  `assign(t, to: Principal)`), never who acts, which is always `invoker`; its form asks for one
  of the workspace's members. Flows may not (E048): they run on their own, and a caller could
  pass anyone.
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
- **A program names any app's resource.** In a program, `@<app id>.<Type>:<id>` of an app it
  neither holds nor reads is still a `host.Resource` (`foreign_ref`): a value it passes to an
  action, as the reference a listing or an answer showed (`n2`) is written. Reading it fails as
  a read of any app the program may not read does.
- **`list(items) { x -> ... }`** is a `list` node whose children are one `item` node per resource,
  the item block's nodes under it (or the type's `row view`). The host builds and lays out a row
  when it scrolls to it, so a long list costs only the rows on screen.

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
  `list_sort_by_key(items, keys) -> list`: stable, ascending or `desc` (the same list reversed),
  keys Int, Float, Text or Bool (E020 otherwise), and a Date or Time, which sorts as its ISO text.
  A key that may be absent sorts after every item that has one, whatever the direction, so the
  items whose day is not known stand last and a list grouped by a day sorts by the same field.
  A store that answers the sort from its index keeps that order and the list's own for the items
  a key cannot tell apart. There is no `list_set`.
- `try act(args)` (flows only, E044 elsewhere) invokes an action and gives a `Result<T, Text>`
  (`Result<(), Text>` for an action without a result): `.ok(result)`, or `.err(reason)` when the
  invocation was denied, declined or trapped. It compiles to one import, `try_invoke(act) -> h`,
  which invokes like `invoke` and builds the `Result` value in the host. Without `try`, a failed
  invocation yields an error value that traps when its scalar is unwrapped.
- `suggest act(args)` records the action as a proposal on the resource it is about instead of
  applying it: an agent's block whose whole expression it is, and a statement in an action or
  flow (E044 elsewhere). An action records it once its own change has committed, so the proposal
  is never part of that transaction. It compiles to one import, `suggest(act)`, and
  `proposals(of)` draws what waits on `of`, each card naming who proposed it; accepting one runs
  it as the person accepting, and the change it makes records who proposed it.
- Sets and maps of Int and Float take their elements and values boxed with `some_i`/`some_f`
  through the existing `set_add`, `set_remove` and `map_put`. Set elements of Int or Float are
  stored under their text form, so a set iterates in text order.
- A record-typed field must be `Option<T>` unless every value is always set: reading an unset
  reference traps (there is no value to return). A removed target stays a readable reference
  (it compares equal); reading its fields fails.
- Pinned references (`@kind:id@version`) parse but compile to E070: `ref_lit` has no version.
- Integer division and remainder trap on zero, as in wasm.
- `proposal view` may also be written `proposal view(a, b)`, naming action parameters.
- **A role says what an action is; only `internal` and `scenario` keep it from being listed.**
  `primary`, `destructive`, `migration` and `internal` are the four words. `offered_actions()`
  skips the `scenario` actions and those with a hiding role, so a gesture-only action
  (`internal action place(...)` behind a tree's `reorder:`) never turns up in `actions(r)`, in
  the host's bar or a window's menus, or in an agent's choices, while the app can still name it
  where the gesture wants it.
- **A window's bar shows its primary action and the commands the app marks `frequent` as
  buttons, and every other action behind the bar's one menu.** The host builds them that way
  from the declarations of the type the window shows: a button per primary and per `frequent`
  action, and the remaining actions in the menu, which the shell's own More button holds
  whatever room the bar has. A view never reads the actions back to draw them: it says what
  can be done by declaring actions on the type and by placing the typed affordances of the
  thing itself (a checkbox that completes, an entry that adds, a drag). `actions(r)` is
  therefore an error in a view (E047); it stays readable to the host, which places the
  declarations, and to an agent, which chooses among them. Which commands earn a
  place in the bar is the app's judgement (`frequent: true`), not the window's width, so a
  narrow window gives way with the menu, never with the one main thing to do.
- **A window switches only between the views its type declares.** `views project_list,
  project_board for Project` names them, in the order the bar shows them, its default view
  first: the host draws them as the bar's labelled switch and opens the one the person last
  chose in that window. A view of a type that is not named is the app's own helper (a row body,
  a piece a page places), so it is never a window of its own.
- **An item's actions are on the item.** A row, card or tree node standing for a resource
  carries its type's actions in its context menu (right-click, long press, Menu key) and, for
  a list row, as its swipe; the page of one item carries them behind the "…" beside its title.
  The host places them there from the declarations, once, and the view places no menu of its
  own.
- **A row is drawn by the view that knows what the place around it says.** `row(title, ...)`
  stands in a list's item block, as a row view's body, or as the whole body of a view of one
  item (first parameter a record, body one row after its `let`s). The extra parameters are what
  only the place knows: a list grouped by a day gives the day, a project's list its project, so
  the row omits what the heading or the page already states instead of every row repeating it.
  The type's `row view` stays the item alone, since the system draws it where no page says
  anything (a result, a notice).
- Blocks: an expression is checked as a value in view-like context (actions and flows become
  `Action` values); a UI primitive or foreign view call, or a view body, is a `View`. The block module
  is `app agent.block.<run id>` (`main` for `compile_block`) and exports `memory` and `b_main`.
- `module_hash` is the SHA-256 of the module without the `tilolang.meta` section.
- **A primitive that is a component is only that component's shorter name.** `text`,
  `rich_text`, `section`, `avatar`, `progress`, `toggle`, `checkbox`, `input`, `rich_input` and
  `image` are `ui.Text`, `ui.BlockEditor`, ... : `prims.rs` names the component (`Prim::component`)
  and `checker::component::component_prim` checks and lowers the call as the component's own, so
  its props, slots, events and bindings are the description's and nothing is added. `button` (a
  declared action's button) builds `ui.Button` (`Prim::builds`) because its arguments are the
  statement form's own. `opening(of)` and `facts(of)` read the fields the type declares and build
  from them: the page's start, and one wrapping row of chips, each the field it shows (a day's
  `ui.DateField`, a person's chip). Removed with replacements named in
  E096: `card` (`ui.Card(item: x, title: meta(x).title)`), `column` (`stack`), `date`
  (`ui.DateField`, or `ui.DatePicker` for a calendar), and the arguments a component's own props
  carry (`text(x, style:)`, `button(act, size:)`, `image(src, width:)`). Layout stays the
  layout.md vocabulary over the host's layout engine (`stack`, `row`, `grid`, `split`, `page`,
  `layers`, `scroll`, `spacer`, `divider`, `fragment`), and `list`, `form`, `field`, `choice`,
  `embed` and `show` lower with rules of their own.
- **`text(x)` is `ui.Text`, so `x` is Text.** A number is interpolated (`text("{count}")`) or
  shown by a component that takes one; a figure is `numeric: true`, not a role.
- **`bind:` is the component's binding.** A component that declares one binding takes `bind:`
  whatever the binding is called (`ui.TextInput(bind: t.title)`, `ui.Checkbox(bind: t.done)`,
  `ui.DatePicker(bind: t.due)`, `ui.RichInput(bind: t.notes)`); a component that declares more
  than one is written with the binding's own name. The checker resolves it from the description
  (`only_binding`), so the value's type and its merge (text spliced, rich text rich, a register
  replaced) are the description's, with no per-widget code, and a view's state binds like a
  field.

- **Text and numbers are one method surface, on the value.** They are methods, not
  `text.`/`math.` namespaces, because everything else a value is asked (`items.len()`,
  `items.map(...)`, `p.name()`) already reads that way and a call reads best where its subject
  is. `checker/text_math.rs` lowers all of them, and `checker/methods.rs` holds the one list of
  names per type that both the lowering and the E027 message (and a container field's read)
  consult, so a name offered in a message is one the type takes. `Text.len`, `to_int`, `find`,
  `slice` and `replace` and `Int.to_float`, `Float.to_int` and `to_text` moved out of `call.rs`
  into it with no aliases; `find` gives an `Option<Int>` rather than a sentinel index.
- **A text is counted in graphemes.** `len`, `find` and `slice` count what a reader sees, so a
  cut never lands inside a family emoji, a flag or a letter with an accent; `src/text.rs` is the
  one reading of that, shared by the runtime's `Text` calls, the index's `len` of a text column
  (`pred.rs`) and search's identifier patterns, so a filter and a view count a column alike.
  Case changes are Unicode case mapping, not locale tailoring: the language carries no locale
  data, `upper("straße")` is `STRASSE`, and a locale's own rule belongs to the system's
  formatting, not to a pure string method. `matches` is the identifiers' `*` glob, so a search
  query and an app's own test ask the same question.
- **The arity of `round`, `floor` and `ceil` is the result's type.** No argument gives the whole
  number as an `Int`; the places to keep give a `Float`, so `f.round(2)` keeps the fraction it
  asked for. Places are clamped to 0..=15 (where every place a double has is representable) and
  the rounding is over the scaled value, so a rounded figure is the one written with that many
  places. `Int` arithmetic saturates where the host's own would trap: `abs` of the least `Int`
  stays the greatest, and a sum stops at the end of the range. An empty list's `avg` is 0.0.
  `clamp(n, lo, hi)` is `max(lo)` then `min(hi)`, which for a reversed range gives `hi` rather
  than failing. Nothing is added for padding or aligning a number: how wide a figure is drawn is
  the UI's numeric formatting (`numeric: true`), not a pure text method. Bytes↔Text is not part
  of the language at all: there is no Bytes type.

## Derived keys

- **Resources store facts; the index derives.** A value that follows from other records (a
  branch finished, a parent's done and total children) is not a field anyone writes: it is a
  `derive` key on the type, and the store's index works it out from the collection, keeps it
  where a view reads fields from, and moves it with every change of what it reads. A stored
  copy diverges under concurrent edits — two people checking the last two steps of a branch
  would each write their own answer, and neither would see the branch finished — and needs a
  migration for every record written before it, one that can only guess.
- **What a key may read.** `derive name: Type = expr` declares one, a line in a type body, its
  `Type` a `Bool` or an `Int`: the type's own fields of those types, another key the same type
  derives, and `not`, `and`, `or`, comparisons of two counts (`>`, `>=`, `<`, `<=`, `==`,
  `!=`), `children.count`, `children.count(x)`, `children.all(x)`, `children.any(x)` and
  `parent.all(x)`. `children` are the items naming the item in its parent field, `parent` the
  item it names there, so a type whose keys read either marks that field:
  `parent: Option<Item> (parent) = .none` (E119 says so). Nothing else is derived from other
  records: text, a day, a record or arithmetic on counts cannot be a key.
- **A key reads one way.** It reads the items under an item or the items above it, never both
  (E119): the two are worked out by walks in opposite directions, and one value cannot be
  settled by both. Reading one direction takes two keys — `finished` downward, `folded`
  upward — which is what makes a branch fold when its last steps are checked and unfold when
  one is opened.
- **A key reads itself and the keys declared before it.** The item's own keys are worked out in
  the order the type declares them, so `folded: Bool = finished and parent.all(folded)` reads
  `finished` declared over it. A key that reads one declared below it is E119: it would read a
  value not worked out yet.
- **Nothing writes a key.** It is a field of its type: a view reads it as any other, and search
  filters, sorts and groups by it. An action that assigns it, or a record made with one, is
  E119 — the change is derived again wherever it is applied, so a written value would be a
  second answer to the same question.
- **A key is worked out where its items change.** The index keeps each item's children and the
  chain it stands in. A change derives the item, walks the items above it deepest first, and
  then — from the highest whose value moved — the items under it, stopping where a value no
  longer moves; a key that reads neither is not recomputed for it, so a title keystroke costs
  nothing. A write of the parent field moves the item between two lists and derives the old
  and the new chain; a whole-record write derives every item it visits, the items under each
  before the items above.
- **Readers are told.** A derived value is named to the views that read it and, in a change
  another replica sent, as that item's own change (`Derive::derived_keys`): a branch that folds
  because someone checked its last step elsewhere is redrawn there, though the change named
  only the step.

## What links to a resource

- **`linked(r)` is three answers, in one order.** A resource links to another when an action
  made it from there (`created(r)?.from` of it names that one), when a reference field of it
  names it (the register's value, through options, lists, tuples and enums), or when its rich
  text mentions it (`RichText::references`). Each is a `host.Link`: `of` the resource and
  `how` a `host.LinkHow`, `created_from`, `field` or `mention`, in that order, then by what is
  linked. It is a system query, not a declaration: no type declares it and no app takes part.
- **The index keeps both ways.** Each resource's outgoing set is a pseudo-field (`$links`) of
  the pairs, kept with the fields it is derived from (`store::LINKS`), and each target holds
  the reverse list of what links to it; `Store::linked` reads the target's list, so its cost is
  what links to `r`, never the size of the workspace (a test at 1k and at 10k items reads the
  same). The runtime's in-memory store has no index and answers by walking the log; that path is
  for tests, and documented as such.
- **A change tells its readers.** A resource's outgoing set is derived where its fields are: a
  rich text written, a field set or cleared, a resource removed, and a resource made (the
  `created_from` pair, from the change's source). The entries move with it, and the keys are
  named as the changes that made them (`Derive::links`, `derived_keys`), so a view of
  `linked(r)` runs again when one more resource is made from or names `r`.

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
- **`fmt` keeps what people wrote.** Plain `//` comments are not part of the tree, but `fmt`
  keeps them: each prints above the declaration, field, statement or match arm it was written
  over, at that line's indentation, inside an expression's blocks and its match arms too. One
  written inside a declaration's head has no line of its own to keep, so it prints above the
  next statement. `///` is prose: a blank line between two `///` lines is a paragraph break,
  and the canonical form writes it as a `///` line.
- **The command line is a library module** (`cli::main`), so the host app's binary can run the
  same commands and add `run`, which needs the platform. The names: `tilolang` is the full build
  tool (the host binary, with `run`); the compiler crate's own binary is `tilolangc`, also
  built as `tlc`, from the same `main.rs`, so neither clashes with `target/<profile>/tilolang`.

## Deviations to report

- E072 (a block with no value) is defined but cannot be reached from source today.
- `layout-check` (layout.md, "Checking") is not part of this crate.

## Syntax revision

Sum types, one `if`, blocks as expressions, dot literals for every built-in option, `as`
anchors, no `do`, view state, and the package-level `root` and `default view` of
workspace-model.md. The grammar is [grammar.md](grammar.md); the README shows it by example.

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
- **Built-in enums of the platform** (`Ty::Choice`): the options of the layout primitives (`Gap`,
  `Align`, `Distribute`, `Collapse`, `Side`, `Position`, `Axis`) and the `space` enums
  (`WidthClass`, `Orientation`). Their values are the variant index (an `i64`), so they compare
  with `==` and can be chosen at run time (`at: if low { .bottom } else { .top }`); a UI option
  becomes `ui_prop_h(node, name, <variant name>)`, a constant for a literal and an `if` chain
  otherwise. The cases a `ui.*` component declares (`ui.Button`'s `role`, `ui.Text`'s `role`)
  are the description's, checked by name against it and lowered the same way (`component.rs`,
  `case_value`). They have no names in source. A bare word where an option
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

convergence.md, "Actions".

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
- **A `.none` default is left out wherever it stands.** `= .none` needs no parameter before
  it, so an application that leaves such a parameter out before one it gives passes `.none`
  there (no E028); the runtime computes a trailing one without code (`DefaultMeta::is_none`).
  An app that publishes its actions without a module (a native app's surface, the session's)
  is read with each parameter it may go without as `Option<T> = .none` (`DefaultMeta::none`,
  no export), so `session.place(resource: n2, x: 80, y: 60)` leaves the rest out, `80` stands
  for `.some(80)` as anywhere, and a `.none` reaches the app as no argument. A resource
  parameter of such an action is `host.Resource` (of the kind it names, when it names one):
  code passes the value it has, never the id's text; a view holding only an id writes
  `resource(id, of: host.Resource)`. `Callable::signature` shows the defaults, so E022 says
  what may be left out.
- **E039.** A write of a register whose value is `not` of that same register, or that register
  plus or minus an amount, is refused: it is computed again wherever the change is applied.
  `let`s are followed (a variable reassigned anywhere is no longer followed). The message names
  both fixes: an outcome, a parameter with a default; a delta, a counter. Other computations from
  the same register (`t.tag = tidy(t.tag)`, `t.n = t.n * 2`) are legitimate replays. E030's
  advice no longer suggests writing the read and the write out.

## Time is values

A day, a moment and a length of time are values of their own: `Date`, `Time` and `Duration`.
`Date` and `Time` were already held as ISO text that compares; the length was a shape of filter
literals only, written `7d` in a query and nowhere else.

- **A length of time is a type, not a filter's spelling.** `30m`, `2h`, `3d` and `1w` are
  `Duration` values anywhere an expression stands, and what they mean in a filter is unchanged:
  relative times still resolve to the minute, and ranges (`a..b`) remain a filter's own. The
  alternative — teaching the checker that a bare `7d` is an `Int` of seconds — would have made
  `due + 7d` and `due + 604800` the same expression, and made `{7d}` read `604800`. A `Duration`
  is an `i64` of seconds at the ABI, like an `Int`, and not one in the language: mixing them is
  E024, and `Duration(n)` reads a stored number as one.
- **Stored lengths stay numbers.** A field cannot hold a `Duration` (E023): a length an app
  keeps is the number it measures it in (Mail's `hold: Int = 10`), read where it is used
  (`now + Duration(m.hold)`). The stored format does not change, and no migration is needed.
- **A day moves by whole days; a moment by anything.** A `Date` is a calendar day and has no
  time of day to move, so `today + 2h` is E024 where it is written, and a length only code holds
  fails where it moves the day (`day_shift`). A `Time` moves by the seconds it is given
  (`time_shift`), and `Date - Date`/`Time - Time` are the seconds between them.
- **What a day and a moment are made of reads on them, not from free functions.** `d.weekday()`,
  `d.day()`, `d.month()`, `d.year()`, `d.start_of_week()`, `t.day()` and `t.time_of_day()` are
  methods: the receiver says which reading is asked, and `t.day()` needs no `day_of(t)` beside
  `now` and `today`. `Date.parse(text, near: at)` and `Time.parse(text, near: at)` stand on the
  type, which is not a value: the checker reads them before the receiver as one (a name that is
  no local and no import), so a package can still hold a root named `Date`.
- **One zone source.** Every reading of a day and a moment is the person's: `t.day()` is the day
  the moment falls on in their zone as the filter's `today` reads it, `today` is where `now`
  falls, `Time.parse` resolves a wall time of day through the person's IANA zone
  (`Platform::zone_name`, `Core.zone_name`) so 09:00 tomorrow is right across a change of
  clocks, and `Date.start_of_week` uses the region's first day of the week
  (`Platform::week_start`, `Core.week_start`, Monday where a host names none) — the same locale
  data the toolkit's dates use. Without a named zone, the host's offset stands in, as it does for
  repeats.
- **A moment belongs to an invocation.** `now` and `today` read only in an action (E049
  elsewhere): the moment is recorded with the change, so replaying a change computes the same
  days. `Date.parse`/`Time.parse` take the moment in words are read against as an argument
  (`near:`), which is what keeps them pure.
- **The entry keeps reading words, not the language.** A typed entry's `Text (entry)` is filled
  by the runtime's own scanner (`Core::entry_fill`, `natural`), so the phrase filling an action's
  day and moment does not need the language; `Date.parse`/`Time.parse` are for code that reads
  words itself and names the moment it reads them at. `dated(text, now)` (which answered the
  rest of the text with its day and moment) and `day_of(t)` (which `t.day()` is) are removed
  with no alias.

## Search

search.md, "What a type declares" and "Types, references and paths".

- **`search` blocks.** `search` is contextual: a type body's line `search {` starts the block,
  one per type (a second is a syntax error). Lines: `filter field ["Label"]`,
  `filter "Label" = expr`, `sort field newest|oldest|a_z|z_a`, `hide field`. A filter's key is
  the field's name, a named filter's its label in snake case (`Has attachment` is
  `has_attachment`); `is`, `sort`, `in` and `text` are search's own words and no key. A
  field's label is its name in sentence case; its doc comment's first line is its description.
  Metadata carries the effective search of every type (`TypeMeta.search`), its defaults
  included, so the platform and other packages read one shape. New codes: E100 (no such
  field), E101 (a type no filter compares), E102 (a named filter the index cannot keep), E103
  (an order the field does not have), E104 (repeated, hidden and filtered, a reserved key),
  each naming the type with its doc comment's first line.
- **A filter's options are declared** with the annotation enum cases and actions take:
  `filter done (icon: .circle_check)`, `filter "Today" = due <= today (icon: .star, count:
  true)`. After a named filter's predicate the parenthesis would read as a call; one that names
  only options is the filter's options. `icon:` is what its control shows beside its label, and
  `count:` whether the place it makes shows how many items it holds. A filter that declares
  neither has no icon and shows no count.
- **Named filters are predicates of the existing vocabulary.** They read fields with `not`,
  `and`, `or`, comparisons, `.len()` and `.is_empty()`. search.md's
  `attachments.count > 0` is written `not attachments.is_empty()` or
  `attachments.len() > 0`: `count` is the counting method that takes a predicate.
- **`std.Identifier` is a built-in value type**, held as its URI text like `Date` and `Time`
  are ISO text, with `.scheme` and `.value` (`ident_part`). search.md has services' types
  (`MailAddress`, `DiscordUser`) implement `std.Identifier`; declaring that a type implements an
  interface needs syntax search.md does not give, so it is not built: fields hold
  `std.Identifier` itself.
- **Query values are search's text.** `query(T, ...)` lowers each argument to the text a typed
  search writes for it (`done` and `false`, `due` and `today..today+7d`, a pattern normalized
  as it is held) and the type's default order as a `sort` term when none is given; one import,
  `query(type, scope, terms, text)`, resolves the terms and the typed search `text` against
  the type's filters at run time (`tilolang_runtime::query`). So `find("from:*@x.com is:unread
  received:-7d")` and `query(Message, from: "*@x.com", unread: true, received: -7d)` are one
  query. `today` and `now` are bare words in a query's values, as `now` is in an action;
  search.md's `today()` is accepted too. Durations are values of their own everywhere (see
  "Time is values"); in a filter they are the same lengths (`7d`, `-30d`; units s, m, h, d, w),
  and ranges (`a..b`, `a..`, `..b`) are values of filters only (E105 elsewhere).
- **Relative times resolve to the minute.** `-7d` and `today` read the host's clock rounded
  to the minute, so a view asks one query a minute; a list does not move by itself as time
  passes.
- **A query in a view looks inside a resource (`in:`).** A view's dependencies are kept per
  collection; one across the workspace would follow no change. Actions and flows may query
  the workspace (`Store::select`, answered by the index's columns across the workspace).
- **`std.list` owns its query.** A template may declare `window`/`lens` state at the top of its
  body; it is the expanding view's, kept under `<view id>:<template>.<name>` and listed in that
  view's metadata. `std.list`'s `window query: Text` holds the place chosen, as people type
  it in search (`is:done`); its places are `filters(T)`'s toggles after all of the items
  (`all`, named `everything:`, by default from the type's plural noun, `host.Meta.plural`: "All notes"); `place:` is
  the place a window starts on, the state's default (Letters starts on `is:drafts`). A place shows a
  count only where its filter declares `count: true` (`items.matching(p.query).len()`), never the
  all place, a project's or one that declares none. The `filter:` and `count:` parameters are
  gone.
- **A list groups by the key its caller gives.** `std.rows(group:)` takes `(T) -> Option<K>` and
  heads each run of items naming the same key with the key itself, as the platform presents a
  value of its kind — a day as Today, Tomorrow, a weekday; a word as it is written. Items that
  name no key stand under `ungrouped:`, in the order the caller gave them; `tail:` names the
  items done with, under `tail_title:`. A place that groups or folds by what it says rather than
  by its items answers per item (`group: x -> if by_day { x.due } else { .none }`,
  `tail: x -> folds and x.folded`), so nothing reads the whole collection on every change:
  `headings:`, `folding:` and `closed:` are gone, and a caller whose fold must stand last
  orders its items itself (`sort_by(.folded)`).
- **A function argument binds the type parameters its result names.** A template's type parameter
  is not a variable, so `rows<T, K>(group: Option<(T) -> Option<K>>)` learns `K` where a call
  passes the lambda, from what the lambda makes (`group: .due` a day, `group: x -> .some(x.t)` a
  word); a function that makes something else is told so. A function passed on by the name its
  caller gave it binds them from that function's result as well.
- **Reflection reads the log, and may not have it.** `created(r)` and `updated(r)` are
  `Option<host.Version>` (`who`, `at`, `what`): a resource whose changes the device has not
  read yet has no versions to show, and code says what it shows then. A version is a change
  that wrote the resource itself (a field, or its insertion), not one of its items. The runtime
  records each change's resources and time with it (`ChangeNote::touched`, `::at`; on Tilo,
  `DynAction::changed` and `Authorship::at`, read back from the record's changes).
- **What a resource was made from is a change's fact.** An invocation carries the resource it
  is made from when it has one (`Runtime::invoke_action_from`, `Journal.from`; on Tilo a plan
  names it, `PlanRequest.from`, and the change records it as `Authorship.from`), and the first
  version of a resource says it: `created(r)?.from`, an `Option<host.Resource>`. It is not a
  field anyone declares or writes — every type has it, and an action writes none. A store
  without a record of changes keeps it nowhere and answers nothing.
- **A scenario's `as` block is a change of its own.** `as who at time { ... }` commits what came
  before, then the block, as `who` at `time`; history names the scenario as the action, since
  seed data is the scenario's doing. Rolling back on a failure undoes the last block only.
- **`made from r { ... }` says what a scenario's seed data was made from.** The block runs as a
  change made from `r`, the way an invocation on `r` is, so what it creates records it
  (`created(r)?.from`) and what links to `r` names them, as a person's make from `r` does. It is
  a change of its own for the same reason `as` is: what the block makes is made from `r`, while
  what came before is not. Only a scenario writes it, since seed data is what shows a
  provenance feature in `layout-check`.
- **A scenario seeds the view state its window stands on.** `window place = Task.done` (and
  `lens picked = ...`) in a scenario names a state the view declares, with an optional view
  (`window home.place = ...`) where two declare one name, and the value is checked as the
  declaration's type is. It sets the declaration, not an instance: the host reads a declared
  state as the person's slot, then the scenario's seed, then the declaration's default, so a
  window that opens later stands on it too, and the seed is a generation bump so an open window
  draws again. A person's own choice is their slot and wins over the seed, and the seed is
  never written as their change — a capture can show a place no one picked without recording
  that anyone did. Only a scenario writes it: production code cannot stage a person's place.
- **`a.then(b)` joins actions.** It is a method of action values, so it means the same wherever
  one is written; `b` runs once `a` is done. A composer sends its lens draft and clears it with
  `send(c, draft).then(set(draft, ""))`; a button presents the first action.
- **An action a view applies reads its view state at the press.** An action's arguments are
  evaluated when it is invoked, on what the invoker sees, the rule parameter defaults follow.
  View state given as an argument (`send(c, draft)`, `set(draft, other)`) is the slot itself
  (`state_at`), read when the event fires, in the view instance that took the typing: the
  window's last draw may be a keystroke behind. An argument that computes from view state
  (`set(open, not open)`, `send(c, "{draft}!")`, `set(n, n + 1)`) is a function of its own,
  exported as `e_<id>` and listed in the metadata's `at_event` like a `fn`: it takes the view
  state it reads (`state_at`) and the values from around it, as drawn, and is checked in the
  discipline of a `fn`. The view passes it with `arg_at(id, args, boxed)`; the runtime runs it
  when the event fires, after reading the state, and passes the value. So a toggle pressed
  twice before the view draws again toggles twice. Every action reference applied from a view
  does this, buttons, events of components and `then` alike; arguments that read no view state
  are the values the view drew with.
- **`if let`, `?.`, `??` and key paths add no context rules.** A match stays exhaustive in a
  view as anywhere; absent renders nothing through `if let` and `Option<View>` parameters,
  which are type rules. `a?.b.c` is not a chain: write `a?.b?.c`.
- **Unions are of resource types.** `A | B` is a reference to a resource of either type, as
  a field of one resource type is; a `Collection<A | B>` owns items of each type, each read as
  the type it is stored as (`$type`). An enum's cases are plain names (E110); a case that holds
  data is a type of its own. `Option` and `Result` stay built in.
- **A name for a container is a resource.** `type Notes = Collection<Note>` is a resource
  type with one field, `items`, which code never names: the resource is used as the container
  where one goes. Any other `type X = T` is another name for `T`. A root is named
  (`root notes: Notes`) and reached as `notes`, or `alias.notes` from another package.
- **A label that interpolates is read where it can be.** Over the subject alone it follows the
  subject in the action's presentation view, as an `if` label does. Over other arguments
  (`"Move to {col.title}"`) it is read where the action is applied with them,
  `button(move_to(b, card, col))`; elsewhere the action's name stands in.
- **Views are components in slots.** A slot that holds one toolkit component takes a view of
  the package whose body is that one component (`view card_tile(...) { ui.BoardCard(...) }`).
- **The ids lock is checked, not written, by `check`.** `check` fails while the lock is out of
  date and lists each change with why it is there; `check --write-ids` and `build` write it,
  drawing new ids at random so packages never share one. `build` writes its module to `--out`
  (default `target/tilolang`), never into the package.
- **Outside origins are kinds the platform declares.** `type T (origin: imap.message)` names a
  kind of `crate::origin::KINDS`; each kind lists its fields with their types and whether people
  change them. A type declares the kind's fields it shows, with the kind's types (E112), and may
  add its own; a collection field of a kind holds a type of that origin's kind it names
  (`Collection<imap.thread>`). Writing a field of the kind people do not change is E113 outside a
  scenario, whose seed data stands in for the service. The metadata carries `origin` per type;
  fields are matched by name. A named filter may test a set holding a literal
  (`boxes.contains("inbox")`), the index's run of that value.
- **`outside` capabilities are a checked list** (`crate::origin::CAPABILITIES`: `post`,
  `smtp.send`), in the compiler and the runtime alike.
- **A type is called by its name; nothing is declared to be "the one".** A type's noun is its
  name in words and its plural derived (`TypeMeta::noun`, `::plural`, what `meta(r).noun` and
  `.plural` say); what other languages call it is the package's translations' job, not a search
  line. A query naming an app means its root (`ResourceType::root`), and what the person then
  sees is the app's default view of its root: Mail's is the inbox, so "my mails" is the inbox with
  no further declaration. `noun "…" "…"` and `primary <test>` were search lines for this and are
  removed: `noun` was used by no app, and `primary` repeated the word `primary action` already
  means for something else, to say what the root's default view already says.
- **A document's running text is placed by its view.** `ui.BlockEditor` is a described
  component like any other: it declares a binding to the document's `RichText` (its paragraphs)
  and one to its Markdown `Text` (the same text round-tripped byte for byte), and edits the one
  the view bound. `bind:` takes the binding whose type the bound place has, since a component
  may declare several. So a body is `ui.BlockEditor(bind: doc.body, placeholder: "…")` where
  the document's window draws it; a field marked `(body)` is the document's metadata and says
  nothing about which control edits it, and neither the host nor the compiler places an editor.
- **`std.Repeat` and `std.Reminder` are built-in value types, held as their RFC 5545 text.** A
  repeat is its RRULE (`FREQ=WEEKLY;INTERVAL=2`), with `X-FROM=DONE` appended when the next
  occurrence counts from the day it was completed rather than from the day it was due; a
  reminder is its TRIGGER duration (`-PT15M`, `PT9H`). Both are written from a string literal,
  canonicalized as they are held (so what is stored expands unchanged), and normalized again on
  read, like `Date` and `Time`. They join `Text`, `Date`, `Time` and `std.Identifier` as types
  that are their own text: a field of one is a text column, an action parameter is text, and code
  reads a repeat in words only through `.label()` (E020 otherwise), which is the reader's
  catalogue's phrase and count (`tilolang-repeat-every-weeks` with `count = 2`), never a phrase
  the compiler bakes in. Expansion is `tilo-recurrence`'s, so a rule Tasks repeats by and one
  Calendar draws are one reading. `.next(from, done_on)` steps from the due day or, with
  `X-FROM=DONE`, from the completion day, which is what makes two people completing the same
  item on the same day write the same next day; completion days on different days are last
  writer wins on the due field itself. `.next_at(at, day)` keeps a wall time of day across a
  change of clocks, so it needs the person's zone as its IANA name (`Platform::zone_name`,
  `Core.zone_name`), falling back to the clock's offset when a host names none
  (`Europe/Berlin` 09:30 is `08:30Z` before the spring change and `07:30Z` after).
  `.at(due, at)` places a reminder: from the instant of a timed item, or from the start of the
  due day for one without a time, so `PT9H` is 09:00 and `-PT15M` a quarter of an hour before
  midnight, both in the person's zone.
- **`std.complete(item, done)` advances a repeating item in place.** An app writes one call and
  its own `done` line (`let again = std.complete(t, done)` then `t.done = done and not again`):
  the helper adds the completion day to the item's add-wins `Set<Date>` and moves the due day and
  the time of day to the next occurrence, answering whether it repeats. The type's shape is the
  contract, as `set`/`open` read their fields: one `std.Repeat` or `Option<std.Repeat>`, one
  `Set<Date>` of the days it was completed, one `Date` or `Option<Date>` of the day it is due,
  and at most one `Option<Time>` of the time of day. A type without them gets E118 naming the
  field to declare, and a view or a function cannot call it (E040). Nothing happens for an item
  that repeats nowhere or for `done = false`, so a check that does not repeat is the app's own
  `done` write.
- **A place the view supplies, and the commands only a view can offer.** An action parameter
  written `above: Option<T> (above)` is a *place*: the view offering the action fills it with
  what it shows there — `std.rows` and `std.outline` pass the item just over this one in its
  group, under the same parent, on the same side of the fold — through `actions(x, above)`. The
  store's order never decides it, so a task first under its day's heading is never nested under
  one of another day. An `applies` line states what this view must show for the action to be
  offered at all (`applies above != .none "Nothing above it here"`); a `require` line states
  what the item's own state must be, and a menu keeps that one listed, disabled, with its
  reason. An item menu (a row's, a window's `...`) leaves out what does not apply there, while
  the published `applies` flag beside `available` and `reason` lets a surface that lists
  everything — a palette, an agent's choices — show it disabled, why. The `run` of an offered
  action carries the place it was offered with, so what runs is what the person saw. Two
  actions offered on one kind of item that both ask for a value of one type and write it to the
  same field are one command under two names (E120).
- **A field declared `personal` holds one value per person, in the record everyone shares.** Some
  data about a shared record is a person's own: the tasks Ada picked for her Today, the order
  she put her day in, the day she snoozed a shared mail to. Writing `personal today: Bool =
  false` says so with one word before the field's name, rather than a `Map<Principal, Bool>` the
  store would hold but a filter could not read for the person looking: a read, a filter, a sort
  and a count then ask the field for whoever looks, and an action writes the value of the person
  who runs it. The values are an ordinary register in the field's own key (a map from person to
  value), so they merge, come back through undo and sync as any field does, and each person's
  change stands in history as theirs; the host index keeps one entry per person, so a filter on
  one stays answered from the index. `others(t.today)` reads the people other than the one
  looking whose value is set — presence, not a secret — which is how a view says who else picked
  a task. The field is a `Bool` or an `Option` because a person who never set it must read the
  empty value, and it takes no default but the empty one, no `merge` of its own (its register
  map is its merge) and no role marker (`(title)` and the rest read one value of the record).
  This is not the window's state: `window` and `lens` are the person looking's arrangement of a
  window, and a personal field is a value about a record anyone sharing it may read.
- **An app's costs are counted in the language's own terms, at the line it is written on.** A
  profile attributes fuel, calls, host reads and published offers to the *unit* that ran — a
  view, a row view, an action, a requirement, an `applies`, an at-event argument, a filter, a
  standard library part — and names each by the line its declaration starts on, so an author
  reads a table of their own source. Fuel is metered by the engine in each call's frame, with
  nested calls counted into their caller as well (inclusive), and reads are the store's own
  counters taken across the frame. The units are the exports the module already has, written by
  the compiler into the module's `tilolang.lines` section beside its metadata, so naming costs
  nothing until a run asks for it; what a package costs to open is what it costs to open
  (`tilolang profile`, `Runtime::record_costs`, `assert_costs`), in counts that do not depend on
  the machine, rather than in seconds, which measure the runner as much as the app.
