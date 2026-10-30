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
- **The person and their agents arrange.** Agents acting for a person open, place, close and
  rearrange like the person would; every change is attributed and undoable. How much of an
  agent's work reaches the screen is the person's setting (§5), not a rule. One rule is fixed,
  for input correctness: nothing moves or takes focus under an ongoing gesture or while the
  person types.
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

The record, marks and seen are task-legibility.md. How a task's work shows up is the person's
choice: settings in the one Settings place, overridable per task while it runs, changeable by
an agent acting for them, and anything beyond them by editing the shell's views.

| Setting | Options | Default |
|---|---|---|
| Where an agent's work goes | where the person works · a space per task (canvas cluster, strip, desktop) · a space per agent | a space per task, and its result where they asked |
| What opens | nothing (the task's log) · what it creates · everything it touches · follow its focus | what it creates, and its answer |
| The agent shown as | a collaborator (presence, member list, mentions) · a tool (results only) · per agent | a collaborator |
| Marks on changes | off · agents' · everyone's | agents' |
| Taking focus | whenever it brings something · when the person is idle · never | when idle |
| Moving what the person placed | yes · not what they pinned · never | not what they pinned |

The agent does the same work under any of these, and may read them to present its work well.

## 6. Shells as packages

A shell is a package of views: the root view of the arrangement, its frame and parts. Defaults
ship: strips, canvas, a dock with a top bar, phone, a single conversation. Anyone may write
another, and an agent can generate one like any view. A person picks one per device class in
Settings. Themes change how parts look, never which parts exist.

## 7. Performance

A shell view re-runs only on discrete changes (a place opened, focus moved, a notification);
motion stays in the Rust components. Idle means no work. The shell's views are held to the same
budgets and `layout-check` as apps.

## 8. On the native session

The native session (`crates/session/session`) is already a thin host: the compositor
(`tilo-compositor`) shows buffers and routes input and draws no interface; a `WindowPolicy`
decides placement and focus; surface apps reach the screen as `SurfaceHost` layers through
`tilo-surface-host`; the desk and chrome are the Rust widgets of `tilo-desktop`
(`desktop_host`, `topbar`, `overview`, `window_switcher`, …) over `tilo-desktop-model`, and
everything a person can do is a published action (`Session::resources`, `Session::invoke`).
The tilolang shell takes the place of `tilo-desktop`'s widgets, one part at a time, on the same
seams:

- **Shell views on layers.** A shell package's views run in the tilolang host like any app's
  and draw through the same UI host; a `SurfaceHost` shows each part (bar, desk, overlays) as
  layers, exactly as `tilo-surface-host` shows a toolkit app. Headless, nested and DRM work
  unchanged, because the compositor's backends do.
- **System resources for the shell.** What the chrome reads today through `chrome.rs` (clock,
  person, notifications), the desktop model (workspaces, strips, windows), tasks and presence
  become typed resources and actions a shell view reads and invokes through the door, the same
  ones agents use. A shell view never reaches Rust state any other way.
- **Arrangement through the policy.** A shell's arrangement type is a record of the person's.
  A `WindowPolicy` implementation per arrangement mechanic (scrolling strips: today's
  `tilo-desktop-policy`; canvas; floating desktop) reads it and places windows; gestures are
  Rust and commit one arranging action at their end.
- **Frames.** A window's frame (bar, tabs, actions) is a shell view drawn on the desk layer
  under the window, positioned from the policy's layout, as `tilo-desktop`'s window bars are
  today. Per-type and per-app frame rules are the shell's.

Order, each step deleting the Rust part it replaces once the tilolang one passes that part's
tests, screenshots and frame budgets:

1. The tilolang surface host and the system resources it needs; the top bar as a tilolang view
   (clock, workspaces, running tasks, notifications, the person).
2. Window frames and tabs as shell views, with per-type rules.
3. The canvas arrangement: a `WindowPolicy` for canvas places, pan and zoom in Rust, point and
   type (canvas.md) as shell views.
4. Overview, switcher, launcher, palette and the agent composer as views.
5. Strips in tilolang over `tilo-desktop-policy`; shell packages and switching them in Settings;
   the §5 settings.

## 9. Open

- Whether a person can give one app or type a different frame (a minimal frame around a video
  editor in an otherwise classic desktop): mechanically a per-type frame rule, so cheap; the
  question is whether it fragments what "my desktop" means.
