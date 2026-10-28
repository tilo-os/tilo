# Language changes from writing the apps

Found by reading `apps/tl/*` and `std`. Structural problems are fixed in the model; syntax is
added only where a pattern is common, awkward otherwise, and no existing mechanism covers it.

## Structural

1. **Enum fields with payloads.** A record field can be an enum whose cases carry values; it
   merges as a register of the whole value. Chat's `Part` stops tagging itself with strings
   (`kind: "text" | "block"`).
2. **Reflection.** The log already records every change with its author, time and action, so
   views and code can read it: `created(r)` and `updated(r)` (when, and by whom) and
   `history(r)` (versions, each with who, when and which action). Apps stop storing their own
   timestamps; Chat's `at: Time = Time("1970-01-01T00:00:00Z")` placeholder goes, and detail
   views show "edited when, by whom" from it.
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

UI code should read like a description of the screen. Views are also what agents compose, so
terse views make agent answers cheaper and more reliable.

1. **Absent renders nothing.** In a view, a `match` without an arm for a case renders nothing
   for it; `if let` works in views; an optional passed where a view goes renders only when
   present.
2. **Key paths:** `.author` where a lambda would only read a field (`author: .author`,
   `sort_by(.due)`).
3. **Optional chaining and fallback:** `c.messages.last()?.said ?? "No messages yet"`.
4. **`if let`, with several bindings:** `if let todos = b.todos, nb = b.notebook { ... }`, in
   actions and views.
5. **`let` in view bodies,** so a row doesn't need helper functions for derived values.
6. **Date and time literals:** `2026-06-29`, `2026-06-29T07:02Z`, `2026-08`, matching search's
   literals. Duration literals (`15m`) already exist.

Values present themselves: a `Time`, a person or an amount passed to a component renders in the
platform's format (relative times, avatar and name, localized money), never formatted by hand.

## Library

- `remove_where(predicate) -> Int` and similar collection functions in `std`, instead of loops
  with counters.
