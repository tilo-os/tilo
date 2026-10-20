# Workspaces, windows, views and apps

This document defines what a person sees in Tilo, what is shared and what is personal, and what an
"app" is once apps are written in tilolang. The language spec, the layout model and the agent
design depend on it.

## 0. Decisions this builds on

These are the user's decisions, not up for re-litigation:

- **The workspace is the unit of sharing.** You, your team and your agents work in one workspace.
  Its arrangement is shared.
- **One reason, one window.** Something you look at for one reason exists once in the workspace. A
  list of notes and an open note are two reasons, so two windows.
- **Attention is public inside a workspace.** Private things belong in your personal workspace (a
  workspace with one member), not in a hidden mode inside a shared one.
- **A resource is state.** Its views and actions are associated with it but distinct from it.

## 1. The model in one table

Everything a person sees is made of six things. Each has one owner and one lifetime.

| Concept | Is | Shared? | Lives as long as |
|---|---|---|---|
| **Resource** | Typed state: a note, a notebook, an album, a conversation, a draft. | Yes, with the workspace's permissions. | Until deleted, with history. |
| **View** | A pure function from a resource (plus window state and lens) to UI. Declared by a package. A type has several: editor, card, row, outline. | — (it's code) | — |
| **Window** | The workspace's single place for one resource. It shows the resource through a view. | Yes. Everyone in the workspace sees the same windows. | Until closed. |
| **Window state** | How the workspace presents that resource: which view, sort, filter, grouping, the window's preferred size. | Yes. | As long as the window. |
| **Lens** | How *one person* is looking: scroll, cursor, selection, reading position, which section is expanded, focus. | Personal, but visible as presence. | Per person and window. Follows you to your other devices. |
| **Lane** | An ordered sequence of windows that belong together: a line of work. A workspace has several lanes. | Yes. | Until closed or archived. |

Three layers of state follow from the table. Every piece of state in an app sits in exactly one of
them, and the author says which:

1. **Resource state:** what the thing *is*. Synced, attributed, undoable, and kept after every
   window closes.
2. **Window state:** how the *workspace* presents it. Shared, and gone when the window closes.
3. **Lens:** how *I* look at it. Personal, published as presence, and resumed on my other devices.

## 2. What an app is

An app is a **package**: a set of types, actions and views (and flows) under one id. It is not a
process, a window or a place.

- **People open things, not apps.** You open a notebook, an album, a conversation, and it appears
  in its window through the default view for its type. "Notes" is the package that knows what a
  notebook is and how to show one.
- **Root types replace "launching".** A package may declare **root types**: resources that exist
  once per workspace, such as Mail's mailbox, Music's library, or Notes' notebook. The launcher
  lists roots. "Open Music" opens the workspace's library window: it goes to the window if one
  exists, and creates it otherwise.
- **Creating means choosing a type.** "New" lists the types the workspace's packages offer: a note,
  a todo list, a poll.
- **Views from any package.** A package can declare views (and actions) for *another* package's
  types, subject to authority (spec §4). The same note can be shown by Notes' editor, by an outline
  view from another package, or by a view an agent wrote. "Show as…" on a window lists the views
  available for its resource. This is the customization story: apps are providers of ways to see
  and change things. They are not walls around data.
- **Packages appear in three places only:** "Show as…" (which views exist), attribution and
  authority ("this view comes from X and can do Y"), and the launcher's roots.

## 3. Windows

**Identity.** A window is identified by its **resource**.

- Showing the same note through a different view is not a second reason. It changes the window's
  view (window state, so shared).
- A view that wants two presentations of one resource side by side (an editor and its preview)
  lays them out *inside* its window.

**Opening means going there.** `open(r)` is a platform action:

- If `r` has a window, your lens moves to that window. Someone else may already be in it. Then you
  are now working together, without a separate mode for it.
- Otherwise a window for `r` is created. It's placed in the lane of the window you opened it from,
  directly after that window. That is how a trail forms: notebook, then note, then a linked note.
- `open(r, in: new_lane)` starts a new line of work instead.

**List and detail are two windows.** A notebook window lists notes. Pressing a note runs `open(n)`,
and the note's window appears next to the notebook's. The app keeps no "selected note" state at
all; that question was never the notebook's to answer. On a narrow screen the lane shows one window
at a time, and back goes to the previous one (§5).

**Closing.**

- `close` removes the window for everyone. It's a shared change, attributed and undoable: "Alex
  closed *Q3 plan*". Closing never touches the resource.
- Windows nobody has looked at for a while **recede**: they collapse into a compact entry at the
  end of their lane instead of taking space. Opening them again restores them.

Together these answer the clutter question without anyone having to tidy up.

## 4. Lenses and presence

- **One lens per person per window.** The view declares which state is lens state (§7). The
  platform stores lenses per person. They sync to that person's devices, so you resume on the
  phone where you left off on the laptop.
- **Lenses are presence.** Everyone in the workspace sees, through the views, where others are and
  what they have selected: their cursor in a text, their selection in a list, their position in a
  book. Agents have lenses too.
- **Following** binds your lens to another person's or agent's live lens: you see what they see,
  scroll as they scroll. Reading a book together means following each other in the book's single
  window. When you stop following, your own lens resumes.
- **Embedded views** (an album card in a chat message, an agent's block, a dashboard cell) are not
  windows. Each viewer has their own lens on them, nested in their lens on the host window. They
  have no window state of their own. Opening anything from them runs `open(r)`, which goes to that
  resource's window.

## 5. Lanes and screens

**The arrangement is structure, not geometry.**

- The workspace stores **lanes**: ordered sequences of windows. It stores no pixel positions.
- Every screen lays the structure out for itself, using the windows' size preferences in `rem`
  (layout.md):
  - an ultrawide shows several windows of a lane side by side;
  - a laptop shows two or three;
  - a phone shows one, and swipes along the lane.
- A shared arrangement can work across different screens only if it is structural. A free 2D canvas
  can't be laid out per device and still be "the same" for everyone.

**Your viewport is part of your lens.** Which lane and which window you're looking at is personal
and published as presence. The workspace overview shows all lanes with everyone's position on
them.

**What a lane is.** A lane is a **line of work**: a topic or a task, not a person. People and agents
work in the same lanes. When an agent starts a task, it works in a lane named for the task. Opening
a window that already exists brings it to that window (§3), so its presence shows up inside windows
other people are looking at.

**When a task finishes,** its lane collapses to the task's result: an answer, a summary of what
changed. Its windows recede. Nothing is lost: the windows come back when opened, and the lane can
be expanded.

## 6. Actions

- Actions belong to types and are available wherever the resource is:
  - **in its views** as primary controls;
  - **in the command surface** for whatever has focus (every action of the focused resource);
  - **in the agent catalogue.**
- **The room has actions too.** `open`, `close`, moving a window to another lane, starting a lane
  and following are actions on the workspace resource. Agents can invoke them like any action
  ("put the notes next to the chat"), under the same authority rules.
- **Visible change** (from the representation design):
  - every invocation shows **in place** as presence and provenance in the windows that show the
    affected resource;
  - **elsewhere** as an invocation card in the lane where it happened;
  - **as a suggestion**, through the action's proposal view, when it waits for approval.

## 7. What this changes in the language

**View state is declared, with its layer:**

```
view note_list(nb: Notebook) {
  window sort: NoteOrder = .recent          // shared: how the workspace shows this list
  lens   expanded: Set<Tag>                 // personal: which tag groups I opened

  stack {
    for n in nb.notes.sort_by(n -> sort.key(n)) {
      note_row(n)                           // pressing a row runs open(n)
    }
  }
}
```

- `window` state is stored with the window and is shared. `lens` state is stored per person. Both
  are typed like resource fields.
- Scroll, focus, cursor and text selection are lens state the platform keeps by itself. Views don't
  declare them.

**`open(r)`** (and `open(r, view: v)`, `open(r, in: new_lane)`) is a platform action, usable from
views (as an action reference on a control) and from flows.

**Package-level declarations:**

```
root Notebook                                  // one per workspace, listed in the launcher
default view note_editor for Note              // the view a Note's window uses unless changed
```

**Views for foreign types** are allowed: `view outline(n: so.tilo.notes.Note)` in another package.
Their actions on that resource are foreign actions under the authority rules.

**`split` stays,** but only for genuinely one-resource layouts: an editor and its preview, a player
and its queue. List and detail are two windows, not a split.

## 8. The workspace is a resource

- The arrangement is itself typed state: a `Workspace` has lanes, a lane has windows, and a window
  has a resource reference, a view and window state. It syncs like everything else, and its
  history says who opened, moved and closed what.
- Lenses are per-person state next to it.
- The desktop shell is therefore a view of the workspace. It can eventually be written in tilolang
  like any app. A dashboard is a different view of the same structure, or a resource of its own
  that embeds views.

## 9. Open questions

1. **Drafts.** They are resources with an owner (decided). The open question is whether others see
   their contents live or only that someone is typing. Under §0, it's visible; the chat view
   decides what to render (a typing indicator, not the text). Confirm.
2. **Lane naming and creation.** Agents name their task lanes. Do people create lanes explicitly,
   or does a lane start implicitly when you open something from the launcher or a search rather
   than from a window?
3. **Receding.** It's driven by attention: nobody has looked at the window for a while. What counts
   as a while, and does it differ per kind of window?
4. **Several workspaces at once.** Is a person always in exactly one workspace on a screen? Or can
   lanes from two workspaces sit side by side (a personal notes lane next to a team lane)?
