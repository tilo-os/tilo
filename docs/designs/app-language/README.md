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
  (`history(r)`, `created(r)`, `updated(r)`, when a field last changed), never declared or
  stored by a type.
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
  appears anywhere else. Views decide which actions they show and where.
- **Actions** are the only way to change anything, and every action is available to people,
  agents, the palette and programs. They are typed, attributed, undoable and permission-checked,
  declare their effects (`uses web`, `uses random`, `uses time`) and may carry metadata a view
  can use. A program that runs several actions is just code calling them.
- **Agents speak tilolang.** An agent or REPL session has default imports for what agents
  often need (`grep`, `find`, `describe`).
- **Nothing domain-specific is in the language:** no field roles, no task or chat words, no
  hard-coded services or windows. What done, nested or due means is an app's own fields and code.

## A small example

From `apps/tl/notes/app.tl`:

```
/// A note: a title, a body and the tags people gave it.
type Note {
    title: Text (title)
    body: RichText (body)
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

root Notebook

/// Starts a note.
primary action new_note(nb: Notebook) -> Note (icon: .plus) {
    nb.notes.insert(Note(title: "Untitled"))
}

/// Pins or unpins a note.
action pin(n: Note, pinned: Bool = not n.pinned) (icon: .pin, on: n.pinned) {
    n.pinned = pinned
}
```

Those lines give collaborative, synced, searchable, undoable notes with a native default UI,
which people, agents and automations can find, read and pin.

A field marked `(body)` is the resource's document body: the document's own view places the
editor for it, `ui.BlockEditor(bind: note.body)`, whether the field holds `Text` (the body kept
as Markdown) or `RichText` (its paragraphs, lists, quotes, tables and code kept as such).

A field whose item comes round again says which day it counts from, and a field that reminds
someone says when it does and whom it reminds:

```
type Task {
    title: Text (title)
    due: Option<Date> = .none
    at: Option<Time> = .none
    assignee: Option<Principal> = .none
    repeats: Option<std.Repeat> (repeats: due) = .none
    remind: Option<std.Reminder> (reminds: due, at, who: assignee) = .none
}
```

The compiler refuses a `std.Repeat` field with no `(repeats: …)` and a `std.Reminder` with no
`(reminds: …)`, and refuses a role naming a field the type does not have or one of the wrong
type. The role is published with the type, so whatever holds the record — a desktop with the
app closed, an agent, another device — finds what repeats and when it reminds without running
the app's code. A record naming nobody is its creator's to be reminded of.

## Effects

An effect is something a replay cannot compute again: a draw, the time, a service outside
Tilo. The action or flow that uses one declares it (`uses random`, `uses time`, `uses web`) and
the compiler refuses it anywhere else (E121); a view never reads one, since it is drawn again
from what it is shown.

```
action roll(die: Die) uses random {
    die.value = random.int(1, 6)
    die.mark = random.id()
}
```

`random.int(lo, hi)` draws a number in `lo..=hi`; `random.float()` one in `[0, 1)`;
`random.choice(xs)` one item of a list, as an `Option` (nothing drawn from nothing);
`random.shuffle(xs)` the same items in a drawn order; `random.id()` 128 random bits as text.
The draw is made on the device that invokes the action or flow, from the system's
cryptographic random source, and the numbers it drew are kept in the invocation's journal: a
flow resumed after waiting for a person, and any run replayed, reads the numbers it drew the
first time.

## Packages and libraries

A package is a directory of `.tl` files with one `app` header and its `app.ids`, the ids every
declaration in it takes. Packages `use` each other by app id, and the alias is what code names
them by:

```
use so.tilo.lang.notes as notes

view todayboard(t: todo.Tasks, nb: notes.Notes) {
    ...
}
```

What a package declares is its own: record types, aliases, enums, functions, actions, flows,
views and the resource types it roots. A package that `use`s it sees them as `alias.X`, and
they stay the library's: the ids are locked in the library's `app.ids`, and a record its action
writes is an ordinary workspace record under the library's app id, written under the authority
of whoever applied the action.

The standard library is one of these packages (`tilo.std`, `crates/lang/tilolang/std/app.tl`):
the frames every window gets. Every package compiles with it and sees its declarations as
`std.X`, whether or not it writes a `use`.

A **template view** is a view that expands where it is called rather than being a function of
its own: it takes its name at the call, builds the caller's nodes, reads and view state under
the caller's authority, and takes its type, state, function and view parameters from the call.
Every view of `std` is one, so `std.list(...)` inside a view is that view's own list. A view is
a template when it is written `template view`, or when it takes type parameters or takes view
state, a function or a view (`template.rs`).

## How it runs

- Code compiles to wasm and runs sandboxed. Authority follows authorship: what code may do is
  the lesser of what the person may do and what its author is trusted with.
- Views produce native components, drawn by the shared UI host (one renderer for every app),
  so an app costs a few megabytes, not a toolkit.
- Changes are CRDT-backed and replay identically everywhere; resources with an outside
  origin follow that service's rules.
- A package can declare **scenarios**: named seed data for screenshots, tests and benchmarks.
  They never touch a person's data; a real first launch starts empty.

## The detail docs

| Topic | Doc |
|---|---|
| Who may do what; versions | authority.md |
| Concurrency, offline, outside services, invariants, flows | convergence.md |
| Search and identifiers | search.md |
| Words and their translations | i18n.md |
| Agents writing tilolang programs | agents.md |
| The agent experience | agent-ux.md |
| The canvas: point at something and type | canvas.md |
| One action system with native Tilo | action-system.md |
| How apps render cheaply (UI host) | ui-host.md |
| Components and custom widgets | components.md |
| Workspace, windows, views | workspace-model.md |
| Layout | layout.md |
| Forms, dialogs and diagrams | forms-and-diagrams.md |
| The shell | shell.md |
| The first new experience | first-experience.md |
| Language changes found by writing the apps | language-changes.md |
| Chat | chat.md |
