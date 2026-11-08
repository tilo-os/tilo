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
  calling run's journal. An `action` still goes through `invoke`, and its own door.
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
  of the workspace's members.
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
- An action invokes another action by calling it, its own or another package's; a `fn` changes
  nothing (E040) and calls no action (E043). In a view or a block, applying an action yields an
  `Action` value (`act_ref`), so an action can be offered as a button or a menu item there.
- `sort_by(x -> key)` compiles to a loop that collects the keys into a parallel list and one call of
  `list_sort_by_key(items, keys) -> list`: stable, ascending or `desc` (the same list reversed),
  keys Int, Float, Text or Bool (E020 otherwise), and a Date or Time, which sorts as its ISO text.
  A key that may be absent sorts after every item that has one, whatever the direction, so the
  items whose day is not known stand last and a list grouped by a day sorts by the same field.
  A store that answers the sort from its index keeps that order and the list's own for the items
  a key cannot tell apart. There is no `list_set`.
- `try act(args)` (in an action, E044 in a `fn`, a view or a block) invokes an action and gives a
  `Result<T, Text>`
  (`Result<(), Text>` for an action without a result): `.ok(result)`, or `.err(reason)` when the
  invocation was denied, declined or trapped. It compiles to one import, `try_invoke(act) -> h`,
  which invokes like `invoke` and builds the `Result` value in the host. Without `try`, a failed
  invocation yields an error value that traps when its scalar is unwrapped.
- `suggest act(args)` records the action as a proposal on the resource it is about instead of
  applying it: an agent's block whose whole expression it is, and a statement in an action
  (E044 elsewhere). An action records it once its own change has committed, so the proposal
  is never part of that transaction. It compiles to one import, `suggest(act)`, and
  `proposals(of)` draws what waits on `of`, each card naming who proposed it; accepting one runs
  it as the person accepting, and the change it makes records who proposed it.
- Sets and maps of Int and Float take their elements and values boxed with `some_i`/`some_f`
  through the existing `set_add`, `set_remove` and `map_put`. Set elements of Int or Float are
  stored under their text form, so a set iterates in text order.
- A record-typed field must be `Option<T>` unless every value is always set: reading an unset
  reference traps (there is no value to return). A removed target stays a readable reference
  (it compares equal); reading its fields fails.
- **`uses web, random, time` is written after the signature, before `proposal` and the options**
  (`action snooze(t: Thread, until: Time) uses time (icon: .clock) { ... }`). It reads as part of
  what the declaration is, and the canonical form keeps it there. The words are ordinary names,
  not keywords, as `require` is: `web` and `time` stay usable as identifiers.
- **What an effect is, and what is not one.** An effect is what a replay cannot compute again,
  so it is recorded on the invocation that read it: the clock and the person's zone (`now`,
  `today`, a time of no zone, `t.day()`, `t.time_of_day()`, a text parsed near a moment,
  `next_at`, `at`), a draw and a service outside Tilo. A filter's `today` is
  the query's input, read where the view is drawn: the query is answered again when the day
  turns (`ViewState::changes_at`), nothing is replayed, and the checker does not ask an action
  for `uses time` because a view it shows filters by a day. `t.day()` in a view is the same
  kind of read — what the person's zone makes of a value that is drawn — and is not refused
  there. `now` alone is refused outside an action (E049): it is the time of an invocation, and a
  view has none.
- **A scenario uses every effect.** Its code is seed data, run where a developer runs it and
  never replayed, so it declares nothing and reads anything; `as who at time` gives its actions
  their time.
- **The journal is kept beside the change.** An action's effects go into the change note the
  store keeps with the change (`ChangeNote::effects`), in the order it read them, so history can
  say what a change was made of ("fetched … 304"). A run's go into its
  [`crate::run::Journal`], saved as it goes, so a run resumed tomorrow reads the moment it
  started. One entry per invocation holds the moment and the zone; a draw keeps its bound and
  its value; a fetch keeps the request and the answer.
- **A draw is one primitive: a number below a bound.** `random.int(lo, hi)`, `random.float()`,
  `random.choice(xs)`, `random.shuffle(xs)` and `random.id()` compile to imports `random_int`,
  `random_float`, `random_choice`, `random_shuffle` and `random_id`, and each is a count of
  draws of `0..upto` the host records: the die is one of six (the host adds `lo`), a float one
  below `2^53` over `2^53`, a choice and each step of a shuffle one below the number of items
  left, an id four below `2^32` written as eight hex digits each. So a draw's entry in the
  journal is the primitive, not the number a person would name: `random.int(1, 6)` records a
  draw of `0..6` whose value is the die minus one. A choice of an empty list draws nothing and
  records nothing, so it returns `.none`.
- **A draw happens where the invocation is.** Only an action draws, and only one that declares
  `uses random`; a `fn` does not: it computes from the values it is given, and a view or
  a block that called it would be drawing. A view or a block does not, since neither has a
  journal. `random` is a module of draws and nothing else: an unknown member is E014, and a
  choice or shuffle of something that is not a list of values is E020. A range is drawn uniformly
  by drawing again at or above the largest multiple of its width, so no number comes up likelier
  than another; `random.int` traps when its range holds more numbers than a draw's bound
  (`hi - lo + 1` past `i64`).
- Pinned references (`@kind:id@version`) parse but compile to E070: `ref_lit` has no version.
- Integer division and remainder trap on zero, as in wasm.
- `proposal view` may also be written `proposal view(a, b)`, naming action parameters.
- **A role says what an action is; only a `scenario` keeps it from being listed.**
  `primary`, `destructive` and `migration` are the three words. `offered_actions()` skips the
  `scenario` actions (the bodies scenarios run) and the migrations, which the platform runs
  itself; every other action is in `actions(r)`, in the host's bar and a window's menus, and in
  an agent's choices. An action is never hidden from people to reserve it for a gesture: a
  helper nobody chooses is a `fn`, and one that writes is a command like any other.
- **A window's bar shows its primary action and the commands that toggle a state of what the
  window shows as buttons, and every other action behind the bar's one menu.** The host builds
  them that way from what the declarations are: the primary action is the Primary button, an
  action with an `on:` stands as the control that shows whether it is on (`on: s.bar_shown`, a
  toggle the person flips where they see it), and the remaining actions are in the menu, which
  the shell's own More button holds whatever room the bar has. An app does not say where its
  actions stand: an action declares what it is and does — label, icon, role, the state it
  toggles, its key, its parameters — and placement follows from that, so no hint can drift from
  the action, and every surface (menus, palette, agents, history) reads one description. A view
  that places the actions of what it shows says their order and which one leads
  (`actions: [check(primary: true), schedule(group: "Plan")]`). A view never reads the actions
  back to draw them: it says what can be done by declaring actions on the type and by placing
  the typed affordances of the thing itself (a checkbox that completes, an entry that adds, a
  drag). `actions(r)` is therefore an error in a view (E047); it stays readable to the host,
  which places the declarations, and to an agent, which chooses among them.
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
- Blocks: an expression is checked as a value in view-like context (actions become
  `Action` values); a UI primitive or foreign view call, or a view body, is a `View`. The block module
  is `app agent.block.<run id>` (`main` for `compile_block`) and exports `memory` and `b_main`.
- `module_hash` is the SHA-256 of the module without the `tilolang.meta` section.
- **A primitive that is a component is only that component's shorter name.** `text`,
  `rich_text`, `section`, `avatar`, `progress`, `toggle`, `checkbox`, `input`, `rich_input` and
  `image` are `ui.Text`, `ui.BlockEditor`, ... : `prims.rs` names the component (`Prim::component`)
  and `checker::component::component_prim` checks and lowers the call as the component's own, so
  its props, slots, events and bindings are the description's and nothing is added. `button` (a
  declared action's button) builds `ui.Button` (`Prim::builds`) because its arguments are the
  statement form's own. `facts(of)` reads the fields the type declares and builds from them one
  wrapping row of chips, each the field it shows (a day's `ui.DateField`, a person's chip); what
  a page opens with is its view's own, as what it says under the heading is. Removed with
  replacements named in
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
  named as the changes that made them (`Derive::notified`), so a view of
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

## The standard library is a package

- **`std` is `tilo.std`, compiled from its source** (`stdlib.rs`, `std/app.tl`, `std/app.ids`).
  It names its app like any package, locks its ids like any package, and reaches every module
  through the same `Env::add_import` a `use` goes through: there is no views-only parse, and no
  second path by which its names become code's. A package sees its types, enums, functions,
  actions and views as `std.X`.
- **Its own module is not an import of itself.** Compiling `std` registers the alias `std` for
  its own app, so its views call each other by their plain names and see its own declarations.
- **Every package depends on it.** `meta.uses` carries `tilo.std`, and a bundle carries the
  library's module before the packages the app `use`s, so the package id its declarations
  belong to is loaded wherever a record of one of its types is read or written.
- **What a library declares is the library's.** A resource type declared in a library package
  is rooted under that package's id: the records its actions write are ordinary workspace
  records under it, and a package that uses the library writes them through the library's
  actions, under the authority of whoever applied them.

## Template views

- **A template view expands where it is called** (`checker/template.rs`). Its name is taken at
  the call, its body checked there, and what it builds is the caller's: its nodes, its reads,
  its view state, under the caller's authority. Its type parameters, state, function and view
  parameters come from the call.
- **`template view v(...)` declares one.** A view is a template when it is written that way, or
  when it takes type parameters, or when a parameter takes view state (`State<E>`), a function
  or a view. A template has no function of its own and appears in no `meta` view list: it is
  expanded by the compiler, and only from the source the compiler has.
- **View state inside a template is the caller's.** A `state` declaration at the top of a
  template body becomes state of the view the template expands in, keyed by that view's id, the
  template's name and the state's: the same template called twice in one view keeps two states.
- **Every view of `std` is a template.** The library builds the caller's frames; it runs no
  views of its own. A test holds that (`stdlib.rs`).

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
- **No field markers.** A field is its name, its type, its merge and its default. A record is
  named by its first `Text` or `RichText` field, cut to its first non-blank line; where a view
  does not draw it (a link, a search result, the palette, history, a notification) it is shown
  through its type's `row view`, and a type without one shows its type's name and
  `created(r)`. What a field means to the UI is the app's own views' business, not the type's.
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
  `state` (at the start of a view statement, before `name:`), `root` and `default` (before
  `view`, at the start of an item) are contextual, so they stay usable as names. A keyword may
  name a call argument (`open(r, view: v)`).
- **Calling an action** runs it: in an action's body, `act(args)` is a call, which the runtime
  applies as a change of its own (see "Actions that call actions"). `do act(args)` is a syntax
  error that says so.
- **Anchors** follow the call: `button("+1", on: inc(c)) as plus`. `#plus` is a syntax error
  that says so. An anchored node keeps the anchor id as its key, so an anchor belongs on a node
  that appears once in its view's tree, not on a row of a list.
- **View state.** `state name: T = default` is declared at the top of a view body (E066
  elsewhere); its id is `view.name` in `app.ids`, and views list them in metadata (`state`: id,
  name, type). It is what its person keeps in that view — how their window shows the resource,
  and where they are in it — so it is kept per person and window, synced to their devices and
  visible to others as presence. Without a default, Int, Float, Bool, Text, options, lists and
  payload-less first variants start empty or first; other types need one. The view reads a slot
  with `state_get(id, default) -> h` (scalars boxed like `some_i`), which returns the slot's
  value or the default; the host records the read. `bind: name` on a control is
  `ui_bind_state(node, id)` (Int, Float, Bool or Text state, E062 otherwise). `set(name, value)`
  in a view is `act_set(id, value) -> h`, an action reference the host applies to the slot; it
  carries no capability and is never a change to a resource. State is the declaring view's own:
  other views and nested view calls do not see it (pass values or `Action`s as parameters).
- **`open(r)`** and `open(r, view: v)` in a view are `act_open(r, view id or "") -> h`, an action
  reference the host turns into a request to go to the window of `r` (E067: `r` is a record, `v`
  a view that takes exactly an `r`). Blocks cannot open windows yet.
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
  actions, no `invoker`, no `now`. Only an action's parameters take one (E028 for a fn, view or
  variant field). Each default is a function of its own, exported as
  `d_<action id>_<param>` and taking the parameters before it; metadata lists it on the
  parameter (`default`: the export and the default as written, for descriptions).
- **When it is computed.** When the action is invoked without the argument, on the invoker's
  state, and the value is recorded as the argument: the runtime completes an invocation's
  arguments before it decides, journals or runs it (`Runtime::complete_args`), so the change
  note, a suggestion and the platform's recorded invocation all carry it. An application from a
  view or a block may leave out only its last parameters (E028 for a gap), which the runtime
  computes when the application is invoked: a button computes the default when it is pressed,
  not when the view renders. A call from an action computes it at the call, on the state the
  call sees. An explicit argument always wins.
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

## Actions that call actions

convergence.md, "Calls"; action-system.md, "One invocation path".

- **A call is a change of its own.** `task(t)` in an action's body — of its own package or another
  one's — goes through the door: the authority decides it on the caller's authorship chain, it
  runs at its own door as a change of its own, and it may wait for a person's approval. So an
  action that calls actions is not atomic across its calls: nothing a call made is lost if a
  later call is refused or fails. Storage allows nothing else: the callee may be another app's,
  whose change another origin holds, so one commit cannot cover the calls.
- **The caller's work is a change per call.** Its own writes go into one transaction, committed
  before the call it makes runs, and again when it returns: an action that calls actions is not
  atomic across its calls either. The caller's writes before a call are therefore in the record
  when the call runs, so a call on a resource the caller itself creates or changes in the same
  action sees it, and a call's change stands on top of it. Its writes after a call are the next
  change, so an action that writes once and calls once makes two changes and one call.
- **A run is replayed, not resumed.** When a call needs a person, the run's journal is saved and
  answering the person runs the code again from its start with the journal answering every read,
  every write and every call it already made: it takes the same branches, it does not write what
  it wrote again, and its earlier calls are not made twice. The ids the store gives the items a
  run inserts are journaled like reads, so the replay names the same items. What the caller wrote
  before the call is already in the record; what the replayed run writes after the call is its
  next change.
- **`try act(args)`** gives the `Result` of a call, and a call that failed reads as `.err` there.
  Without `try`, a failed call is an error value a program traps on, as before.

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
- **The entry keeps reading words, not the language.** An action's one `Text` parameter is what
  a typed entry fills: a call of the action leaves it for the words, and the runtime's own
  scanner (`Core::entry_fill`, `natural`) titles it. The parameter carries no marker of its own
  (`(entry)` is removed); a view that offers the entry names the action as its `entry:`, and an
  action with none or several `Text` parameters takes no words (E068). The scanner fills it, so
  the phrase filling an action's day and moment does not need the language; `Date.parse`/
  `Time.parse` are for code that reads words itself and names the moment it reads them at.
  `dated(text, now)` (which answered the rest of the text with its day and moment) and
  `day_of(t)` (which `t.day()` is) are removed with no alias.

## Search

search.md, "What a type declares" and "Types, references and paths".

- **Every text is indexed.** Search reads every `Text` and `RichText` field of a resource, in
  declaration order; the first is what a result is called by, the rest are found by their
  words. No marker gives one text more weight than another.
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
- **A filter's option is declared** with the annotation enum cases and actions take:
  `filter done (icon: .circle_check)`, `filter "Today" = due <= today (icon: .star)`. After a
  named filter's predicate the parenthesis would read as a call; one that names only options is
  the filter's options. `icon:` is what its control shows beside its label. A filter that
  declares none has no icon.
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
  collection; one across the workspace would follow no change. Actions may query
  the workspace (`Store::select`, answered by the index's columns across the workspace).
- **The places are the app's, in its own state.** A template may declare `state` at
  the top of its body; it is the expanding view's, kept under `<view id>:<template>.<name>` and
  listed in that view's metadata. `std.places` draws the filters the app names, after all of the
  items (`everything:`, "All notes"), and selects the app's own `chosen` state with their query
  as people type it in search (`is:done`); the app starts a window on a place by giving that
  state its default (Letters starts on `is:drafts`). No template scans `filters(T)`: the window
  offers what its app declares, and a place of days or projects is the app's own `ui.NavList`.
  Places draw no badge: a count is a query of the app's own (`items.matching(p.query).len()`),
  which a view that counts writes.
- **A window that is only a list is `std.browse`.** It writes that window from the inputs the app
  names alone — `of`, `items`, `places`, `sort` with the order each case asks (`by`), `everything`,
  `empty`, `entry` — with its query state in the order chosen, the title and count naming the place
  chosen, and the pane beside the list when rows open there. It reads no layout off a type, scans
  nothing, and takes `opens` from the type alone. An app whose window needs a sidebar of counts,
  grouped rows or a bar action of its own composes `std.list`, `std.places` and `std.header` itself.
- **A list groups by the key its caller gives.** `ui.ListItem(group:)` takes the item's own key and
  heads each run of items naming the same key with the key itself, as the platform presents a
  value of its kind — a day as Today, Tomorrow, a weekday; a word as it is written. Items that
  name no key stand under the list's `ungrouped_title:`, in the order the caller gave them;
  a row's `tail:` names the items done with, under the list's `tail_title:`. A place that groups or folds by what it says rather than
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
- **`invoke(call, by: who, at: time, from: r)` runs an action as a person, at a time, from a
  resource.** It gives what the call gives, and each one is a change of its own: what came
  before commits first, so history names each change as the person who made it, at their time,
  and rolling back on a failure undoes the last one only. `by` defaults to `invoker`, `at` to
  now, `from` to nothing; a call made from `r` records it as an invocation on `r` is
  (`created(r)?.from`). Only a scenario writes it, since seed data is what shows a provenance
  feature in a capture; elsewhere an action runs as whoever invokes it, now.
- **A run stages the view state its windows stand on.** `layout-check --state place=Task.done`
  names a state a view declares, and the value as the app's own code writes it: a filter of the
  type the state queries, a case of its enum, or a literal a day, a time, a number, a truth or
  a text takes. It sets the declaration, not an instance: the host reads a declared state as
  the person's pick, then what the run staged, then the declaration's default, so a window that
  opens later stands on it too. A person's own choice wins over it, and what a run stages is
  never written as their change — a capture shows a place no one picked without recording that
  anyone did. It is no language construct: laying out a window at a state is a run's option,
  not a scenario's, so app code cannot stage a person's place.
- **`a.then(b)` joins actions.** It is a method of action values, so it means the same wherever
  one is written; `b` runs once `a` is done. A composer sends its draft and clears it with
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
- **A type name is another name for a type.** `type X = T` names `T` and is nothing more: a
  resource is a `type` with fields, so what holds a collection holds it as a field
  (`type Notebook { notes: Collection<Note> }`). A root is named (`root notebook: Notebook`)
  and reached as `notebook`, or `alias.notebook` from another package.
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
  the document's window draws it; neither the host nor the compiler places an editor.
- **A repeat and a reminder are an app's fields, not the language's types.** The language holds
  general mechanisms only: a recurrence rule is `Text` the app stores as RFC 5545 writes one
  (`FREQ=WEEKLY;INTERVAL=2`), a reminder is the `Duration` before the due moment, and the words
  a row says them in are the app's `phrase(...)` with the count it computed, in the reader's
  catalogue as every phrase is. `std.Repeat`, `std.Reminder`, their `.next`, `.next_at`, `.at`
  and `.label` methods and the five host imports behind them are gone: a calendar or a task list
  that expands a rule does it over `Date`, `Time`, `Duration` and the shared `tilo-recurrence`
  crate, whoever needs the next occurrence. The zone a wall time of day is read in stays the
  language's (`wall_clock`, `t.day()`, `today`), since every app reads a moment by the person's
  clock.
- **A place the view supplies, and the commands only a view can offer.** An action parameter
  written `above: Option<T> (above)` is a *place*: the view offering the action fills it with
  what it shows there — the list passes the item just over this one in its group, under the
  same parent, on the same side of the fold, as its row says (`above:`) — through
  `actions(x, above)`. The
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

## Web requests

effects.md, "Web". A request is an effect whose answer is kept, so the language needs a value for
what came back and a rule for when it is sent.

- **`web.get` and `web.post` answer a `Response`, and `Response.body` is a `Bytes`.** `Bytes` is
  the buffer itself (`len()`, and `text()` decoding it as UTF-8 with the bad bytes replaced); a
  `Response` answers `ok`, `status`, `status_text`, `headers` and `body`, and `text()` reads the
  body's text, so `web.get(url).text()` is one call. Both are values a call answers, not types a
  field may hold or an action may take (E118, "cannot be stored"): the answer belongs to the run
  that made it, and a program keeps what it read by writing it into a record's `Text`.
- **A request is sent once, where the invocation runs, and its answer is journalled.** The first
  run asks the invoking device's transport; the `Web { request, response }` entry it writes is
  what every later run and every resume answers from, so a replay makes no request. The journal
  keeps the request whole — method, URL, headers, body, `if_none_match` — and a run that reaches
  the effect with a different request than its journal recorded traps: it diverged from the
  journal, and answering the recorded reply would answer a question nobody asked.
- **A failure is a value.** A refused host, a timeout, a network error and a reply over the limit
  all answer a `Response` with `ok` false and status 0 (the recorded error in `status_text`), so
  an app decides what to do with it like any other outcome. Determinism comes from the journal
  entry, not from the transport: replaying a failed entry fails the same way.
- **The hosts are declared by the package, and the workspace may revoke one.** The app header's
  `(web: ["example.com", "*.rss.org"])` is the grant: `PackageMeta.web`, checked at the build and
  kept in the manifest, so an install shows the hosts before the app runs. The runtime refuses a
  request to a host the calling package did not declare, as a value, and records it. `*.suffix`
  covers subdomains only (not `suffix` itself), and only `http` and `https` URLs reach a
  transport. Revocation is `Authority::may_fetch`, which the platform answers from the grants it
  installed minus the hosts the workspace revoked (`TiloAuthority::revoke_web`), so a revoked
  host is refused the same way without editing the package.
- **Limits are fixed, not negotiated.** One request waits at most 30 seconds and answers at most
  2 MiB; a longer body is refused with a status-0 `Response` naming the limit. Streaming and
  redirect policy beyond the transport's own are not part of this effect.

## Triggers

An action the app runs by itself is declared with `trigger` (README.md, "Triggers"). The
language only describes it; the host runs it, so the choices are about what the declaration
holds and what the host reads from it.

- **`trigger name(r: T) every <duration>: action(r)`, or `at r.field`.** A trigger names exactly
  one record type — the records it runs for — and exactly one action of its own package. The
  action's arguments are the record it runs for (`r`) or a root (`root Board`); anything else
  would have the host compute a value without running the package. An action parameter the
  trigger does not give must have a default.
- **`every` is pinned to the epoch, not to when the app started.** `every 30m` runs at the start
  of each half hour on the wall clock, so every device that runs a trigger agrees on its
  periods, and the period a run belongs to is the same everywhere it is later read.
- **`at` reads a `Time` or `Option<Time>` field of the record, and the record is the mark.** The
  trigger fires once when the stored instant has come; the host records the instant it fired at,
  so the same instant does not fire again, and a changed instant (a later one, or an earlier one
  that has already passed) fires once more. The field is read by the host, never by the package,
  so a time in a field of some other shape is refused by the compiler (E133).
- **The run is recorded in the change it makes, in the same edit.** The host writes what the
  trigger last did for the record (the period it ran in, or the instant it fired at) into the
  record's `$triggers` map, in the same change as the action's own writes (`DynEdit::TriggerRan`),
  and the change is committed under the app's own principal, `trigger:<app>`. A second run of the
  same period or instant is refused where changes are applied in order, so two devices cannot
  both count a run.
- **One device runs them: the workspace's origin.** Only the replica that governs the workspace
  (the same one that runs migrations) invokes triggers, so nothing has to be elected and no
  device depends on being asked. A device that opens the app later reads the marks and finds
  nothing due.
- **Runs missed while every device was off happen once, at the next turn.** `every` compares the
  period `now` is in against the mark, not the number of periods that passed; `at` compares the
  instant the field holds against the mark. Five hours off is one run, not ten.
- **Nothing polls.** The host waits until the next instant a trigger is due (the next period's
  start, or a record's time still to come) and wakes then; it never returns an instant already
  passed, so a refused run is not retried in a loop.
- **A trigger runs for the package's records, as the platform stores them:** the resources of
  the trigger's type at the top of the app's tree (what migrations and publication see), not
  items inside a collection.
## JSON

Typed decode into records, enums, lists and options, encoding back as text, and a value for a
document whose shape code asks as it goes; what can go wrong is a value. `json` is pure: what it
reaches is the text and the value it is given.

- **`json` is a compiler-known namespace, like `random` and `web`, and not a package in `std`.**
  The interface is typed by the type it is given — `json.decode<T>(text)` — and tilolang has no
  `fn` with type parameters, no reflection over a name and no way to build a record value outside
  the runtime's own record path, so a package could not write it; a `std` package wrapping a
  compiler-known call would add a layer and no rule. The compiler recognises the namespace where
  one is not imported (`checker/call.rs`), checks the type argument, and leaves the reading and
  the writing to the runtime (`tilolang-runtime/src/json.rs`); the layout is the same as `web`'s.
- **`json.Value` is a built-in value type, not a declared enum.** Its cases are JSON's six kinds,
  and two of them hold further values (`Map<json.Value>`, `List<json.Value>`): a declared enum's
  case carries no payload here (a case with data is a type of its own, in a union), so an enum
  cannot be recursive, and no declared type could hold a document of unknown depth. `Ty::Json` is
  therefore a case of `Ty`, the mirror of `Bytes` and `Response`: a value a run holds, and no
  field's type (E023). `match` over one is `match`'s existing machinery with a `Shape::Json`: six
  kinds, all named, `Imp::EnumIs` against the kind's name, and the payload read through
  `Imp::EnumGet*`.
- **Decode is by field name, strict about required fields, and silent about the rest.** A record's
  field is read from the key its type declares it under (`FieldInfo.name`), so a type is free to
  rename a field's Rust-side id without changing the text it reads. A key the type does not
  declare is ignored rather than refused: a service growing a field must not break its readers.
  A field the text leaves out is its declared default, `none` for an `Option`; a required one
  missing is an error, because a required field with no value would otherwise trap later, at a
  read whose message says nothing about the text. The type a decode names is one a call answers,
  so it is no container: a `Map`, a `Set` and a `Collection` are fields' types, and a document's
  object reads into a record, or into a record's field of a `Map`.
- **Errors are values, and name the JSON path.** Decode answers `Result<T, Text>`; a text that is
  no JSON answers the parser's line and column, and a value whose shape is not the type's answers
  its path from the root (`$`, `$.items[2].title`, `$.by_key.title`), the value it found, and what
  the type declares. Encoding a value JSON has no form for (a `ui` node) traps: it is a mistake in
  the code, not in the data it read. A stored resource traps too, and by decision rather than as
  an omission: a record's JSON is not one document, since two resources may refer to each
  other, so there is no single text it is; code that posts a resource writes the fields the
  service is given into a record and encodes it.
- **Int and Float follow the field, not the digits.** JSON has one number; tilolang has two. A
  field declared `Int` reads a whole number that fits (`5`, `5.0`) and is an error otherwise; a
  field declared `Float` reads any number. A `json.Value`'s `.number` is a `Float`, since what the
  document meant is unknown; `Float.to_int` converts it once code decides. Encode writes an `Int`
  without a fraction and a `Float` as the number it is, refusing a value no JSON can hold (`NaN`).
- **A decoded record is a value, not a resource, until it is inserted.** The language's records are
  resources: created, stored, referenced. `json.decode<List<Item>>` has nowhere to store anything
  and no ids to give, so it answers record values, which a collection insert turns into items (the
  one thing a record value is for, `coll_insert`), and whose fields read as the text gave them
  (`res_get` reads a `Value::Rec`'s field, or the type's default for one the text left out). A
  field of a record that holds a resource (a record, a collection, a set) is an error at its path:
  a stored field of that type holds a reference, and text makes no reference.
- **Encoding writes the type's shape.** A record's fields by their declared names, in declaration
  order, a field it does not hold as its declared default (so a `none` is `null`, and a reader
  sees every field); a case of an enum as its name; a `Map` as an object with its text keys (a
  key that is no text traps); an `Option` as its value or `null`. A `json.Value` written is the
  document it was read from, unchanged.
- **No effect, and nothing journalled.** Decoding and encoding reach nothing outside the text and
  the value they are given, so an action needs no `uses` for them and a replay computes the same
  answer from the same text. Only the request whose answer the text came from is an effect, and
  that one is journalled (`## Web requests`).
