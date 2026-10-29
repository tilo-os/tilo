# Language changes from writing the apps

Found by reading `apps/tl/*` and `std`. Structural problems are fixed in the model; syntax is
added only where a pattern is common, awkward otherwise, and no existing mechanism covers it.

## Structural

1. **Union types.** Types compose directly: `type Channel = TextChannel | VoiceChannel`, usable
   wherever a type is (fields, parameters, `Collection<Category | Channel>`), and matched by type:
   `match c { t: TextChannel => …, v: VoiceChannel => … }`. A union field merges as a register of
   the whole value. `enum` is only for plain named cases without data (`enum NoteOrder { created,
   title }`).
2. **Reflection.** The log already records every change with its author, time and action, so
   views and code can read it: `created(r)` and `updated(r)` (when, and by whom) and
   `history(r)` (versions, each with who, when and which action). Apps stop storing their own
   timestamps; Chat's `at: Time = Time("1970-01-01T00:00:00Z")` placeholder goes, and detail
   views show "edited when, by whom" from it. Lists sort by them like by a field
   (`sort_by(c -> updated(c), desc)`), answered by the index. Code can't rewrite the log, but undo, redo, restoring a version and reverting someone's
   changes are ordinary actions on it: attributed, permission-checked and themselves undoable.
3. **Scenarios set recorded inputs.** `as mia at 2026-06-29T07:02Z { post(...) }` runs the
   block with that time as `now`. Seed data stops overwriting fields after the fact.
4. **Typing is personal.** Text being composed is lens state in the view (`lens draft: Text`),
   passed to the action as an argument. A shared `draft` field on a conversation or list is a
   bug: two people typing overwrite each other.
5. **Positions are recorded by neighbour.** A move names a neighbour (`after:`, `before:`) or a
   place (`to: i`), which is resolved to its neighbour when the action runs (convergence.md,
   "Actions"); the language has no storage-shaped restriction on positions.
6. **An item is its id.** Items are stored flat by id, with the collection they are in as a
   field of their own (convergence.md, "Items"). A reference is the id, never the path down
   to the item, so storage no longer shows in the language: a card moves between columns with
   `move` on the column it goes to and stays the same card, instead of an app keeping a
   `column` field beside one collection of every card.
7. **Roots are named values of any type.** `type Notes = Collection<Note>`, `root notes: Notes`;
   `type Channels = Collection<Category | Channel>`, `root channels: Channels`. No wrapper type
   named after the app. Other code reaches a root by the app's alias and the root's name
   (`notes.notes`) wherever authority allows, so a dashboard needs no `attach` action or optional
   fields. Views attach with `default view chat for channels`.

## Syntax

UI code should read like a description of the screen. Views are also what agents compose, so
terse views make agent answers cheaper and more reliable.

1. **Absent renders nothing, by uniform rules.** Syntax means the same in views as anywhere
   else: `match` is exhaustive everywhere, and `if let` works the same in actions, functions and
   views. A slot typed `Option<View>` renders nothing for `.none`; that follows from its type.
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
