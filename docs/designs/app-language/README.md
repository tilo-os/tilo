# tilolang

tilolang is Tilo's principles (AGENTS.md, "Principles") as a language. You describe your data
and what can be done with it; collaboration, sync, search, undo, history, agent access and a
native UI come from the system.

## The model

- **Resources** are the data: a note, a mail, a video. Each is a typed record.
- **Each resource lives somewhere.** Notes live in the workspace. Mail lives on the mail
  server, videos on YouTube. Where a resource lives decides who has the final say on it.
- **Capabilities** are what can be done with resources: get them, search them, change them
  through actions. Whatever holds a resource offers its capabilities. For notes that's the
  workspace; for mail it's the code that talks to the mail server.
- **Actions** are the only way to change a resource. They're typed, attributed to who did
  them, undoable, and checked against permissions.
- **Views** show resources. They're plain functions of resources. Every resource type has a
  default view, and anyone (you, an agent) can compose others.
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
    body: Text (body)
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

## How it runs

- Code compiles to wasm and runs sandboxed. Authority follows authorship: what code may do is
  the lesser of what the person may do and what its author is trusted with.
- Views produce native components, drawn by the shared UI host (one renderer for every app),
  so an app costs a few megabytes, not a toolkit.
- Changes are CRDT-backed and replay identically everywhere; what lives outside the workspace
  follows the outside service's rules.

## The detail docs

| Topic | Doc |
|---|---|
| Concurrency, offline, outside services, invariants, flows | convergence.md |
| Search and identifiers | search.md |
| Agents writing tilolang programs | agents.md |
| The agent experience | agent-ux.md |
| The canvas: point at something and type | canvas.md |
| One action system with native Tilo | action-system.md |
| How apps render cheaply (UI host) | ui-host.md |
| Components and custom widgets | components.md |
| Workspace, windows, views | workspace-model.md |
| Layout | layout.md |
| The first new experience | first-experience.md |
