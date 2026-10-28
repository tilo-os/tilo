# Language changes from writing the apps

Found by reading `apps/tl/*` and `std`. Structural problems are fixed in the model; syntax is
added only where a pattern is common, awkward otherwise, and no existing mechanism covers it.

## Structural

1. **Enum fields with payloads.** A record field can be an enum whose cases carry values; it
   merges as a register of the whole value. Chat's `Part` stops tagging itself with strings
   (`kind: "text" | "block"`).
2. **Creation defaults from recorded inputs.** A field default may use `now` (or another
   recorded input); it's evaluated when the resource is created and recorded with the action.
   `at: Time = now` replaces placeholder values like `Time("1970-01-01T00:00:00Z")`.
3. **Scenarios set recorded inputs.** `as mia at 2026-06-29T07:02Z { post(...) }` runs the
   block with that time as `now`. Seed data stops overwriting fields after the fact.
4. **Typing is personal.** Text being composed is lens state in the view (`lens draft: Text`),
   passed to the action as an argument. A shared `draft` field on a conversation or list is a
   bug: two people typing overwrite each other.
5. **Positions by neighbour only.** Collection moves take `after:` or `before:` another item;
   the compiler rejects integer positions (convergence.md, "Actions").
6. **Roots as expressions.** `root notes.Notebook` gives the workspace's one notebook anywhere
   authority allows, so a dashboard needs no `attach` action or optional fields.

## Syntax

1. **Optional chaining and fallback:** `c.messages.last()?.at`, `x ?? "No messages yet"`.
2. **`if let`, with several bindings:** `if let todos = b.todos, nb = b.notebook { ... }`, in
   actions and views.
3. **Date and time literals:** `2026-06-29`, `2026-06-29T07:02Z`, `2026-08`, matching search's
   literals. Duration literals (`15m`) already exist.

## Library

- `remove_where(predicate) -> Int` and similar collection functions in `std`, instead of loops
  with counters.
