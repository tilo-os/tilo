# tilolang

tilolang is Tilo's principles (AGENTS.md, "Principles") as a language. You describe your data
and what can be done with it; collaboration, sync, search, undo, history, agent access and a
native UI come from the system.

## The model

- **The workspace is a tree of resources**, versioned and changed only by actions: the same
  tree that holds a person's home and a workspace's folders, code packages included.
- **A resource** is a typed node: fields hold its values, child lists hold the resources nested
  in it (`subtasks: List<Task>`, `items: List<Task | notes.Note>`). Every resource has an id,
  links, presence and permissions. How resources are stored, synced and encrypted is the
  platform's and never shows in the language.
- **History, versions and authorship belong to every resource and every action alike.** Who
  changed what, when, through which action and made from what is read from the log
  (`history(r)`, `created(r)`, `updated(r)`, and `changed(r, .field)`: the version that last
  changed one of its fields), never declared or stored by a type. An action that makes one
  resource and asks another package's action about it calls that one made from it
  (`agent.ask(...).made_from(sent)`): the call is a change of its own, with its own provenance.
- **Packages** declare types, actions, views and functions. Packages are untrusted: a type's
  resources change only through its own package's actions; other packages read what they are
  allowed to and call those actions. A library is a package like any other; `std` is one, and so
  is a connector to an outside service, written in tilolang or backed by Rust.
- **Each app has a root:** one resource per workspace at the top of the tree, made when the app
  is first used. Opening the app opens it; what is made without a more specific place goes in
  it. Resources can live anywhere else in the tree too: a note inside a project.
- **Queries**: `T.all` and the type's declared filters (`Task.today`) are lazy and answered by
  the index; `in: r` narrows to a subtree. Nothing loads more than what is shown.
- **Views** are functions of resources, plus each person's own view state, which shows as
  presence. `default view v for T` is what opens a resource of `T`, `row view r for T` how it
  appears in a list, and `place view p for T` what a place of it shows access on its own when
  its default view is the app's window (a root's). Views decide which actions they show and where.
- **Actions** are the only way to change anything, and every action is available to people,
  agents, the palette and programs. They are typed, attributed, undoable and permission-checked,
  declare their effects (`uses web`, `uses random`, `uses time`) and may carry metadata a view
  can use. A program that runs several actions is just code calling them.
- **Agents speak tilolang.** An agent or REPL session has the agent library in scope without a
  `use`: `grep`, `find`, `read`, `describe`, `print`, and `me`, the person it acts for. Any
  other package `use`s it like a library and calls `agent.grep(...)` — one library, one
  syntax, no agent dialect.
- **Nothing domain-specific is in the language:** no field roles, no task or chat words, no
  hard-coded services or windows. What done, nested or due means is an app's own fields and code.

## A small example

From `apps/tl/notes/app.tl`:

```
/// A note: a title, a body and the tags people gave it.
type Note (untitled: "Untitled") {
    title: Text
    body: RichText
    pinned: Bool = false
    tags: Set<Text>

    search {
        filter pinned (icon: .pin)
        filter tags
    }
}

/// The workspace's notes.
type Notebook {
    notes: Collection<Note>
}

root notebook: Notebook

/// Starts a note. Its title starts empty: what is typed first names it.
primary action new_note(nb: Notebook) -> Note (icon: .plus) {
    nb.notes.insert(Note(title: ""))
}

/// Pins or unpins a note.
action pin(n: Note, pinned: Bool = not n.pinned) (icon: .pin, on: n.pinned) {
    n.pinned = pinned
}
```

`(untitled: "Untitled")` says once what a note is called while its title is empty: its row, its
window's title, a reference to it, its history and its title field's placeholder all read those
words, translated in the app's catalogue, so no view writes the fallback itself. A type's resources
are named by its first text field; `(title: subject)` names the one that does it instead, as a
draft whose first field is the address it goes from is called by its subject:

```
type Draft (title: subject, untitled: "New message") {
    from: Text
    to: Text
    subject: Text
}
```

Those lines give collaborative, synced, searchable, undoable notes with a native default UI,
which people, agents and automations can find, read and pin.

A resource's document body is its `RichText` field, whether it is kept as `RichText` (its
paragraphs, lists, quotes, tables and code kept as such) or as `Text` (the body kept as
Markdown); the document's own view places the editor for it, `ui.BlockEditor(bind: note.body)`.

## Opening files

An action that takes a `File` says which kinds of file open the app with it:

```
action import_markdown(nb: Notebook, file: File) -> Note (
    icon: .file_plus,
    opens: ["text/markdown", .md, .markdown, .txt],
) { … }
```

Each kind is a media type in quotes or an extension with a dot. They become the app's manifest's
`[providers]`, so Files, and the person's own file system, offer the app for exactly those kinds
and for no other; the app's name never appears in a table of file types. A file opened that way
reaches the action as the same `File` a chooser gives it — the address of its content and its name,
since an app has no path of its own — and what the action returns opens in the window. The action
takes one of the package's roots and the file, and nothing else: that root is the record the file
is opened into. One action of an app opens a kind of file, so a Markdown file and an archive of
pages are two actions.

## Effects

An effect is something a replay cannot compute again: a draw, the time, a service outside
Tilo, a person told something, an image's pixels decoded on the device. The action that uses one
declares it (`uses random`, `uses time`, `uses web`, `uses notify`, `uses image`) and the compiler
refuses it anywhere else (E121); a view never reads one, since it is drawn again from what it is
shown.

```
action roll(die: Die) uses random {
    die.value = random.int(1, 6)
    die.mark = random.id()
}
```

`random.int(lo, hi)` draws a number in `lo..=hi`; `random.float()` one in `[0, 1)`;
`random.choice(xs)` one item of a list, as an `Option` (nothing drawn from nothing);
`random.shuffle(xs)` the same items in a drawn order; `random.id()` 128 random bits as text.
The draw is made on the device that invokes the action, from the system's
cryptographic random source, and the numbers it drew are kept in the invocation's journal: a
run resumed after waiting for a person, and any run replayed, reads the numbers it drew the
first time.

`notify(to: who, title: text, body: text, opens: r)` tells the person `who` something. A notice
is a resource the action writes in its own change: the workspace keeps it like any other resource,
so every device of that person reads it and no one else's does, and what one of them does with
it — dismiss it, or choose it and open the resource `r` — is a change of the resource too, which
takes it down on all of theirs. A device tells only what reaches it as news: a notice the resource
held when it first looked, or one it told before, stays quiet, so a replay and a device joining
late tell nothing again. The journal records that the notice was written, so a run resumed or
replayed writes no second one.

What a notice says is words a person reads: its `title` and `body` are messages of the action,
translated in the package's catalogues like the text of a view (`d-<action>.w-<word>`).

`clipboard.copy(text)`, `clipboard.copy_image(file)`, `clipboard.paste()` and
`clipboard.paste_image()` are the person's clipboard. A copy is theirs: it reaches their device
and stays there as a copy of theirs, kept in their history and on their other devices like one
they made in an app. A paste reads what is on the clipboard now, as an `Option<Text>` or an
`Option<File>` — an image the device keeps for the app once pasted. An action that copies or
pastes declares `uses clipboard`; a view does neither, since it is drawn again from what it is
shown.

`file.image_info()` says what an image's own container states: its upright width and height, the
capture time, the EXIF orientation, and the place the camera recorded. `file.cropped(x, y, width,
height)`, `file.turned(quarters)`, `file.resized(width, height)` and `file.encoded(format: "png")`
work on its pixels and answer a new `File` kept in the content store. The decode turns the picture
upright first, so a crop fraction and a size are of the picture a viewer shows, and a turn is
clockwise. An action that reads or makes pixels declares `uses image`; a view does not, since it is
drawn again from what it is shown.

`file.choose(a, accept: [...])` asks the person for files and runs `a` with what they chose: the one
file the next parameter takes, or every file when it is a `List<File>`. `file.choose_folder(a,
accept: [...])` asks for a folder and runs `a` with the files it holds, each with the path it stands
at inside it (`file.path`); `file.path` is that path, or the file's own name when one file was
chosen, and never a host path. Both stand on a control, since they ask the person at the window.

```
action remind(t: Task) uses notify {
    notify(
        to: t.assignee,
        title: t.title,
        body: if let d = t.due { "Due {d}" } else if let p = t.project { p.name } else { "" },
        opens: t,
    )
}
```

## Initial content

What a new resource starts with is an `initial action` on its type. The platform applies it
inside the making of each new resource of that type: a root the first time it is wanted, and an
item wherever code inserts one. It writes in the change that makes the resource, so what it
writes is the resource's making: one entry in history, nobody's change, and nothing anyone is
caught up on.

```
/// A new board starts with the three columns most teams use.
initial action columns(b: Board) {
    b.columns.insert(Column(title: "To do"))
    b.columns.insert(Column(title: "Doing"))
    b.columns.insert(Column(title: "Done"))
}
```

A resource that already exists never gets it, so changing an initial action changes what new
resources start with and nothing else; upgrading what an earlier version wrote is a `migration
action`'s job. A type has at most one. It takes the new resource and nothing else, answers
nothing, declares no `uses` and holds no `require`: it is the same wherever and whenever the
resource is made. It calls no action, since each action is a change of its own; it inserts what
it needs itself, and each item it inserts starts with its own type's initial content in the same
change. Content that would make itself again (a `Book` that starts with a `Book`) is refused.
Nothing invokes an initial action by name, and no surface offers it.

A field of a record type holds a reference. When a resource is made without it (a root, or
`Clocks()` inserted without `stopwatch:`), it starts holding a record of its own, built from its
type's defaults: `type Clocks { stopwatch: Stopwatch }` reads `clocks.stopwatch.elapsed` as `0`
from the start. That record is kept in the holder's document, so it has the same readers,
history, sync and removal. It is the same record on every device that makes the holder. The
checker refuses it where the making leaves the field out and the record cannot start on its own:
a field of it has no default, it would hold itself again, or it belongs to another app.

## Events

A handler (`on activate(x: Calculation): ...`) is data the host fills with the event's value
when it fires, never a closure. An action on a resource takes that value as it is
(`open(x)`), so what it is called with is what the person chose. A `set` of
the view's own declared state may compute from it, with field reads and `fn` calls:
`set(expr, x.expr).then(set(fresh, false))` loads a past calculation into the editor. The new
value is a function the runtime runs at the press, so it uses no action. View state is the
person's presentation, so this never changes a resource outside an action.

## Opening

What opening a resource does is an `opened action` on its type. The system's one open path runs
it whenever the resource is opened, from the app's own list, search, a link, an agent's `open` or
another app, so an app never chains it after its own `open(x)`:

```
/// Opening a conversation reads it.
opened action read_on_open(t: Thread) {
    mark_read(t, false)
}
```

It runs as the person who opened the resource, under their grants: a change it makes is theirs,
in their history. Someone who may not change the resource opens it all the same, without the
change; the open is never refused. A past version and a scenario's capture change nothing. A type
has at most one, it takes the opened resource and nothing else and answers nothing, and nothing
invokes it by name; no surface offers it.

## Triggers

An action that runs without anyone pressing anything is a `trigger`: the package says when it
runs, and the host invokes it as the app's own action, with the app's authority and its declared
effects, as undoable and as visible in history as any other.

```
/// A board's own clock: it recounts its columns every half hour.
trigger recount(b: Board) every 30m: recount_columns(b)

/// A reminder rings once, at the time it holds; move that time and it rings again there.
trigger due(r: Reminder) at r.at: ring(r)
```

A trigger names one type and runs for each of the package's resources of it. `every` takes a
length of time and runs at the start of each period on the clock (`every 30m` is on the half
hour); `at` reads a `Time` or `Option<Time>` field of the resource and runs once when that instant
has come. An `every` period that passed while every device was off runs once when one comes up,
and so does an `at` time that passed; a resource whose time moves to a later instant rings again
there. The action takes the resource (`ring(r)`) or a root (`root Board`) as its arguments.

The host that runs a trigger is the workspace's origin, the device that governs the workspace,
and it records in the same change what the trigger last did for the record: the period it ran
in, or the instant it fired at. A second run of the same period or instant is refused there, so
a trigger runs once across the workspace's devices however they sync. A trigger needs no window
of its app open: the workspace's document service runs it, as it runs the app's root, whether or
not anything of the app is on screen. Nothing polls: the service wakes at the next instant a
trigger is due. The field an `at` trigger reads is kept as one of the type's columns, so finding
the next instant costs the pages up to it, not the type's records: a reminder among ten
thousand tasks wakes at its own time and reads none of the others.

## Packages and libraries

A package is a directory of `.tl` files with one `app` header and its `app.ids`, the ids every
declaration in it takes. Packages `use` each other by app id, and the alias is what code names
them by:

```
use so.tilo.notes as notes

view todayboard(t: todo.Tasks, nb: notes.Notes) {
    ...
}
```

What a package declares is its own: document types, aliases, enums, functions, actions,
views and the resource types it roots. A package that `use`s it sees them as `alias.X`, and
they stay the library's: the ids are locked in the library's `app.ids`, and a resource its action
writes is an ordinary workspace document under the library's app id, written under the authority
of whoever applied the action.

The standard library is one of these packages (`tilo.std`, `crates/lang/tilolang/std/app.tl`):
the frames every window gets. Every package compiles with it and sees its declarations as
`std.X`, whether or not it writes a `use`. Like any package it is read from its source: the
tool, run in a checkout, compiles against that checkout's library, so an edit of it takes effect
at the next command without rebuilding the tool. The copy built into the tool is the packaged
release's, used where no checkout holds one.

The agent library is the other shipped one (`tilo.agent`,
`crates/lang/tilolang/src/agent.rs`): the words a session talks to a workspace with. Its
declarations are typed tilolang functions backed by host imports, so a session has them in
scope with no `use` and a package that wants them writes `use tilo.agent as agent`: its words
are then `agent.grep(...)`, and `agent.me` is the person it acts for.

A **template view** is a view that expands where it is called rather than being a function of
its own: it takes its name at the call, builds the caller's nodes, reads and view state under
the caller's authority, and takes its type, state, function and view parameters from the call.
Every view of `std` is one, so `std.list(...)` inside a view is that view's own list. A view is
a template when it is written `template view`, or when it takes type parameters or takes view
state, a function or a view (`template.rs`).

## How it runs

- Code compiles to wasm and runs sandboxed. Authority follows authorship: what code may do is
  the lesser of what the person may do and what its author is trusted with.
- Views produce native components, drawn by the shared app window (one renderer for every app),
  so an app costs a few megabytes, not a toolkit.
- Changes are CRDT-backed and replay identically everywhere; resources with an outside
  origin follow that service's rules.
- A package can declare **scenarios**: named seed data for screenshots, tests and benchmarks.
  They never touch a person's data; a real first launch starts empty.

## The detail docs

| Topic | Doc |
|---|---|
| Who may do what; versions | authority.md |
| Concurrency, offline, outside services, invariants, calls between actions | convergence.md |
| Search and identifiers | search.md |
| Words and their translations | i18n.md |
| Agents writing tilolang programs | agents.md |
| The agent experience | agent-ux.md |
| The canvas: point at something and type | canvas.md |
| One action system with native Tilo | action-system.md |
| How apps render cheaply (app window) | app-window.md |
| Components and custom widgets | components.md |
| Workspace, windows, views | workspace-model.md |
| Layout | layout.md |
| Forms, dialogs and diagrams | forms-and-diagrams.md |
| The shell | shell.md |
| Language changes found by writing the apps | language-changes.md |
| Chat | chat.md |
| Developing, publishing and zero-install use | publishing.md |
