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
   views and code can read it: `created(r)` and `updated(r)` (when, and by whom), `changed(r,
   .field)` (the version that last changed one of its fields) and `history(r)` (versions, each
   with who, when and which action). Apps stop storing their own
   timestamps; Chat's `at: Time = Time("1970-01-01T00:00:00Z")` placeholder goes, and detail
   views show "edited when, by whom" from it. Lists sort by them like by a field
   (`sort_by(c -> updated(c), desc)`), answered by the index; so does `changed` for a field
   (`group: t -> changed(t, .done)?.at.day()`). Code can't rewrite the log, but undo, redo,
   restoring a version and reverting someone's
   changes are ordinary actions on it: attributed, permission-checked and themselves undoable.
3. **Scenarios set recorded inputs.** `invoke(post(...), by: mia, at: 2026-06-29T07:02Z)` runs
   the call as mia at that time, so `now` and `invoker` are what the data was made with. Seed
   data stops overwriting fields after the fact.
4. **Typing is personal.** Text being composed is view state in the view (`state draft: Text`),
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
7. **Roots are named records.** `type Notebook { notes: Collection<Note> }`,
   `root notebook: Notebook`; `type Conversations { channels: Collection<Category | Channel> }`,
   `root conversations: Conversations`. Other code reaches a root by the app's alias and the
   root's name (`notes.notebook.notes`) wherever authority allows, so a dashboard needs no
   `attach` action or optional fields. Views attach with `default view sidebar for Conversations`.

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
5. **`let` in view bodies,** so a row doesn't need helper functions for derived values; the
   type is written where a branch has nothing to say what it is
   (`let heading: Option<K> = if let g = group { g(x) } else { .none }`).
6. **Date and time literals:** `2026-06-29`, `2026-06-29T07:02Z`, `2026-08`, matching search's
   literals. Duration literals (`15m`) already exist.
7. **Lambdas take the values a parameter takes:** `(moved, parent, before) -> move_to(tasks,
   moved, parent, before)` for a parameter typed `(T, Option<T>, Option<T>) -> Action`. One
   value stays `x -> ...`, and a key path stays `.field`. What the lambda makes binds a type
   parameter its parameter's result names, so `group: .due` gives a list a heading of days and
   `group: x -> .some(x.t)` one of words.
8. **A template's item block fills a described component's part:** `std.outline(...) { x ->
   task_row(x) }` gives the `row: (T) -> View` parameter a block that the tree draws once per
   node it shows, so a view written in `std` names the item's parts without the caller
   repeating them.
9. **An item block names the values it draws with:** `std.list(items) { (x, at) -> row(x,
   at) }` for a parameter typed `(T, Option<Principal>) -> View`. The item and who else is at
   it reach the row; the item it stands under the row says itself, as the list item's `parent:`.

Values present themselves: a `Time`, a person or an amount passed to a component renders in the
platform's format (relative times, avatar and name, localized money), never formatted by hand.

## Library

- `remove_where(predicate) -> Int` and similar collection functions in `std`, instead of loops
  with counters.
- `flat_map(x -> ...) -> List<T>`: the lists the lambda gives for each item, joined in the order
  the items and their own lists keep — a group's channels and the channels beside it, one list
  of what both hold. A list of one type fits the list of a union the join gives.
- `index_of(item) -> Option<Int>`: where an item stands among the items, or none when it is not
  one of them — a sibling's place, a run's first item.
- `at(place) -> Option<T>`: the item at the place `index_of` answers for it, or none where the
  list ends before that place — the pair of `index_of`, so a copy of a task is read where the
  task it copies stands.
- `descendants(t, x -> x.parent) -> List<T>` and `ancestors(t, x -> x.parent) -> List<T>`: the
  items under `t`, and the ones it is under, in the order the list keeps them and never `t`
  itself. An item names its parent, so the tree is the list's own: checking a task off takes
  every task under it, opening one opens the tasks over it, and neither walks the list once per
  item.
