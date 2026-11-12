# Workspaces, windows and views

What a person sees, what is shared and what is personal. The workspace itself (members,
devices, roles) is defined in `docs/architecture/workspace.md`; this doc builds on it and uses
its names.

## 1. Decisions

- **The workspace is the unit of sharing.** You, your team and your agents work in one. What
  it shares is meaning: resources, actions, tasks, permissions, history and presence.
  Something private goes in a workspace only you are in.
- **The arrangement is the workspace's.** What is open where (strips, window groups,
  windows, canvas places) is one workspace record: every person, agent and device in the
  workspace has the same windows. Each shell lays that out for its device, and no shell has
  to have windows at all.
- **How you look inside a window is yours.** Which place a window's sidebar shows, its sort,
  the active tab of a group, your scroll, cursor and selection: each person's own, on all their
  devices, and visible to everyone else as presence.
- **Attention is visible inside a workspace.** There's no hidden mode inside a shared one:
  which resource someone is on, and what they selected, shows in every shell.
- **Drafts are records.** Half-typed text is part of the workspace: others see it as it is
  typed, on every device, like any other change.
- **A resource is state.** Views and actions belong to it but are separate from it.

## 2. The pieces

| Piece | Is | Shared? | Lasts |
|---|---|---|---|
| **Resource** | typed state: a note, a notebook, a conversation | with the workspace's permissions | until deleted, with history |
| **View** | a function from a resource to UI; a type has several (editor, card, row) | it's code | |
| **Shell** | a view of the workspace's arrangement and resources: strips, canvas, dock, top bar, list, conversation | the person's choice, per device class | |
| **Arrangement** | what is open where (strips, window groups, windows, canvas places) | with the workspace: everyone has the same windows | until closed |
| **View state** | what one person keeps in a window: how it shows its resource (which view, sidebar place, sort, filter, active tab) and where they are in it (a draft, what is open); scroll, cursor, selection and focus the platform keeps without a declaration | personal, visible as presence | per person and window, on all their devices |

In code the arrangement is the workspace's layout record (`tilo_desktop_model`'s workspace; a
strip is `layout::Strip`, a window a `layout::Tab` in a `layout::WindowGroup`).

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
- **A list and its item usually share one window.** A notebook's window has the notes in its
  sidebar and the chosen note beside them; which note is chosen is each person's view state,
  so Ada can read one note while Bo writes another, each seeing the other's face at their note.
  A view that wants a second window (a custom layout, a preview beside an editor) opens one in
  the shared arrangement. On a phone, one pane shows at a time and back returns to the list.
- **A window with nothing at all to list says so once.** While the app's list holds nothing (a
  first run, an empty library) and nothing is chosen, the window shows the app's own state for
  what is missing, spanning the place its columns would stand: never two states side by side
  (the places' and the content's), and never a phone opening on "nothing open" when there is
  nothing to open. The app declares the words and the line under them; the bar keeps the way to
  make the first, and the columns come back with it.
- **A window that shows a list, or the switch between views, takes the window over.** A
  notebook's list is the window's list; opening its project shows the project's list where the
  notebook's stood, with the project's title, its view switch and its controls in the one bar,
  and the sidebar keeps the places to go back by. A view that offers neither (a page) opens
  beside the list it came from. Which views a window switches between is the app's
  declaration (`views list, board for Project`); a view it does not name is a helper its own
  views place, never a window.
- **Closing** removes the window from the workspace's arrangement; it never touches the
  resource. Windows nobody has looked at for a while shrink into a compact entry at the end of
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
- Others work in the same windows: a person's or an agent's work shows as their presence
  where it happens, and a window they open is a window of the workspace, next to where they
  came from.
- **The canvas** (canvas.md) is one shell among others. Working together on one surface is a
  canvas *resource* the members share and open, not everyone's desktop.

## 7. Actions

- A resource's actions are available in its views, in the command surface for what has focus,
  and to agents.
- Arranging (open, close, move) is an action on the workspace's arrangement, attributed and
  undoable like any other; agents arrange as collaborators do, visibly. Following and moving
  your own lens are yours alone.
- Every change shows where it happened: marked in the views showing the resource, as presence,
  and in its task (shell.md, "What a person sees of a task").

## 8. In the language

```
view note_list(nb: Notebook) {
    state order: NoteOrder = .recent    // personal: how this window shows the list
    state expanded: Set<Tag>            // personal: which groups I opened
    stack { for n in nb.notes.sort_by(n -> order.key(n)) { note_row(n) } }
}

root notebook: Notebook
default view note_editor for Note
```

- `state` declares what the person keeps in a view: how its window shows the resource, and where
  they are in it. Each declaration is personal, typed, kept per person and view on all their
  devices, and visible to others as presence; `set(name, value)` changes one. State everyone
  should share is a field of the resource. Scroll, focus, cursor and selection are kept by the
  platform without being declared.
- An app whose data is each person's own (mail) says so in its header:
  `app so.tilo.lang.mail "Mail" (records: .own)`. Each member then keeps the app's resources in
  their own record of the workspace (`member:<person>:<kind>`, `tilo_action_policy::member_record`),
  which only their devices hold the key to and every door decides as theirs alone, the
  workspace's owner included. Another member opening the app in the same workspace starts from
  their own, empty record.
- `open(r)` is an action usable from views and from actions.

## 9. The arrangement is a resource

The workspace's arrangement is typed state of the workspace, synced to every member's devices
with history and undo. Each shell is a view of it and of the resources, tasks and presence.
People and agents arrange it, attributed and undoable like any change.

## 10. Shells

A shell is a way to present the workspace on a device: a scrolling strip window manager, a 2D
canvas, a phone, a watch, a browser. Every shell shows the same resources and actions; how
things look and are arranged follows the device. Pixels are not shared between shells; meaning
is: resources, actions, permissions, undo and history.

**A customizable shell.** A shell is a tilolang view of the workspace's arrangement, composed
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

- Whether people create strips explicitly, or a strip starts when something is opened from
  search or the launcher.
- How long before an unvisited window shrinks.
- Strips from several workspaces side by side on one screen.
