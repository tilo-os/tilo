# The shell

A shell presents the workspace on one device for one person. It is a tilolang view, so people
choose, change or write their own. Builds on workspace-model.md (§1 decisions, §10 shells),
components.md and layout.md.

## 1. Decisions

- **A shell is a view** of the person's arrangement and the workspace: resources, tasks,
  presence, notifications. It is chosen per person and device class, and switching keeps the
  arrangement.
- **Rust keeps the mechanics, tilolang the structure.** The compositor, input, rendering and
  the UI host are Rust; so are the hot interactions, as described components: window drag and
  resize, strip scrolling, canvas pan and zoom, gestures at native refresh. Each commits one
  arranging action when its gesture ends. Which parts exist and where they sit is tilolang.
- **Apps draw no chrome, colours or fonts.** Frames, menus, bars and their look belong to the
  person's shell and theme, so every shell and theme reaches every app.
- **Only the person arranges.** Opening, closing and moving happen from their input or a view
  acting on it, never from a task.
- **The system shows what tasks did.** What a person sees of an agent's or script's work never
  depends on the agent (§5).
- **Trust stays system-drawn.** Asks, the trusted sheet, sign-in and lock, and a key that
  always reaches the system are drawn above any shell view. A shell view never sees secrets,
  never draws an ask and can't take input the system layer claims.

## 2. Arrangement

Each shell declares its arrangement type, in its own terms:

```
type Strips { strips: List<Strip> }            type Strip { places: List<Place>, width: … }
type Canvas { places: Map<Place, Point> }      type Place { shows: Resource, view: View, state: … }
```

- It lives in the person's own record (`records: .own`), synced across their devices that use
  the same shell, with history and undo. Nothing about it is shared with other members.
- A shell that doesn't know a place a different shell made ignores it and leaves it alone.
- Today's arrangement in `tilo_desktop_model` (shared, on the workspace) moves here; the
  migration keeps every window, strip and canvas place.

## 3. References, not opening

A resource appears in other places as a **reference**: in a chat message, an agent's answer, a
list row, a search result. A reference is drawn with a view of its resource and can be
activated. Nothing in an app or an agent "opens" anything; activating a reference is the
person's input, and their shell decides what it does:

| Shell | Activating a reference |
|---|---|
| strips | a column after the one it came from |
| windows | a new frame, the same frame split, or a tab, by the person's setting |
| canvas | a place next to where it came from |
| phone | a page, with Back |
| conversation | an embedded view in the transcript |

So apps never decide layout. A list's rows are references to its items; whether list and item
sit side by side, in one frame or on two pages is the shell's and the person's. "One reason,
one window" is the strips shell's policy, not a rule for apps. Someone sending a resource in
chat and an agent handing one over are the same thing: a reference.

Activating something already shown goes to it. A shell may offer "another view" for two views
of one resource side by side, each with its own window state.

## 4. Windows

In shells that have them, a frame is the shell's view of a place:

```
view frame_classic(p: Place) {
    stack {
        row { ui.Icon(p.shows); ui.Text(title(p.shows)); spacer; frame_buttons(p) }
        ui.MenuBar(actions(p.shows))
        p.content
    }
}
```

- Title, icon, menus and presence come from the resource and its type, never from app code.
- Apps give a windowing shell their size preferences (layout.md), a title field and an icon,
  views that work at any size (`layout-check`), and everything they can do as actions.
- Foreign surfaces (Xwayland, web apps, legacy native apps) are framed the same way. Their
  content keeps its own look; their decorations are suppressed where the protocol allows.
- Shared resource, personal frames: two people on one document each see it in their own frame,
  size and place; what is shared is the document and their presence on it. Following someone
  opens what they're on, in the follower's shell.

## 5. What a person sees of a task

An agent's or script's changes are recorded with their task: what was created, changed and
deleted, in which steps. The system presents them from that record:

- **Always.** Whatever the agent answers or forgets, its effects are shown; an agent can neither
  hide its work nor fail to show it.
- **As the person chooses**, by default and per task, switchable while it runs:
  - **watching**: what the task creates appears near where they asked, placed by their shell
    under their setting; things already shown that it changes are marked; its presence moves
    to what it's working on (e.g. on the canvas);
  - **contained**: nothing in their arrangement changes; the task's log shows each step with
    embedded, live views of what it touched (e.g. in a chat log);
  - on a phone, a notification whose activation shows the result.

  The agent does the same work either way and isn't told which.
- **Marks in place.** Views showing a changed resource mark the change (new, changed, removed,
  by whom), from the record, the same for people, agents, scripts and peers.
- The agent's own answer (agents.md, agent-ux.md) is commentary on top, never the only way to
  see its work.

## 6. Shells as packages

A shell is a package of views: the root view of the arrangement, its frame and parts. Defaults
ship: strips, canvas, a dock with a top bar, phone, a single conversation. Anyone may write
another, and an agent can generate one like any view. A person picks one per device class in
Settings. Themes change how parts look, never which parts exist.

## 7. Performance

A shell view re-runs only on discrete changes (a place opened, focus moved, a notification);
motion stays in the Rust components. Idle means no work. The shell's views are held to the same
budgets and `layout-check` as apps.

## 8. Order

1. Components out of today's Rust shell, with no behaviour change (in progress).
2. System resources and actions a shell view can read and use: tasks, presence, notifications,
   devices, power, network; open, close, focus, move.
3. The arrangement in the person's own record, migrated.
4. The strips shell rewritten in tilolang, matching today's, gated by the shell's tests and
   budgets.
5. Canvas, dock with top bar, phone and conversation shells; a live switcher.
6. Task presentation settings (§5).

## 9. Open

- Whether a person can give one app or type a different frame (a minimal frame around a video
  editor in an otherwise classic desktop): mechanically a per-type frame rule, so cheap; the
  question is whether it fragments what "my desktop" means.
