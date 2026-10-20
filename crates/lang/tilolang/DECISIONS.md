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
- **Keys.** A node key is `<view id>.<ui_call node id>`, or the anchor's id when it has `#anchor`.
  Inside `for` the key is `str_concat(key, suffix)` where the suffix accumulates `"/"+ref_id(item)`
  for record items and `"/"+int_to_str(index)` for other values.
- **Layout arguments** (layout.md): unit literals `11rem`, `4dp`, `1fr` are `Length` values; a prop
  with a length is `ui_prop_f(n, "<name>.<unit>", value)` (`"min.rem"`). Rhythm tokens, alignments
  and positions (`gap: section`, `at: bottom_trailing`, `collapse: drawer`) are `ui_prop_h` string
  props. Bare flags (`truncate`, `page(full)`) are `ui_prop_i(n, "<flag>", 1)`; `fill` is
  `ui_prop_f(n, "fill", 1.0)` and `fill(2)` carries the weight. `aspect: 16/9` is constant-folded to a
  Float. `primary`/`secondary` are primitives (slots of `split`); the compiler does not check they sit
  in a `split`. `column` stays as an older name of `stack`. `;` separates statements on one line.
- **`embed(source, author: who)`** (implementation.md 7.8) is a UI primitive with two `Text`
  arguments, both required (E022 when one is missing, E020 when not `Text`). It emits
  `ui_node("embed", key)` with `ui_prop_h` props `source` and `author`; it takes no other
  arguments than the sizing ones. The host fills in the node's children (see the runtime's
  DECISIONS). The principal is a `Text` id in v1.
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
- `try act(args)` (flows only, E044 elsewhere) invokes an action and reports whether it worked:
  a `Bool` for an action without a result, an `Option` of the result otherwise (`none` when the
  invocation was denied, declined or trapped). It compiles to `invoke` then `is_error(h) -> i32`;
  a scalar result is already boxed by the host, so the `Option` is that handle. Without `try`, a
  failed `do act(..)` yields an error value that traps when its scalar is unwrapped.
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

## Deviations to report

- E072 (a block with no value) is defined but cannot be reached from source today.
- `layout-check` (layout.md section 6) is not part of this crate.
