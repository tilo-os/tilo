# tilolang

The compiler of tilolang, the Tilo app language, and its command line. The design is in
`docs/designs/app-language/`; the grammar is [grammar.md](grammar.md); the choices this crate
made where the design is silent are in [DECISIONS.md](DECISIONS.md).

## A package

A package is a directory. Every `.tl` file directly in it is part of one module, and exactly
one of the files starts with the app header:

```
apps/tl/counter/
  app.tl        app so.tilo.counter "Counter"   (the header, then any declarations)
  views.tl      more declarations; no header
  app.ids       the ids lock: generated, committed
```

How the declarations are spread over the files does not matter: a file may use what another
declares, in any order. Diagnostics name the file and the line within it.

The ids lock gives every declaration its stable id. The compiler adds new declarations to it and
retires removed ones; commit it with the sources. Rename a declaration with
`tilolang ids --rename old new`, so it keeps its id and with it the data stored under it.

Rename the app itself by giving it its new id and naming the old one in the header:
`app so.tilo.lang.tasks "Tasks" (previously: so.tilo.lang.todo)` (several in brackets). The
records stored under the old id are the app's: they open in it where they are, with the ids they
have, and its actions change them there; what it creates goes under the new id. The id it had
before is signed into the app's manifest, and installing it over an app of that id from another
publisher is refused; from the same publisher it takes that app's place.

A package migrates its own records when its schema changes shape. The field the type no longer
uses stays, marked `(previously)`: its values stay in the records, and only a migration reads
it. A `migration action` on the type moves what it held to where the type keeps it now:

```
type Column {
    title: Text (title)
    cards: Collection<Card>
}

type Board {
    columns: Collection<Column>
    cards: Collection<Card> (previously)
}

/// Puts each card of a board from before into its column, in the order the board showed them.
migration action place_cards(b: Board) {
    for card in b.cards {
        let col = card.column
        col.cards.move(card, to: col.cards.len())
    }
}
```

The platform runs it once on each resource of its type that holds something in a `(previously)`
field (a type without one: once on each resource), as an ordinary action of the person whose
window opened the data: it is in history, undone like any other, and records in the same change
that it ran. A migration is never offered. It cannot delete anything; moving items between
collections keeps their ids and history.

To start a package, make a directory next to the others with one file:

```
app my.counter "My counter"

type Counter {
    value: Int merge counter
}

root Counter

default view counter for Counter

primary action increment(c: Counter) (icon: .plus) {
    c.value += 1
}

view counter(c: Counter) {
    page {
        text("{c.value}", role: .numeric)
        button(increment(c)) as plus
    }
}
```

`root Counter` makes `Counter` a root type: one resource of it per room, which the launcher
opens through the type's default view (`default view counter for Counter`).

`use other.app as other` imports another package. Imports are found among the package's sibling
directories (for `apps/tl/today`, the other directories of `apps/tl`); `--path <dir>` adds a
package directory, or a directory of packages, to look in.

A package may also `use` a native app: `use so.tilo.chat as chat` reads what Chat publishes, the
resources, records and enums of its committed `surface.toml`, as types (`chat.Workspace`,
`chat.Channel`, `chat.ChannelKind`) and its verbs as actions, and checks field reads and action
signatures against them. The language's tool (`tilolang check`) finds native apps among the
`--path` directories (`--path apps/chat`, or `--path apps`). Reading native records at runtime and
invoking native actions come later; for now this is compile-time only.

## Syntax reference

The full grammar is [grammar.md](grammar.md). In short:

**Declarations.** A package is `type`s, `enum`s, `action`s, `flow`s, `view`s and `fn`s, plus two
package-level declarations (workspace-model.md, "Apps"):

```
type Note { title: Text, pinned: Bool = false, tags: Set<Text> }

enum NoteOrder {
    created
    title
}

type Notes = Collection<Note>          // a resource that is a collection
type Channel = TextChannel | Thread    // a resource of either type

root notes: Notes                      // one per room; the launcher opens it
default view notebook for notes       // what a window of the root shows
default view note_editor for Note     // what a window of a Note shows
```

**Enums** are plain named cases. A value is a dot literal resolved from the type the context
expects, or written with its type where none is expected: `let o: NoteOrder = .title`,
`NoteOrder.created`. A field of an enum type is a register.

**Unions.** Cases that hold data are types of their own, and `A | B` (usually named,
`type Channel = TextChannel | Thread`) takes a resource of any of them. A field of a union type
is a reference, as a field of one resource type is; a `Collection<Channel>` owns items of
each type. `match` takes a union apart by type and names every case:

```
fn label(c: Channel) -> Text {
    match c {
        t: TextChannel => "# {t.name}",
        x: Thread => x.title,
    }
}
```

`Option<T>` is the built-in `.none | .some(value)` and `Result<T, E>` the built-in
`.ok(value) | .err(error)`. A `T` is accepted where an `Option<T>` is expected. Enums, options,
results and resources compare with `==`; options and results are taken apart with `match`
(`.some(x) => ...`), `if let`, `?.` and `??`.

**Type names and roots.** `type Name = T` names any type. A name for a collection, set or map is
a resource of its own that code uses as that container (`notes.insert(n)`, `for n in notes`,
`std.list(notes, notes)`); its one field is `Name.items` in the ids lock, so renaming an old
`type Notebook { notes: Collection<Note> }` to it (`tilolang ids --rename`) keeps its records. A
root names one such resource per room: `root notes: Notes` is `notes` in the package and
`alias.notes` in a package that uses it.

**`if` and blocks.** There is one `if`: `if cond { ... } else { ... }`. Used as a value its branches
agree on a type and the value is each block's last expression; as a statement it needs no
`else`. Any block is an expression (`let n = { let a = 2; a * 2 }`), and a body that ends with an
expression returns it:

```
fn title_of(list: TodoList) -> Text {
    if list.name == "" { "Todo" } else { list.name }
}
```

**Options without `match`.** `if let` binds an Option's value, several at once, each seeing
the names before it; `x?.f` reads a field (or calls a method) of an Option's value, giving an
Option; `x ?? y` is the value or the fallback. They mean the same in actions, fns and views:

```
fn last_said(c: Conversation) -> Text {
    c.messages.last()?.text ?? "No messages yet"
}

view board(b: Board) {
    if let todos = b.todos, nb = b.notebook {
        button(morning_cleanup(todos, nb))
    }
}
```

A template's `Option<View>` parameter placed as a statement shows what it was given, and nothing
when it was left out.

**Key paths.** Where a lambda would only read a field, `.field` (or `.a.b`) is that lambda:
`items.sort_by(.due)`, `items.count(.done)`, `items.remove_where(.done)`. `sort_by` takes
Int, Float, Text, Bool, Date or Time keys, and `desc` for the largest or latest first:
`items.sort_by(.due, desc)`.

**Reflection.** The log records who made every change, when and by which action, so code reads
it rather than storing its own timestamps: `created(r)` and `updated(r)` are an
`Option<host.Version>` (`.who`, `.at`, `.what`, the action's label), `history(r)` all of them,
oldest first. `v.is(edit)` asks whether the action `edit` made it, by the action itself, never
its label: `history(m).any(v -> v.is(edit))`. An offered `host.Action` answers the same:
`actions(m).any(a -> a.is(edit))`. A version also says which task run made it (`.task`, a
`host.TaskRun`), what it did (`.kind`: `created`, `changed` or `removed`; a removed resource
keeps its versions) and which `.fields` it changed. `changes(of: run)` is every version a run
made, of every resource, and `unseen(r)` the versions of `r` an agent's run made that the
person looking has not seen yet. The UI host marks those where views show them, with no code
of the app's; `ui.ChangeMark(r)` places the mark where the default spot is wrong
(task-legibility.md).

**Members.** Who is in the workspace is the platform's membership, read and never copied:
`workspace.members` is everyone in it, people and agents, a `List<Principal>`, and `members(r)`
those of them the workspace's grants let read `r`. A principal says what it is: `p.name()`,
`p.kind()` (`.person` or `.agent`, a `PrincipalKind`) and `p.role()` (`Option<Text>`, the
workspace role, `owner` or `editor`, of a member who holds one); `avatar(p)` shows it. A view
that reads them runs again when membership or grants change. Every `rich_input`'s `@` drawer
offers who spoke last in the resource its view shows, then `members(r)` of it (the workspace's
members in a view of no resource), without the view declaring anything. A primitive the
toolkit draws takes that component's events in its block, as `ui.RichInput` written out does:
`rich_input(bind: draft) { on submit: send(c, draft) }` sends on Enter (Shift+Enter breaks the
line).

**Collections.** An item is its id: a reference to it names its document and id
(`@board.Card:root-x/i3`), wherever the item is. `items.remove(x)` removes an item with
everything in it; `items.remove_where(pred)` removes what the predicate holds of and gives how
many went. A move names a neighbour, `items.move(x, after: y)` or `before: y`, or a place,
`items.move(x, to: i)` (what a drag gives): the place is resolved, when the action runs, to the
item it then comes before, and the move is recorded beside it, so concurrent moves converge
where each person meant. The collection a move is called on is where the item goes, from
whichever collection of the document holds it: `done.cards.move(card, to: done.cards.len())`
takes a card from its column to the foot of `done`, the same card, with its history and every
reference to it. An item cannot go inside itself. A place may count only a run of the
collection, the items a board's lane shows: `items.move(x, to: i, among: items.filter(t ->
t.section == s))` puts `x` at the place `i` among that run's other items, beside the one it
then comes before, or after the run's last. A board lane's drop says both what was dropped and
where: `on drop(moved: Task, at: Int): place(moved, s, at)`. How moves, edits and removals made
at the same time merge is convergence.md, "Storage and sync".

**Flows** invoke actions by calling them. `try act(args)` gives a `Result<T, Text>` (`Result<(),
Text>` for an action without a result) instead of failing:

```
flow cleanup(list: todo.TodoList) -> Text {
    for item in list.items {
        if item.done {
            todo.remove(list, item)
        }
    }
    match try todo.rename(list, "Done") {
        .ok => "renamed",
        .err(why) => why,
    }
}
```

**Parameter defaults.** An action parameter may have a default written in terms of the
parameters before it. An invocation that leaves the argument out gets it, computed when it is
invoked on what the invoker sees, and records it as the argument; an explicit argument wins:

```
action check(item: Todo, done: Bool = not item.done) (label: if done { "Check" } else { "Uncheck" }, on: item.done) {
    item.done = done
}
```

A register written from itself (`item.done = not item.done`, `c.n = c.n + 1`) is an error
(E039): it is computed again wherever the change is applied. Take the outcome as a parameter
with a default, as above, or make the field a counter for a delta.

**Views.** A view body is UI statements: primitive and view calls (with child blocks), `for`,
`if`/`else if`/`else`, `match` (each arm one UI call or a block) and `let`. Options of UI
primitives are the primitives' own enums, written as dot literals; lengths keep their units and
flags stay bare words. An anchor follows the call as `as name`:

```
view notebook(nb: Notebook) {
    window order: NoteOrder = .created     // shared by everyone looking at this window
    lens expanded: Option<Note> = .none    // one per person per window
    page {
        stack(gap: .section) {
            row(distribute: .between) {
                text("Notes", role: .title)
                button(new_note(nb)) as new
            }
            button("Sort by title", on: set(order, .title))
            for n in listed(nb, order) {
                button(n.title, on: open(n))
                text(n.body, role: .caption, truncate, min: 11rem)
            }
        }
    }
}
```

The options and their values: `gap:`/`padding:` `.tight .related .sibling .section .group`;
`align:` `.start .center .end .stretch`; `distribute:` `.start .center .end .between`; `collapse:`
`.stack .drawer .navigate`; `side:` `.leading .trailing`; `at:` the nine positions `.top_leading`
... `.bottom_trailing`; `axis:` `.vertical .horizontal`; text `role:` `.body .title .caption
.numeric`. A button takes its look from its action, `primary action` or `destructive action`, and
its label and icon from the action too unless it names them (`button(n.title, on: open(n))`).
`space.width_class` (`.compact .medium .expanded .wide`) and `space.orientation` (`.portrait
.landscape .ultrawide`) compare with the same literals.

**View state** (workspace-model.md, "In the language") is declared at the top of a view:
`window name: T = default` is the window's and shared, `lens name: T = default` is each
person's own in that window. The view reads them like variables; they change only through a
control bound to them (`input(bind: query)`, `checkbox("Open", bind: open)`) or the platform
action `set(name, value)` on a control. `open(r)` and `open(r, view: v)` are platform actions
too: they go to the window of `r`. An action's arguments are evaluated when it is invoked, on
what the invoker sees, as its parameter defaults are: an action a view applies reads the view
state its arguments use when the control fires (`send(c, draft)`, `set(open, not open)`,
`send(c, "{draft}!")`), so a press right after a keystroke sends that keystroke and a toggle
pressed twice before the view draws again toggles twice.

**Outside origins** (convergence.md, "Origins" and "Outside services"). A type holds the
resources of an outside service when it names one of the service's kinds:

```
type Message (origin: imap.message) {
    subject: Text (title)
    from: Text
    unread: Bool
    mailbox: Text
    note: Text            // the app's own field: the service never touches it
}
```

The system keeps such resources in step with the service (getting, searching, and telling it
what people changed), with credentials the package never sees. The fields the type declares of
the kind take the kind's types (E112); the type may add fields of its own. Only the service
writes the kind's other fields: code writing one is E113, except in a scenario. People change the
fields the kind marks as theirs (a message's `unread`, `flagged` and `mailbox`): the change
applies at once and becomes final when the service accepts it. The kinds are in
`src/origin.rs`: `imap.account`, `imap.mailbox`, `imap.thread`, `imap.message`.
`outside(capability, payload)` takes one of the platform's capabilities (`post`, `smtp.send`),
checked by the compiler.

**Search** (search.md, "What a type declares"). A type may end with a `search` block: its filters in
order (the first are the visible chips), its default order, and fields kept out of filters.
Without one, every Bool, enum, Date, Time, number, reference, identifier and Set field is a
filter in field order, and the first Time the default order, newest first.

```
type Message {
    subject: Text (title)
    from: std.Identifier
    received: Time
    unread: Bool = true
    attachments: Collection<File>

    search {
        filter unread "Unread"                          // a field; its label
        filter from
        filter "Has attachment" = not attachments.is_empty()   // a named filter
        sort received newest                            // newest, oldest, a_z, z_a
        hide subject
    }
}
```

A named filter reads only the type's own fields and literals (`not done`, `due != .none`,
`effort > 2`, `tags.len() > 0`), so the index keeps it as fields change. A `std.Identifier`
is an account of any service held as its URI: `std.Identifier("mailto:jane@x.com")`, a bare
`jane@x.com` or `+4930…` taken as `mailto:` or `tel:`; `.scheme` and `.value` read its parts.

`query(T, in: x, filter: value, sort: .field_order)` lists the resources of `T` inside `x`
that the filters hold of, a lazy list the index answers. Its arguments are `T`'s filters,
checked against what each compares: a Bool; a case, or several (`[.open, .blocked]`); a Date
or Time, `today`, `now`, either plus or minus a duration, a duration from now (`-7d`), or a
range of them (`today..today + 7d`, `2026-10-01..`); a number or a range; a resource; an
identifier or a pattern (`"*@x.com"`); `none` or `any` where a filter may hold no value. Without
`sort:`, the type's own order. Outside a view (an action, a flow) a query may leave out `in:`
and look across the workspace. `items.matching("is:done due:-7d")` asks the same of a list,
written as people type it in search, and `filters(T)` gives a type's filters as places and
chips show them (`host.Filter`).

**Programs** (agents.md, "Querying" and "Reading and describing"). An agent's program (the
door's `tilo` tool, `tilolang repl`) has words for reading what the workspace holds, across
apps:

```
grep(/\bChen\b/, in: notes + chat).count_by(.doc)      // counts first, then a row per note
grep(/^- \[ \] (.*launch.*)/i, in: notes, not: .quote, changed: -14d)
    .each(t -> todo.add(todo.tasks, t.group(1)))
find("standup chen")                                   // the palette's words, every app
find(Note, pinned: true).sort_by(.title).take(3)       // a type and its filters
find("groceries").single()                             // one, or an error listing them
read(n3, matching: /lemon/, around: 1)                 // size first, anchors, paged
describe(notes)   describe(Note)   describe(notes.pin)   describe(Note.title)   describe(n3)
print(xs, fields: [.title, .due])
```

- `/.../flags` is a regular expression, a `Text` (`i`, `m`, `s`, `x` written inline). A `/`
  after a value still divides.
- `grep` gives `host.Match`es: `.doc` (the resource whose text holds it), `.anchor` (`¶4`,
  `¶2.1`, or a field's name), `.text`, `.line`, `.groups` and `.group(n)`, `.kind` (`paragraph`,
  `heading`, `quote`, `list`, `code`, `field`), `.written_by` and `.changed`. It searches the
  apps `in:` names (all it may read without one) or the resources it names, and says first
  how many matched in how many resources and what it could not cover. A plain text is read as
  markdown writes it: `> ` a quote, `# ` a heading, `- ` a list item.
- `count_by(key)` and `group(key)` give rows with `.key` and `.count` or `.items`, the largest
  first; `take(n)`, `each(f)` and `single()` chain on any list.
- `me` is the person the task acts for.
- An app the program's host does not hold is read through that app's host: its resources are
  `host.Resource`s, which print, count, group, `read` and `describe`, and `.of(T)` narrows to a
  type the host holds.

**Native vocabulary.** Some smaller pieces of syntax:

- An enum case takes an icon: `all (icon: .file_text)`.
- An action's `label:` and `icon:` may be an `if` over its subject, and `on:` is a Bool over
  the subject. The `else` branch is the static fallback:
  `action pin(n: Note) (label: if n.pinned { "Unpin" } else { "Pin" }, on: n.pinned)`. A label
  may interpolate its arguments, `label: "Move to {col.title}"`: over the subject it follows
  the subject as an `if` does; over another argument it is read where the action is applied
  with it, `button(move_to(b, card, col))`. `button(pin(n))` reads its `label:`, `icon:` and
  `on:` with the arguments it applies, as the view draws: Pin turns into Unpin, toggled.
- A slot that holds a component (`ui.BoardLane` holds `ui.BoardCard`s) takes a view of the
  package whose body is that component: views are components.
- `require <condition> "<reason>"` lines head an action's body: when the action is available
  on its subject. Where it would be offered on the subject (`actions(r)`: `std`'s headers and
  menus, agents' choices) it is not offered while one does not hold; invoked anyway, it is
  refused with the reason before anything changes. A condition reads only the subject and is
  pure, as the presentation is (it is checked in the action's presentation view):
  `primary action send(d: Draft) { require not d.sent "It went already" ... }`.
- `2026-10-24` is a Date and `2026-10-24T09:30Z` a Time (UTC), held as ISO text that compares
  with the ordering operators; `2026-10` is a month, a filter's value. `Date("2026-10-24")` and
  `Time(...)` read the same from text. `date(bind: t.due)` edits one. In an action, `now` is the
  invocation's Time, recorded with the change.
- A scenario names people with `let mia = person("Mia")`. Inside
  `as mia at 2026-06-29T07:02Z { post(t, "Hi") }`, the actions called run with `invoker` = mia
  and `now` = that time, and the block is a change of its own, so history and reflection show
  it as mia's at that time.
- `root T` (or `root alias.T`) is the workspace's resource of a root type, anywhere code may
  read it; until the host makes it, it reads as it would start.
- An item block names a template's other slots beside the item's view; they do not see the
  item: `std.transcript(c, c.messages) { m -> message(m, replying) composer: { ... } }`.
- `composer { ... }` is the entry bar of a frame and stands directly in `page`, `document` or
  `canvas`.
- `xs.first()` and `xs.last()` give an `Option` of the item.
- A row view may have `let`s before its `row`. `row(r)` with a resource shows that resource's
  row. `card(r, count: n)` shows a count.
- A card is a summary of its resource: a `list` inside a `card` shows its first five rows (the
  list node's `preview` property), the count says how many there are, and the card opens the
  resource for the whole list.

## Commands

`tilolang` is the language's tool (`crates/lang/tilolang-cli`, `cargo run -p tilolang-cli --`).
The compiler crate has the commands that need nothing but the compiler under two names,
`tilolangc` and the short `tlc` (`cargo run -p tilolang --bin tlc --`).

```
tilolang check [dir] [--json] [--watch] [--write-ids]
                                          every error of the package and its imports, and an
                                          ids lock that is out of date (written with --write-ids)
tilolang build [dir] [--out DIR]          writes a signed Tilo app per package: OUT/<name>.tapp
tilolang fmt [dir] [--check]              rewrites every file in canonical form
tilolang ids [dir] [--rename old new]     prints or edits the ids lock
tilolang run [dir] [--peers N] [--headless]  runs the package as an app in a nested Tilo session
tilolang describe [dir] [--search DIR]    prints what an app publishes as tilolang: a native app
                                          (its surface.toml) and a tilolang package alike
```

`dir` defaults to the current directory. Errors print as `path:line:col: E0xx message`;
`--json` prints them as one JSON array, each with its `file`. `check --watch` checks again
whenever a `.tl` file of the package or its imports changes. Exit status: 0 fine, 1 errors in
the sources, 2 anything else (a missing directory, a bad option).

`fmt` prints the one canonical text projection. It keeps `///` doc comments; plain `//`
comments are not part of the canonical form, so `fmt` leaves a file that has any alone and
names it, instead of deleting what someone wrote.

## Packages are apps

Every package is a Tilo app of its own. `tilolang build` compiles it and the packages it uses
and writes each as a signed `.tapp`:

- the manifest, generated from the `app` declaration: the package id is the app id, its name
  and `category:` are the app's, its `icon:` (a platform glyph, `.file_text`) is drawn into the
  payload's `icon.svg` and signed as `[package] icon`;
- the entrypoint: `[entrypoint] runtime = "tilolang/<version>"`, `bin = "bin/tilolang-app"`. The
  launcher runs the tilolang runtime bundle's binary (`apps/tilolang`) and points it at the
  package's payload with `TILO_APP_MODULE`;
- the payload: `app.wasm`, `app.ids`, the compiled modules of the packages it uses (`imports/`), the
  `surface.toml` generated from the metadata (declaring the kinds it reads from the packages it
  uses as `reads`), and the icon.

The runtime compiles nothing: it loads the one module it was started for, under the id the
package was signed as. Each app has its own window, record, custody and permissions. It reads
another app's records only as its surface declares (`reads`), read-only; it changes another
app's resources only by invoking that app's actions.

## The development loop

```
dist/dev/nested-session-build.sh target/nested-session   # once; TILO_NESTED_APPS="" builds no other app
cargo run -p tilolang-cli -- run apps/tl/counter --peers 2
```

`run` builds the package into `.tapp`s for a locally built nested Tilo session
(`dist/dev/nested-session.sh`, or `dist/dev/nested-pair.sh --peers N` for N people in one
community workspace), installs them there and opens the app through the session's own action,
so the launcher starts it as on a real system. The session bakes the tilolang runtime bundle from
the build. Then:

- **Edit and save.** When any `.tl` file of the package or of an import changes (inotify), the
  packages are built again, reinstalled with `tilo-install --force`, and the app's window is
  closed and opened again on the new build. No cargo build runs. A build with errors keeps the
  last good one installed; the terminal lists the errors.
- **Collaborate.** `--peers N` runs N sessions, each a different throwaway person, joined to one
  workspace.

New declarations get their ids when they are first compiled, and `build` writes them to the
package's `app.ids` straight away, so every build agrees on them.
