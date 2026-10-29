# Workspaces, windows and views

What a person sees, what is shared and what is personal. The workspace itself (members,
devices, roles) is defined in `docs/architecture/workspace.md`; this doc builds on it and uses
its names.

## 1. Decisions

- **The workspace is the unit of sharing.** You, your team and your agents work in one. What
  it shares is meaning: resources, actions, tasks, permissions, history and presence.
  Something private goes in a workspace only you are in.
- **Arrangement is each person's.** How things are laid out (windows, strips, a canvas, a
  dock, one long list, a single conversation) belongs to that person's shell, synced across
  their devices that use it. Nobody else's shell can lay out yours, and no shell has to have
  windows at all.
- **Attention is visible inside a workspace.** There's no hidden mode inside a shared one:
  which resource someone is on, and what they selected, shows in every shell.
- **A resource is state.** Views and actions belong to it but are separate from it.

## 2. The pieces

| Piece | Is | Shared? | Lasts |
|---|---|---|---|
| **Resource** | typed state: a note, a notebook, a conversation | with the workspace's permissions | until deleted, with history |
| **View** | a function from a resource to UI; a type has several (editor, card, row) | it's code | |
| **Shell** | a view of the person's arrangement and the workspace: strips, canvas, dock, top bar, list, conversation | the person's choice, per device class | |
| **Arrangement** | the shell's state: what is open where, in the shell's own terms (windows and strips, canvas places, a conversation's order) | personal, synced across that person's devices | until closed |
| **Window state** | how one place shows its resource: which view, sort, filter | personal | as long as that place |
| **Lens** | scroll, cursor, selection, focus | personal, visible as presence | per person and resource, on all their devices |

In code today the arrangement is still the workspace's (`tilo_desktop_model`'s workspace; a
strip is `layout::Strip`, a window a `layout::Tab` in a `layout::WindowGroup`), shared by the
room's layout record. That is the default strips shell's state and moves to the person's own
record when the shell becomes customizable (§10).

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

## 4. Windows (in shells that have them)

- **A window is its resource.** Showing a note through another view changes that window's
  view; it isn't a second window. Two presentations side by side (an editor and its preview)
  are laid out inside one window.
- **Opening means going there.** `open(r)` moves you to where your shell already shows `r`,
  or opens it right after where you came from; others already on `r` show as presence. That's
  how a trail forms. `open(r, in: new_strip)` starts a new line of work.
- **List and detail are two windows.** Pressing a note in a notebook opens the note's window
  next to it. The notebook keeps no "selected note". On a phone, one window shows at a time and
  back returns to the previous one.
- **Closing** removes it from your arrangement; it never touches the resource and nobody
  else's arrangement. Windows nobody has looked at for a while shrink into a compact entry at the end of
  their strip.

## 5. Lenses and presence

- One lens per person per resource. It syncs to that person's devices, so you continue on the
  phone where you left off.
- Lenses are presence: everyone sees where others are and what they selected. Agents have
  lenses too.
- **Following** ties your lens to someone else's: you see what they see.
- **Embedded views** (a card in a chat message, an agent's answer) aren't windows; each viewer
  has their own lens on them. Opening something from them goes to its window.

## 6. Arrangement

- Each shell stores its own structure, not pixels: strips of windows, canvas places, a dock's
  items, a conversation's order. Each screen lays it out with the views' size preferences
  (layout.md).
- What others do reaches your shell as presence and as tasks, never as a change to your
  layout: an agent's task shows where your shell shows tasks, and its answer opens where you
  open it.
- **The canvas** (canvas.md) is one shell among others. Working together on one surface is a
  canvas *resource* the members share and open, not everyone's desktop.

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

- `window` state belongs to the place showing the resource in the person's arrangement, `lens`
  state to the person; both are personal and typed. State everyone should share is a field of
  the resource. Scroll, focus, cursor and
  selection are lens state the platform keeps without being declared.
- An app whose data is each person's own (mail) says so in its header:
  `app so.tilo.lang.mail "Mail" (records: .own)`. Each member then keeps the app's resources in
  their own record of the workspace (`member:<person>:<kind>`, `tilo_action_policy::member_record`),
  which only their devices hold the key to and every door decides as theirs alone, the room's
  owner included. Another member opening the app in the same workspace starts from their own,
  empty record.
- `open(r)` is an action usable from views and flows.

## 9. The arrangement is a resource

A person's arrangement is typed state of theirs, in the shell's own terms, synced across their
devices with history and undo. The shell is a view of it and of the workspace (resources,
tasks, presence). Arranging is actions on it, so agents can open things for the person too,
within the same rules.

## 10. Shells

A shell is a way to present the workspace on a device: a scrolling strip window manager, a 2D
canvas, a phone, a watch, a browser. Every shell shows the same resources and actions; how
things look and are arranged follows the device. Pixels are not shared between shells; meaning
is: resources, actions, permissions, undo and history.

**A customizable shell.** A shell is a tilolang view of the person's arrangement, composed
from parts: docks, top bars, strips, a canvas, a launcher list, a conversation. A few default
compositions ship; anyone can write another, as with any view. Rust keeps the mechanics as
described components (window drag and resize, strip scrolling, canvas pan and zoom, gestures
at native refresh), each committing one arranging action when a gesture ends. Themes change
how parts look, never which parts exist. Apps draw no chrome, colours or fonts of their own, so
every shell and theme reaches them. Asks, the trusted sheet, sign-in and attribution stay
system-drawn above any shell view, which never sees secrets or draws an ask.

**The web.** The browser is already a shell (tabs, history, back and forward, address bar), so
a web session uses it instead of drawing a desktop inside it:

- One open resource is, by default, one tab with its own URL, opened through its default view.
  Links are real links; back and forward follow the trail of what was opened.
- A strip or a canvas is a workspace resource too, so it has a URL. Opening one gives that tab
  a scrolling window manager or a 2D canvas. Arrangement appears only where someone opens one,
  never as a frame around everything.
- Views render as real DOM components (text fields, lists, links), so the browser supplies
  input, IME, accessibility, find, translation and autofill. Shared design tokens keep it
  recognisably Tilo; components follow web conventions rather than matching the native look.
- Outside services sync on the workspace's host, never in the page; the browser shows what the
  workspace holds.

## 11. Open

- Whether others see a draft's text live or only that someone is typing.
- Whether people create strips explicitly, or a strip starts when something is opened from
  search or the launcher.
- How long before an unvisited window shrinks.
- Strips from several workspaces side by side on one screen.
