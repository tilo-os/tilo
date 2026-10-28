# Workspaces, windows and views

What a person sees, what is shared and what is personal. The workspace itself (members,
devices, roles) is defined in `docs/architecture/workspace.md`; this doc builds on it and uses
its names.

## 1. Decisions

- **The workspace is the unit of sharing.** You, your team and your agents work in one, and
  its arrangement is shared. Something private goes in a workspace only you are in.
- **One reason, one window.** A list of notes and an open note are two reasons, so two windows.
- **Attention is visible inside a workspace.** There's no hidden mode inside a shared one.
- **A resource is state.** Views and actions belong to it but are separate from it.

## 2. The pieces

| Piece | Is | Shared? | Lasts |
|---|---|---|---|
| **Resource** | typed state: a note, a notebook, a conversation | with the workspace's permissions | until deleted, with history |
| **View** | a function from a resource to UI; a type has several (editor, card, row) | it's code | |
| **Window** | the workspace's one place for one resource, showing it through a view | yes | until closed |
| **Window state** | how the workspace shows it: which view, sort, filter, size | yes | as long as the window |
| **Lens** | how one person looks: scroll, cursor, selection, focus | personal, visible as presence | per person and window, on all their devices |
| **Strip** | an ordered row of windows that belong together: a line of work | yes | until closed |

In code: the workspace is `tilo_desktop_model`'s workspace; a strip is `layout::Strip`; a
window is a `layout::Tab` in a `layout::WindowGroup`; resources and actions are `tilo-actions`.
Window state and lenses get added to `layout`, not beside it.

## 3. Apps

An app is a bundle: types, actions, views and connections under one id. It isn't a process, a
window or a place.

- **People open things, not apps.** You open a notebook, and it appears through its type's
  default view.
- **Root types replace launching.** A type declared `root` exists once per workspace (a
  mailbox, a library). "Open Music" goes to the library's window, or creates it.
- **New means picking a type:** a note, a list, a poll.
- **Anyone can add views for any type,** within the authority rules (authority.md). "Show as…"
  lists the views available for a window's resource. Apps provide ways to see and change
  things; they aren't walls around data.

## 4. Windows

- **A window is its resource.** Showing a note through another view changes that window's
  view; it isn't a second window. Two presentations side by side (an editor and its preview)
  are laid out inside one window.
- **Opening means going there.** `open(r)` moves you to `r`'s window if it exists (perhaps
  where someone else already is), or creates it right after the window you came from. That's
  how a trail forms. `open(r, in: new_strip)` starts a new line of work.
- **List and detail are two windows.** Pressing a note in a notebook opens the note's window
  next to it. The notebook keeps no "selected note". On a phone, one window shows at a time and
  back returns to the previous one.
- **Closing** removes the window for everyone, attributed and undoable; it never touches the
  resource. Windows nobody has looked at for a while shrink into a compact entry at the end of
  their strip.

## 5. Lenses and presence

- One lens per person per window. It syncs to that person's devices, so you continue on the
  phone where you left off.
- Lenses are presence: everyone sees where others are and what they selected. Agents have
  lenses too.
- **Following** ties your lens to someone else's: you see what they see.
- **Embedded views** (a card in a chat message, an agent's answer) aren't windows; each viewer
  has their own lens on them. Opening something from them goes to its window.

## 6. Arrangement

- The workspace stores structure: strips of windows, not pixel positions. Each screen lays it
  out with the windows' size preferences (layout.md): several side by side on an ultrawide, one
  at a time on a phone.
- Which strip and window you're looking at is part of your lens.
- A strip is a line of work, not a person's. An agent's task gets a strip; when the task ends,
  the strip shrinks to its result.
- **The canvas** (canvas.md) is another arrangement of the same windows on a 2D surface.
  Open: how a shared canvas lays out on a phone (one window at a time along an order derived
  from the canvas, or a per-device arrangement).

## 7. Actions

- A resource's actions are available in its views, in the command surface for what has focus,
  and to agents.
- Arranging is actions too: open, close, move to another strip, follow. Agents can do them
  under the same rules.
- Every change shows where it happened: as presence in the windows showing the resource, and
  as a suggestion with a preview when it waits for approval.

## 8. In the language

```
view note_list(nb: Notebook) {
    window sort: NoteOrder = .recent    // shared: how the workspace shows this list
    lens expanded: Set<Tag>             // personal: which groups I opened
    stack { for n in nb.notes.sort_by(n -> sort.key(n)) { note_row(n) } }
}

root Notebook
default view note_editor for Note
```

- `window` state is shared, `lens` state is personal; both are typed. Scroll, focus, cursor and
  selection are lens state the platform keeps without being declared.
- `open(r)` is an action usable from views and flows.

## 9. The workspace is a resource

The arrangement is typed state (strips have windows; a window has a resource, a view and window
state), synced like everything else, with history of who opened, moved and closed what. The
shell is a view of it.

## 10. Open

- Whether others see a draft's text live or only that someone is typing.
- Whether people create strips explicitly, or a strip starts when something is opened from
  search or the launcher.
- How long before an unvisited window shrinks.
- Strips from several workspaces side by side on one screen.
