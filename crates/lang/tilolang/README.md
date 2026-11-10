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

A package that reaches services outside Tilo says which hosts its requests may reach, in the
same header: `app so.tilo.feed "Feed" (web: ["example.com", "*.rss.org"])`. `*.rss.org` covers
its subdomains, not `rss.org` itself. A request to any other host is refused as a value; a
package that declares no hosts reaches none. The grant is part of the manifest an install shows,
and the workspace may revoke a host.

A package migrates its own records when its schema changes shape. The field the type no longer
uses stays as an ordinary field: its values stay in the records, and a migration reads them. A
`migration action` on the type moves what a field held to where the type keeps it now:

```
type Column {
    title: Text
    cards: Collection<Card>
}

type Board {
    columns: Collection<Column>
    cards: Collection<Card>
}

/// Puts each card of a board from before into its column, in the order the board showed them.
migration action place_cards(b: Board) {
    for card in b.cards {
        let col = card.column
        col.cards.move(card, to: col.cards.len())
    }
}
```

The platform runs it once on each resource of its type, as an ordinary action of the person
whose window opened the data: it is in history, undone like any other, and records in the same
change that it ran. A migration is never offered. It cannot delete anything; moving items
between collections keeps their ids and history.

An action is a thing somebody does, so every action is offered: people choose it from the
header, an item's menu or the palette, agents choose among them, and programs call it. An app
places its actions where its views show them — a drag's `place`, an entry's create, a key a
control runs — and the same actions stay reachable everywhere a command is listed. A helper the
app needs but nobody chooses is a `fn`, which computes from what it is given; an action that
writes gets a name that says what it does, because a person can choose it too.

Where a surface draws an offered action is not a property of the action's role, and an action
does not say where it stands: the platform reads that off what the action is. A command that
toggles a state (`on:`) stands in the header row as the control that shows whether it is on; the
primary action is the header's Primary button; every other offered action sits in the header's
menu beside the same commands an item's menu lists. A view that places the actions of what it
shows (`actions: [check(primary: true), …]`) says their order and which one leads.

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
opens through the type's default view (`default view counter for Counter`). A type whose window
switches between views names them (`views project_list, project_board for Project`), its default
view first; a view it does not name is a helper its own views place.

`use other.app as other` imports another package. Imports are found among the package's sibling
directories (for `apps/tl/today`, the other directories of `apps/tl`); `--path <dir>` adds a
package directory, or a directory of packages, to look in. Another package's `fn` is called
through the alias, `todo.finished(list, t)`, and runs as that package's own code: a `fn` changes
nothing, so a call is as free as reading that package's resources, and the rule lives where it is
written. Another package's actions are applied or invoked, never called.

A package may also `use` a native app: `use so.tilo.chat as chat` reads what Chat publishes, the
resources, records and enums of its committed `surface.toml`, as types (`chat.Workspace`,
`chat.Conversation`, `chat.ChannelKind`) and its verbs as actions, and checks field reads and action
signatures against them. The language's tool (`tilolang check`) finds native apps among the
`--path` directories (`--path apps/chat`, or `--path apps`), and an agent's task is handed the
same catalogue by the session it runs in: every installed app's surface for its workspace, read
by one loader. Invoking a native app's verb, or setting a field of its record, is the app's own
action: it goes to the app through the session's broker, which decides it as it decides the
agent's own call and starts the app when it is not running, and the step sits in the task's
history with every other. Reading a native record goes the same way: `find(chat.Conversation)` lists
the resources under the app's record as its directory does, and reading one, or a field of one,
asks the app for it — the resource's own `read` view where its directory offers one, else the
record's with the resource as its item, which is the read a window and `tilo_read` take too — as
the task's caller, starting the app when it is not running. One app code over one record answers a
person's window, an agent's `tilo_read` and a program, and a field reads as the type the app's
surface declares it (`Option<Text>` as the text it holds or `.none`, a case of its enum, a set as
its members). What the app keeps in the layout of its own model is never read here: no byte of a
native record is.

## Syntax reference

The full grammar is [grammar.md](grammar.md). In short:

**Declarations.** A package is `type`s, `enum`s, `action`s, `view`s and `fn`s, plus two
package-level declarations (workspace-model.md, "Apps"):

```
type Note { title: Text, pinned: Bool = false, tags: Set<Text> }

enum NoteOrder {
    created
    title
}

type Notebook {                        // a resource holding a collection
    notes: Collection<Note>
}
type Channel = TextChannel | Thread    // a resource of either type

root notebook: Notebook                // one per workspace; the launcher opens it
default view notebook for Notebook     // what a window of the root shows
default view note_editor for Note      // what a window of a Note shows
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
`.ok(value) | .err(error)`. A `T` is accepted where an `Option<T>` is expected: the value it
holds. One level only, so `label: title` is `label: .some(title)`, `.some(x)` written is one
`some` and never two, and an Option of an Option — which nothing but a declaration usually
needs — says which of its levels it means (`.some(.none)`). Enums, options,
results and resources compare with `==`; options and results are taken apart with `match`
(`.some(x) => ...`), `if let`, `?.` and `??`.

**Type names and roots.** `type Name = T` names any type. A name for a collection, set or map is
a resource of its own that code uses as that container (`notes.insert(n)`, `for n in notes`,
`notes.matching(query)`); its one field is `Name.items` in the ids lock, so renaming an old
`type Notebook { notes: Collection<Note> }` to it (`tilolang ids --rename`) keeps its records. A
root names one such resource per room: `root notes: Notes` is `notes` in the package and
`alias.notes` in a package that uses it.

**A package's own name wins.** The language provides types of its own — `Int`, `Float`, `Bool`,
`Text`, `RichText`, `Bytes`, `Principal`, `Date`, `Time`, `Duration`, `File`, `Response`,
`Action` and `View` — and a package that declares a type, enum or alias with one of those names
takes the name for its own: in that package, everywhere and every time it is written, the bare
name is the package's declaration, and what the language provides of that name is written
`std.`'s (`std.File`, `std.Text`, `std.Date`). A `type File { title: Text }` beside a
`picture: Option<File>` is a `File` of the package's own, and `file.choose`'s answer is a
`std.File`; the declaration shadows the language's type throughout the package, so a language
type a later version adds can never break a package that already declares that name. What a
package cannot take is a name that writes a type rather than naming one — `List`, `Set`, `Map`,
`Collection`, `Option`, `Result`, `Filter`, `Sort` — because `List<T>` would still mean what the
language's means; declaring one is refused where it is written (`type List` is an error).

**`if` and blocks.** There is one `if`: `if cond { ... } else { ... }`. Used as a value its branches
agree on a type and the value is each block's last expression; with no `else` it is the Option of
that value, `.none` where the condition does not hold, which is what `else { .none }` gives it.
A branch that is an Option itself is that Option, so two conditions in a row need no nesting. As
a statement it needs no `else`. Any block is an expression (`let n = { let a = 2; a * 2 }`), and
a body that ends with an expression returns it:

```
fn title_of(list: TodoList) -> Text {
    if list.name == "" { "Todo" } else { list.name }
}

fn finished_at(t: Task) -> Option<Time> {
    if let v = changed(t, .done) { if t.done { v.at } }
}

fn title_or_none(list: TodoList) -> Option<Text> {
    if list.name != "" { list.name }
}
```

**Options without `match`.** `if let` binds an Option's value, several at once, each seeing
the names before it; `x?.f` reads a field (or calls a method) of an Option's value, giving an
Option; `x ?? y` is the value or the fallback. They mean the same in actions, fns and views, and
an `if` or `if let` without an `else` is the Option of what its branch gives:

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

**Text and numbers.** A text and a number are asked what they are and what to become as methods, on the value itself, in one surface and with no namespace or global beside it:

```
let parts = t.trim().split(",")           // List<Text>
let shown = parts.join(", ")              // Text
let n = " 42 ".to_int() ?? 0              // Option<Int>
let half = n.to_float() / 2.0             // Float
let price = total.round(2)                // Float, two places
let sum = items.map(i -> i.price).sum()
```

`Text` has `len` and `is_empty`; `slice(from, to)`, a range of graphemes clamped to the text; `find(what)`, an `Option<Int>`; `starts_with`, `ends_with`, `contains` and `matches(pattern)`, where `*` stands for any run (the same pattern search's identifiers use); `trim`, `lower` and `upper`; `split(sep)`, which with an empty separator gives what a reader sees as one thing, and `replace(from, to)`, which replaces every occurrence; and `to_int`, an `Option<Int>` taking a decimal number written with an optional sign, around any outer space. A text is counted in what a reader sees: `len`, `find` and `slice` count graphemes, so a family emoji, a flag or a letter with an accent is one and a cut never splits one, and a text column's `len` in a filter counts the same way. Case changes are Unicode case mapping and not locale tailoring (`upper("straße")` is `STRASSE`).

`Int` and `Float` have `min`, `max`, `clamp(lo, hi)` and `abs`; a `Float`'s `round`, `floor` and `ceil` give an `Int`, or, given the places to keep (`f.round(2)`), a `Float`. `Int.to_float()` and `Float.to_int()` convert between them (dropping the fraction), and `.to_text()` writes any number as `Bool.to_text()` writes `true` or `false`. A `List<Text>` has `join(sep)`; a `List<Int>` or `List<Float>` has `sum()` (the items' type) and `avg()` (a `Float`, 0.0 for an empty list); `contains` asks whether an item is one of a list, set or collection, as `index_of` finds it. All of them are pure — no clock, no workspace, no effect — so a view, an `fn` and an action write them alike.

**Key paths.** Where a lambda would only read a field, `.field` (or `.a.b`) is that lambda:
`items.sort_by(.due)`, `items.count(.done)`, `items.remove_where(.done)`. `sort_by` takes
Int, Float, Text, Bool, Date or Time keys, and `desc` for the largest or latest first:
`items.sort_by(.due, desc)`.

**Lambdas.** A parameter of a function type takes the values the function is handed: `x -> ...`
for one, `(a, b) -> ...` for several, as `std.outline`'s place is told what moved, where to and
before which of its siblings: `(moved, parent, before) -> move_to(tasks, moved, parent, before)`.

**Reflection.** The log records who made every change, when and by which action, so code reads
it rather than storing its own timestamps: `created(r)` and `updated(r)` are an
`Option<host.Version>` (`.who`, `.at`, `.what`, the action's label), `history(r)` all of them,
oldest first, and `changed(r, .field)` the one that last changed that field, or the one that
made `r` when none after it did. A store with an index keeps the last change of each field, so
a list groups or sorts by it at the cost of one value a row, never of a row's history:
`group: t -> changed(t, .done)?.at.day()`. `v.is(edit)` asks whether the action `edit` made it, by the action itself, never
its label: `history(m).any(v -> v.is(edit))`. An offered `host.Action` answers the same:
`actions(m).any(a -> a.is(edit))`. A version also says which run made it (`.run`, a
`host.Run`), what it did (`.kind`: `created`, `changed` or `removed`; a removed resource
keeps its versions) and which `.fields` it changed. A resource can be made from another one:
an invocation carries the resource it is made from when the host or the invoker names one, the
change records it, and the resource it created says so, `created(r)?.from` — an
`Option<host.Resource>`. `changes(of: run)` is every version a run
made, of every resource, and `unseen(r)` the versions of `r` the person looking has not seen
yet: another person's or an agent's, since their seen mark of `r` (a comment about
`r` counts as a change to it). The UI host marks those where views show them, with no code
of the app's; `ui.ChangeMark(r)` places the mark where the default spot is wrong
(task-legibility.md).

**What links to a resource.** `linked(r)` is every resource that links to `r`, a
`List<host.Link>`: `.of` is the resource and `.how` how it links, a `host.LinkHow`:
`.created_from` for one made from `r`, `.field` for one whose reference field points at it, and
`.mention` for one whose rich text names it. It is read from the index, at the cost of what links to `r` and not of the
workspace's size, and a view that reads it runs again when what links to `r` changes.

What a type's records are to a resource is the page's to show: a comment is about what it
was made from, and the thread under that resource draws every comment about it, so a page that
draws a thread says what it draws itself (`std.linked(t, without: discussion.said_about(t))`).

**Members.** Who is in the workspace is the platform's membership, read and never copied:
`workspace.members` is everyone in it, people and agents, a `List<Principal>`, and `members(r)`
those of them the workspace's grants let read `r`. `workspace.me` is the person the view is
shown to, the runtime's viewer read where the view runs (an action acts for the `invoker`
instead). A principal says what it is: `p.name()`,
`p.kind()` (`.person` or `.agent`, a `PrincipalKind`) and `p.role()` (`Option<Text>`, the
workspace role, `owner` or `editor`, of a member who holds one); `avatar(p)` shows it. A view
that reads them runs again when membership or grants change. Every `rich_input`'s `@` drawer
offers who spoke last in the resource its view shows, then `members(r)` of it (the workspace's
members in a view of no resource), without the view declaring anything. A component primitive's
block is its component's, events included: `rich_input(bind: draft) { on submit: send(c, draft) }`
sends on Enter (Shift+Enter breaks the line).

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
collection, any part of it in its own order, such as the items one filter shows:
`items.move(x, to: i, among: items.filter(t -> t.section == s))` puts `x` at the place `i`
among that run's other items, beside the one it then comes before, or after the run's last.
Where an item stands among them is `items.index_of(x)`, an `Option<Int>`: none when it is not
one, and the item just before it is `items.previous(x)`, an `Option<T>`: the row over the one a
list is drawing, which the host knows without reading the items before it. A drop that names a
place says both what was dropped and where: `on drop(moved: Task, at: Int): place(moved, s, at)`.
How moves, edits and removals made at the same time merge is convergence.md, "Storage and sync".

**Counts of what is not all read.** A window answers rows from the index it opens over, which
may still be reading the workspace: `items.len()` and `items.count(pred)` are then the part read
so far. `items.counted()` is that count once the store has read all of the workspace, and
`none` before it has, so a view shows a badge or a total as not yet known rather than as a
number that grows under the person reading it:

```
let total = tasks.counted()
if let n = total { "{pending.count(x -> x.done)}/{n}" } else { .none }
```

**Actions that call actions** invoke them by calling them, their own package's or another
one's: each call runs at its door as a change of its own, and may wait for a person's approval.
What the caller writes before a call is a change of its own too, committed before the call runs,
so a call on a record the caller made in the same action sees it; what it writes after the call
is its next change.
`try act(args)` gives a `Result<T, Text>` (`Result<(), Text>` for an action without a result)
instead of failing:

```
action cleanup(list: todo.TodoList) -> Text {
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

**Triggers.** An action that runs by itself is a `trigger`: the package says when it runs, and
the host invokes it as the app's own action (DECISIONS.md, "Triggers"):

```
trigger recount(b: Board) every 30m: recount_columns(b)
trigger due(r: Reminder) at r.at: ring(r)
```

A trigger runs for each of the package's records of the type it names. `every` takes a length of
time and runs at the start of each period on the clock; `at` reads a `Time` or `Option<Time>`
field of the record and runs once when that instant has come, and again if the field moves to
another time. Its action takes the record (`ring(r)`) or a root (`root Board`) and nothing else.
The workspace's origin runs them: once per period or instant across the workspace's devices, once
for a period or time that passed while every device was off, and never by polling.

**Effects.** An action whose body reads what a replay cannot compute again says so:
`uses time` after its parameters, comma-separated when it reads more (`uses web, random`).
Reading the clock or the person's zone anywhere in the body — `now`, `today`, a time of no
zone, `t.day()`, `t.time_of_day()`, `Time.parse(...)`, `r.next_at(...)` — is an error until
it does (E121), and so is a `fn` or an action it calls that reads them. An
invocation records what it read in its journal: the moment and the zone once per invocation,
and each draw and fetch with its answer as the language grows into them. The journal is kept beside the change an action makes, so history
says what a change was made of; a run that waits for a person and goes on reads the moment it
started, not the moment it went on.

A view reads the person's zone and the day it is as what it is drawn with, not as an effect: it
has no `now` (E049) and nothing it does is replayed, while the day a filter reads is the query's
and the window draws again when the day turns. A scenario is seed data, run where a developer
runs it and never replayed, so it reads every effect without saying so.

**Requests.** An action that calls a service outside Tilo declares `uses web` and uses
`web.get(url, headers:, if_none_match:)` or `web.post(url, body:, headers:)`. Both answer a
`Response`:

```
action refresh(feed: Feed) uses web {
    let response = web.get(feed.url, if_none_match: feed.etag)
    if response.ok {
        feed.body = response.text()
        feed.etag = response.headers.find(h -> h.starts_with("ETag: ")) ?? feed.etag
    }
}
```

`ok` is `200..=299`, `status` and `status_text` what the service answered, `headers` a list of
`Name: value`, `body` the bytes it sent, with `.text()` and `.len()`. `headers:` take the same list
of lines, and `if_none_match:` the tag a service gave, so an unchanged answer comes back as a
`304`.

The request runs once, where the invocation runs: on the person's device, or on the host for a
host-run invocation. Its answer is a `Web` journal entry of that invocation, so a replay, and a
run resumed tomorrow, answer from the record and do not ask again. What can go wrong is a value,
never a trap: a host the package did not declare, a host the workspace revoked, a timeout (30
seconds) and no network each answer `ok == false` with `status == 0` and a `status_text` that
says why, and the invocation carries on. A reply's body may be up to 2 MiB; nothing streams.

**Files.** A `File` is a file whose bytes are in the device's content store: what a person
picked, or what a program was handed. It is a value, not a resource — its type is a field's
and an action's parameter — and what it holds of the content is the address, never the bytes,
so a record, an argument and a history entry carry the file by reference:

```
type Note {
    body: RichText
    picture: Option<File>
}

/// Puts a picture in a note.
action insert_image(n: Note, image: File) (did: "added an image") {
    n.body = RichText(n.body, "\n![]({image.url})")
}

view note_editor(n: Note) {
    ...
    button("Insert image", on: file.choose(insert_image(n), accept: ["image/png"]))
}
```

`f.name`, `f.media`, `f.size` and `f.url` read on it, `url` being where its bytes are
(`tilo-blob://<hash>`), which is what a picture takes (`ui.Image(f.url, alt: f.name)`).
`f.text()` reads a text file's content as a `Result<Text, Text>` — `.ok` the text, `.err` why it
is not read (not text, more than 256 KiB, or the content is not on this device) — which is how
an importer reads a document. Reading it is a read and not an effect: the bytes are what the
address names and never change, so a replay reads the same ones, and a device that does not hold
them says so.

A person chooses a file in their own chooser, on the platform's terms: a control applies
`file.choose(a, accept: [...])`, which asks them for a file of those media types and runs the
action `a` with it as the parameter it still needs. `file.save(a, name: "Notes.md")` runs `a`
and puts what it made — text, or a file — in a file the person names. Both stand on a control,
as `open` does, and neither is offered to an `fn`, an action or an agent: a file is the person's
own. A program that holds bytes invokes the same action with a file it has, so an app's import
path is one action whichever way the file arrived.

`f.files()` reads an archive (a `.zip`) as the files it holds, each kept in the device's content
store under the name it has inside it, as a `Result<List<File>, Text>` — `.err` why it does not
read (not an archive, or one that holds more than 256 files, more than 1 MiB in one file or more
than 16 MiB together) — which is how an importer reads a package: an export of notes, a page and
its pictures. The files are the person's, read within those bounds, and each file's `name` is its
path inside the archive, so an import keeps the nesting the archive had.

A chosen file may be at most 1 MiB; a larger one is refused with its size, before anything is
written. An action never carries the bytes: its invocation carries the address, so undo, a
replay and a sync move the reference and the store moves the content.

**Rich text.** A `RichText` is a document: its paragraphs with their formatting, a heading, a
list or checklist item, a quote, a table, a fenced code block (a formula is one, in its own
language), a divider, a picture. `r.excerpt` is its first line with text, `r.text` all of it,
`r.references` the resources it refers to and `r.people` the people it mentions, and
`r.is_empty()` says it holds nothing. `r.markdown` writes it as Markdown and
`RichText.from_markdown(text)` reads a Markdown text as one — the same document, block for
block: a heading, a list item with its depth, a mark over a run of text:

```
fn note_as_file(n: Note) -> Text {
    "# {n.title}\n\n{n.body.markdown}"
}

action import_markdown(nb: Notebook, file: File) -> Note {
    match file.text() {
        .ok(text) => nb.notes.insert(Note(title: "", body: RichText.from_markdown(text))),
        .err(why) => nb.notes.insert(Note(title: "", body: RichText(why))),
    }
}
```

A picture is the Markdown it is written as, a paragraph of its own holding `![alt](source)`:
`r.markdown` writes the address of its bytes (`![](tilo-blob://…)`), which every device of the
workspace holds, and `RichText.from_markdown` keeps the source a picture was written with, so
the document a person exported is the document read back. A table is kept as its Markdown
source, which is what an editor shows it as a grid of.

**JSON.** Text a service sent reads into the type the code names, and a value writes back as
text, with no effect to declare: the work is in the text and the value, nothing outside them.

```
action refresh(feed: Feed) uses web {
    let response = web.get(feed.url)
    match json.decode<List<Entry>>(response.text()) {
        .ok(entries) => {
            for entry in entries {
                feed.entries.insert(entry)
            }
        }
        .err(why) => { feed.trouble = why }
    }
}
```

`json.decode<T>(text)` reads `Int`, `Float`, `Bool`, `Text` (and what holds text: `Date`, `Time`,
`std.Identifier`), a `List`, an `Option`, an enum and a record, matching a record's fields by
name; a record's field may hold a `Map`, a short list of text keys to values. A field the text leaves out is none when it is an `Option`, or the value the type
declares for it; a required one it leaves out is an error at that field's path. Names the type
has no field for are ignored, so a service that adds one breaks no reader.

A number reads as an `Int` when it is a whole one that fits (`5` and `5.0`), as a `Float`
otherwise; a field declared `Float` reads any number. An enum reads from its case's name.

What can go wrong is a value, never a trap: text that is no JSON, and a value whose shape is not
the type's, answer `.err(why)`, and `why` names the JSON path of the value that did not fit:
`$.entries[2].title is not a Text, as text: 5`. A whole decode fails at the first value that does
not fit, so a message names one path, not each.

A record a decode answers is a record value: it has no id, and no store holds it until it is put
in a collection (`feed.entries.insert(entry)`), which is what a record value is for; its fields
read as the text gave them. A record field that holds a resource (a record, a collection, a set)
cannot be made from text, so text carrying one is an error naming that field's path: read the
list and insert what it holds.

`json.encode(value)` writes the fields a record's type declares, by their names, an enum as its
case's name, a list as an array, a `Map` as an object, an `Option` as its value or `null`, and a
`json.Value` as it was read. Writing a stored resource (a reference) traps: its fields are what to
write, and a resource's JSON is not one document: two resources may point at each other. Read
the text into values, or write the fields it should carry into a record, and encode those.

A service whose shape the code does not know is read as a `json.Value` and taken apart as the
code asks, with `json.parse(text)`:

```
match json.parse(response.text()) {
    .ok(value) => match value {
        .object(fields) => { feed.keys = fields.keys().len() }
        .array(items) => { feed.keys = items.len() }
        .string(text) => { feed.trouble = text }
        .number(number) => { feed.trouble = number.to_text() }
        .bool(flag) => { feed.trouble = if flag { "yes" } else { "no" } }
        .null => { feed.trouble = "nothing" }
    }
    .err(why) => { feed.trouble = why }
}
```

`object` binds a `Map<json.Value>` (`fields.keys()`, `fields.get("name")`), `array` a
`List<json.Value>`, `string` a `Text`, `number` a `Float` (JSON has no integers of its own),
`bool` a `Bool`, and `null` nothing. A match over one names all six kinds, as a match over an
enum names all its cases. Like a `Response`, a `json.Value` belongs to the run that read it, so
it is no field's type (E023): a program keeps what it found by writing it into a record.

The type between `<` and `>` is what makes a decode typed; only `json.decode` takes one.

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
`if`/`else if`/`else`, `match` (each arm one UI call or a block) and `let`, whose type is its
value's; a `let` names the type only where the value does not say enough of it.
Options of UI primitives are the primitives' own enums, written as dot literals; lengths keep
their units and flags stay bare words. An anchor follows the call as `as name`:

```
view notebook(nb: Notebook) {
    state order: NoteOrder = .created      // how my window shows the notes
    state expanded: Option<Note> = .none   // one per person per window
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
... `.bottom_trailing`; `axis:` `.vertical .horizontal`; text `role:` `.display .large_title
.title .heading .subheading .body .label .caption .reading .code`, with `numeric: true` for
figures. A component primitive (`text`, `input`, `checkbox`, `rich_input`, `image`, ...) takes
the arguments, slots, events and `bind:` its `ui.*` component's description declares; the rest
of this section is what the layout primitives take. A button takes its look from its action,
`primary action` or `destructive action`, and its label and icon from the action too unless it
names them (`button(n.title, on: open(n))`).
`space.width_class` (`.compact .medium .expanded .wide`) and `space.orientation` (`.portrait
.landscape .ultrawide`) compare with the same literals.

**Motion** is on by default: a `ui.VirtualList`'s rows arrive, leave and change places, a
checkbox's mark fills, a section or a tree branch unfolds, without a line of code saying so, and
reduced motion lands all of it at once. A view that wants a different pace says `motion:` on the
component that owns the change — `ui.ListRow(title: t.title, checkbox: t.done, motion: .brisk)` —
with `.standard` (the theme's, the default), `.brisk`, `.calm` or `.still`. It changes how long
the design system's named motions take, never their curves, colours or durations. The whole
ladder of how far a view may go — options, composing `ui.*`, drawing, native widgets — is
[components.md](../../../docs/designs/app-language/components.md#4-customizing-a-component-a-ladder).

**View state** (workspace-model.md, "In the language") is declared at the top of a view:
`state name: T = default` is what each person looking at the window keeps in it — how their
window shows the resource, and where they are in it; it is each person's own, so one person's
window setting or draft never moves another's view. The view reads a slot like a
variable; it changes only through a control bound to it (`input(bind: query)`,
`checkbox("Open", bind: open)`) or the platform action `set(name, value)` on a control. `open(r)` and `open(r, view: v)` are platform actions
too: they go to the window of `r`. An action's arguments are evaluated when it is invoked, on
what the invoker sees, as its parameter defaults are: an action a view applies reads the view
state its arguments use when the control fires (`send(c, draft)`, `set(open, not open)`,
`send(c, "{draft}!")`), so a press right after a keystroke sends that keystroke and a toggle
pressed twice before the view draws again toggles twice.

**Outside origins** (convergence.md, "Origins" and "Outside services"). A type holds the
resources of an outside service when it names one of the service's kinds:

```
type Message (origin: imap.message) {
    subject: Text
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
    subject: Text
    from: std.Identifier
    received: Time
    unread: Bool = true
    attachments: List<File>

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
`effort > 2`, `tags.len() > 0`), and the two a filter is asked at and for: `today`/`now` and
`workspace.me`, which is each person it is asked by (`filter "Assigned to me" = assignee ==
workspace.me and not done`). The index keeps it as fields change.

**Values across records.** A value that follows from other records of a collection — a branch
finished when it and everything under it are, a row saying how many of its subtasks are done —
is an ordinary `fn` over the collection, called from the views that show it:

```
type Task {
    title: Text
    done: Bool = false
    parent: Option<Task> = .none     // what the items under it nest by
}

/// Whether `t` is finished: done itself, and so is every task under it.
fn finished(items: List<Task>, t: Task) -> Bool {
    let under = items.descendants(t, .parent)
    if under.is_empty() { t.done } else { under.all(x -> x.done) }
}
```

`items.descendants(t, .parent)` and `items.ancestors(t, .parent)` follow a reference field
through the items the view passes in. Nothing writes the result: each person reads it from what
they hold, and both agree once the changes meet.

A `std.Identifier`
is an account of any service held as its URI: `std.Identifier("mailto:jane@x.com")`, a bare
`jane@x.com` or `+4930…` taken as `mailto:` or `tel:`; `.scheme` and `.value` read its parts.

A repeating item and a reminder are the app's own facts, not the language's: an app that has
them declares a field holding what it stores (`repeat: Text` for an RFC 5545 rule,
`remind: Option<Duration>` for how long before the due moment) and says in words what it holds
through `phrase(...)`, as every other field is said.

`query(T, in: x, filter: value, sort: .field_order)` lists the resources of `T` inside `x`
that the filters hold of, a lazy list the index answers. Its arguments are `T`'s filters,
checked against what each compares: a Bool; a case, or several (`[.open, .blocked]`); a Date
or Time, `today`, `now`, either plus or minus a duration, a duration from now (`-7d`), a month
(`2026-10`), or a range of them (`today..today + 7d`, `2026-10-01..`); a number or a range; a
resource; an identifier or a pattern (`"*@x.com"`); `none` or `any` where a filter may hold no
value. Without `sort:`, the type's own order. Outside a view (an action) a query may
leave out `in:` and look across the workspace. A filter is a value: `Task.today` names the one
the type declares, `Task.project(p)` one with its value, and `items.matching(Task.today)`,
`items.matching(Task.project(p), Task.done)` and `query(Task, in: x, Task.today)` ask them, no
search text written. `filters(T)` lists the type's filters (`host.Filter`), each with the query
choosing it asks, its label, icon and whether it is on or off; the filters that are on or off
are what a list's places are, and a `ui.NavItem` given one, or a `ui.NavList` given the chosen
one (`selected: place`), is keyed by it without the app writing a key.
`items.matching("is:done due:-7d")` still asks the same of a list, written as people type it in
search. A filter's fields also read as text (`f.query` is `is:today`), so a
window's search box and a query's own words stay one language. What a window shows beside its
places is the app's own, drawn by its view: the projects a root holds under a heading that adds
one, or any other list of it, in the window's own `ui.NavList`.

A filter is a predicate over the type's own fields, and nothing more: what a place holds is
what its predicate says (`filter "Today" = due <= today and not done`), how its list reads is
the app's (`ui.ListItem(group:, tail:)` in the row the app builds, each answered per item
where the place decides so itself), and its order is the app's (`sort_by`).

**Agent and REPL sessions** (agents.md, "Querying" and "Reading and describing"). A session's
code — the door's `tilo` tool, `tilolang repl` — has the agent library's words in scope for
reading what the workspace holds, across apps. The library is `tilo.agent` (`agent.rs`), one
like any other: a package that wants the words writes `use tilo.agent as agent` and calls
`agent.grep(...)`, the code a session's `grep` is.

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
- `me` is the person the session acts for (`agent.me` in a package).
- An app the session's host does not hold is read through that app's host: its resources are
  `host.Resource`s, which print, count, group, `read` and `describe`, and `.of(T)` narrows to a
  type the host holds.

**Time.** A `Date` is a calendar day (`2026-10-24`), a `Time` an instant
(`2026-10-24T09:30Z`; `2026-10-24T09:30` is the person's own time of day, read in their zone),
both held as ISO text that compares with the ordering operators. `Date("2026-10-24")` and
`Time("2026-10-24T09:30:00Z")` read one from text, and `ui.DateField(bind: t.due)` edits one.

A `Duration` is a length of time: `30m`, `2h`, `3d`, `1w` (`s`, `m`, `h`, `d`, `w`), or
`Duration(n)` for `n` seconds — a value of its own and not an `Int`. It shows as the largest unit
that divides it (`Duration(7200)` reads `2h`, `Duration(5400)` reads `90m`), `.seconds()` gives the
number back, and two of them compare.
A `Time` moves by any length (`now + 2h`, `at - 30m`) and two are a `Duration` apart
(`end - start`). A `Date` moves by whole days only (`today + 3d`, `today - 1w`), and two are
apart in days: a length that spans part of a day is refused where it is written, and where code
holds it, where it is applied.

What a day and a moment are made of reads on them. On a `Date`: `d.weekday()` is the day of the
week, matched as `.monday` … `.sunday`; `d.day()`, `d.month()` and `d.year()` are its parts;
`d.start_of_week()` is the first day of the week it stands in, the day the person's region
starts one on. On a `Time`: `t.day()` is the `Date` it falls on for the person, in their zone
as `today` reads it, and `t.time_of_day()` the `Duration` since their midnight.

An action reads the moment it is invoked at, `now`, and the day that falls on for the person,
`today`; nothing else does, because a moment is a fact of the invocation, recorded with the
change. Words name a day or a moment when they are read against one: `Date.parse(t, near: at)`
and `Time.parse(t, near: at)` read their text — "tomorrow" is the day, "tomorrow 9am" the moment,
a day's start when the words name no time of day — or `none` for words no phrase reads. A stored
day or moment is a `Date` or a `Time` field; a stored length of time is the number the app
measures it in, read as one with `Duration(n)`.

**Native vocabulary.** Some smaller pieces of syntax:

- An enum case takes an icon: `all (icon: .file_text)`.
- An action's `label:` and `icon:` may be an `if` over its subject, and `on:` is a Bool over
  the subject. The `else` branch is the static fallback:
  `action pin(n: Note) (label: if n.pinned { "Unpin" } else { "Pin" }, on: n.pinned)`. A label
  may interpolate its arguments, `label: "Move to {col.title}"`: over the subject it follows
  the subject as an `if` does; over another argument it is read where the action is applied
  with it, `button(move_to(b, card, col))`. `button(pin(n))` reads its `label:`, `icon:` and
  `on:` with the arguments it applies, as the view draws: Pin turns into Unpin, toggled.
- An action says what it did, as history reads it after who did it: `did: "moved this"` or
  `did: "added a subtask"`. A history row is then one sentence in a person's terms — "Ana
  moved this", "You added a subtask" — rather than a label beside a paragraph about the
  command.
- An action declares its shortcut: `key: "Ctrl+Enter"`, `key: "Ctrl+Shift+M"`, `key: "Delete"` —
  modifiers in any order, then one key. The command modifier is the platform's own, so
  `Ctrl+Enter` is Command+Enter on a Mac. Where the action is offered its key runs it: an
  action on an item while that item has the keyboard, an action on the app's root window-wide.
  The host's bar and menus show it, and so does the palette. The key is checked when the app
  compiles, and two actions of one subject cannot answer the same keys.
- A slot that holds a component (`ui.BoardLane` holds `ui.BoardCard`s) takes a view of the
  package whose body is that component: views are components.
- `require <condition> "<reason>"` lines head an action's body: when the action is available
  on its subject. Where it would be offered on the subject (`actions(r)`: the host's bar and a
  window's menus, agents' choices) it is not offered while one does not hold; invoked anyway, it is
  refused with the reason before anything changes. A condition reads only the subject and is
  pure, as the presentation is (it is checked in the action's presentation view):
  `primary action send(d: Draft) { require not d.sent "It went already" ... }`.
- A scenario names people with `let mia = person("Mia")`, and runs an action as one of them
  with `invoke(post(t, "Hi"), by: mia, at: 2026-06-29T07:02Z)`: inside that call `invoker` is
  mia and `now` is that time, and the call is a change of its own, so history and reflection
  show it as mia's at that time. `from: r` makes it from `r`, as an action invoked on `r` is,
  so what it creates shows what it was made from (`created(r)?.from`, `std.origin`,
  `std.linked`); `by` defaults to `invoker`, `at` to now, `from` to nothing. Only a scenario
  writes `invoke`, and the seed data it makes is what shows a feature in a capture.
- Which place a window stands on is a run's option, not code:
  `layout-check --state place=Task.done` stages the state a view declares
  (`state place: Filter<Task> = ...`) at a value the app's own code names, such as one of its
  filters (`Task.today`) or a case of its enum (`Side.right`). A person's own choice stands
  over it, and what a run stages is never saved as anyone's change.
- `root T` (or `root alias.T`) is the workspace's resource of a root type, anywhere code may
  read it; until the host makes it, it reads as it would start.
- An item block names a template's other slots beside the item's view; they do not see the
  item: `std.transcript(c, c.messages) { m -> message(m, replying) composer: { ... } }`.
- `composer { ... }` is the entry bar of a frame and stands directly in `page`, `document` or
  `canvas`.
- `xs.first()` and `xs.last()` give an `Option` of the item.
- A row view may have `let`s before its `row`. `row(r)` with a resource shows that resource's
  row. `ui.Card(item: r, count: n)` shows a count.
- A record shown where no view of it is drawn — a link, a search result, the palette, a history
  entry, a notification — is drawn through its type's `row view`; a type without one shows its
  type's name and `created(r)`. Where code needs a record's name it is the first `Text` or
  `RichText` field, cut to its first non-blank line, and search indexes every text field.
- A view of one item whose whole body is one row is that item's row wherever it is drawn
  (`view task_row(t: Task, place: RowPlace) { row(t.title, ...) }`), so the parameters after
  the item are what the place around the row says: a list grouped by a day gives the day, a
  project's list its project, and the row repeats neither.
- A card is a summary of its resource: a `list` a `ui.Card` holds shows its first five rows (the
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
tilolang profile [dir] [--seed N] [--then INTERACTION]...
                                          counts what the app costs to open and to use, in the
                                          language's own terms (see "Cost")
```

`dir` defaults to the current directory. Errors print as `path:line:col: E0xx message`;
`--json` prints them as one JSON array, each with its `file`. `check --watch` checks again
whenever a `.tl` file of the package or its imports changes. Exit status: 0 fine, 1 errors in
the sources, 2 anything else (a missing directory, a bad option).

`fmt` prints the one canonical text projection. It keeps `///` doc comments, and plain `//`
comments above the declaration, field, statement or match arm they were written over, inside
expression blocks and match arms as well; one written inside a declaration's head prints above
the next statement. A blank line between two `///` lines is a paragraph break, kept as a `///`
line.

## Cost

What an app costs is measured in the language's own terms, not in milliseconds: the fuel each
call into its wasm spends, the calls it makes, and what it reads from the host — records,
index pages and entries, evaluations — plus the offers it publishes. The runtime counts it
(`tilolang_runtime::cost`, `crates/lang/tilolang-runtime/src/cost.rs`), off until asked, and the
compiler writes each unit's export to the line it is written on (the `tilolang.lines` section of
the module), so a table names a *unit* — a view, a row view, an action, a requirement, an
`applies`, a filter, an at-event argument — by its source line:

```
tilolang profile apps/tl/tasks --seed 10000
      fuel  unit                                  written                         calls    record     index
    442511  Task.task_page view                                                      1         0     10026
     13105  Task.home row                          apps/tl/tasks/app.tl:567         48         0       141
      2340  Task.check~present view                apps/tl/tasks/app.tl:200         52         0        94
      1092  Task.set_priority_high requirement     apps/tl/tasks/app.tl:298         52         0        47
      1092  Task.set_priority_low requirement      apps/tl/tasks/app.tl:286         52         0        47
...
```

A unit no declaration is written for (the view a list's page is rendered as) has no line.

`--seed N` opens a workspace of N items, seeded as `tilolang bench` seeds one; `--scenario`
stages a scenario (the app's first otherwise); each `--then` runs after the app is open and is
counted alone: `key:Tab`, `click:X,Y`, `type:TEXT`, `scroll:DX,DY` (points in logical pixels).
The table is sorted by inclusive fuel, so a unit that calls others carries theirs; a row says
how often the unit ran, so a requirement evaluated for every item shows the count grow with the
workspace rather than with the rows a person can see.

The counts do not depend on the machine or its load, so they gate: a test records the costs of
an interaction (`Runtime::record_costs`, `::reset_costs`, `::costs`) and holds them
(`tilolang_runtime::assert_costs("open", &costs, |c| c.calls("Task.due_today requirement") <= rows)`),
failing with the whole table. Fuel is a function of the code run, in the engine's own units: a
budget in fuel is a budget on the work, the same in debug and release, on any runner.
`tests/budgets.rs` of `tilolang-tilo` holds some to what they may take.

Instrumentation costs nothing while off: the engine reads the fuel of a call anyway, and only
the recorder being on makes a call name its unit and read the store's counters.

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
(`dist/dev/nested-session.sh`), installs them there and opens the app through the session's own
action,
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
