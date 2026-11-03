# tilolang

tilolang is Tilo's principles (AGENTS.md, "Principles") as a language. You describe your data
and what can be done with it; collaboration, sync, search, undo, history, agent access and a
native UI come from the system.

## The model

- **Resources** are the data: a note, a mail, a video. Each is a typed record.
- **Each resource has an origin:** the server with the authoritative copy. Notes: the workspace
  host. Mail: the IMAP server. Videos: YouTube.
- **Capabilities** are what can be done with resources: get them, search them, change them
  through actions. A resource's capabilities reach its origin: for notes, the workspace host;
  for mail, the IMAP server, through the mail app's connection.
- **Actions** are the only way to change a resource. They're typed, attributed to who did
  them, undoable, and checked against permissions.
- **Views** show resources. They're plain functions of resources. Every resource type has a
  default view, and anyone (you, an agent) can write others: a view someone writes is a real
  `view` declaration, kept as that person's own code (agent-ux.md, "Answers as views").
- **The system** decides when and where all of this runs: search across everything or within
  one thing, windows, the canvas, agents' programs, automations.

An "app" is a bundle of these: some types, their actions, default views, and connections to
outside services. It isn't a boundary: an agent can compose a view from several apps'
resources, and your search covers all of them.

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
| Chat, the flagship app | chat.md |
