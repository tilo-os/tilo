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

A shell's own data is derived, not saved. What the session owns and can make again at start (the
clock, the desk, the device's state, the session origin's resources) is written into the store's
unsaved layer, never into a collaborative record: a change of one reaches the views at once, and
leaves no change to be committed, synced or replayed. A minute therefore costs a view run and a
draw, not a record round-trip. Data that is the person's (their arrangement, their settings)
stays a saved record, synced and undoable like any app's.

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
- **Where the window goes.** A shell view never draws a place's content: it puts a well where
  the content goes, and the app's own window shows there.

  ```
  view place_window(p: ArrangementPlace) {
      stack {
          frame_bar(p)                 // the shell's: title, tabs, actions, presence
          ui.WindowWell(place: p.key)  // the app's window, at this rectangle
      }
  }
  ```

  `ui.WindowWell` is a described Rust component. It draws nothing and takes no input, because
  the window is composited above the desk layer. After each layout, and on each frame that a
  component around it moves it (`ui.Map`'s pan and zoom, a strip row's scroll), it reports its
  rectangle in layout coordinates to the host's `Wells`, keyed by place. The shell's views run in
  the compositor's process (`tilo-surface-host`), so the surface host and the policy share one
  `Wells` handle, as `DesktopHost` and `StripPolicy` share `Desk` today.

  The policy for a tilolang shell answers `initial_size`, `place` and `arrange` from `Wells`, as
  `StripPolicy` answers them from `Desk::shown`. A well that moves without changing size moves
  its window in the same frame. A size change configures the client, and the window catches up
  on its next commit, as it does today. A place without a well on screen (scrolled away, or on
  another workspace) leaves its window unmapped from the output, so it gets no frame callbacks.

  A place's key is the tab id the desktop model gives the window when it maps
  (`DesktopPolicy::map`), so a well and its window meet by that id. A window the arrangement
  has no place for gets one through the arranging action, never a well of its own.

- **A place shows a resource through one of its views, not an app.** `ArrangementPlace`
  already has `record` and `shown_as` (`tilo_desktop_model::own_arrangement`'s `view`), but it
  starts from `shows`, an app id. A place made from a resource fills `record` with the
  resource and `shown_as` with a view id from its published views (`ResourceSpec::views`,
  primary when empty), and `shows` follows from the resource's app. Placing one invokes the
  resource's `open` with that view. The app's host opens a surface with just that view. The well positions it, and
  the shell draws whatever frame it likes, or none (`spaces_canvas`). "Mail" is never placed.
  A mailbox's list, one message and the folder sidebar are three places, each its own view of
  its own resource, so a person or an agent can lay out any of them anywhere. A tilolang app
  window is already exactly this (apps draw no chrome); what changes is that the place is
  made from a resource and view, not from launching an app.
- **Finding what to place.** Text typed on the canvas ("my mails", "videos of the Rust
  conference") is a `find` over the system index and the apps' search providers, the same
  one the palette and the `tilo` tool use. The top result's primary view is placed at the
  caret. Results that are a service's own (a video site's channel page) are resources of the
  app that connects to it, so "videos of X" places that channel's page view from the YouTube
  client, not the client.

## 8.1 The conversation

The agent chat and the REPL are one view, `conversation(r: Run)`, over a run of the session's
agent (`tilo_agent_runs`). It is a shell view like any other: it can be a place on the canvas,
a panel beside the strips or the whole screen of the conversation shell.

- **One input, two ways in.** A line that parses as a tilolang program runs as it is: one
  `tilo` call on the run's door session, attributed to the person, with the same `let` scope
  as the agent's own calls. Anything else is a sentence to the agent, which answers with
  `tilo` calls of its own. So the person types exactly what the agent would. The terminal
  `tilolang repl` is the same session reached from outside.
- **The transcript is steps.** Each entry is a `ProgramStep`, whoever wrote it: the program,
  highlighted; the changes it made, as undo rows; and what it printed, as references.
  The agent's prose sits between steps. Nothing is drawn from the agent's text that a step
  does not back.
- **References place.** A reference in a step is drawn with its type's row view (search.md).
  Activating it is §3: on the canvas, a place next to the conversation; in strips, a column
  after it; in the conversation shell, embedded in the transcript. What the agent wants to
  show is a reference it printed. It reaches the screen by §5's "What opens" setting: by
  default what it creates, plus its answer.
- **Looks.** The conversation uses the shared components (`ui.Composer`, `ui.CodeBlock`,
  `ui.ListRow`) and theme roles only, so every theme reaches it.

## 8.2 Steps

Each step deletes the Rust part it replaces once the tilolang one passes that part's tests,
screenshots and frame budgets.

1. *(landed; off by default)* The tilolang surface host and system resources, with the top
   bar as a tilolang view. Before `shell_bar` is on by default, it still needs a bar per
   display and to step aside for a fullscreen window.
2. Places show resources (§8, first point above): `shows` and `view` on a place, `open` with
   a view, and the canvas placing from `find`.
3. The conversation (§8.1) as a canvas place, beside the REPL.
4. The canvas shell as a whole: `WellPolicy` for canvas places, point and type (canvas.md),
   with the conversation and placed views.
5. Window frames and tabs as shell views with per-type rules; the traditional desktop (dock,
   top bar, the focused window's actions in the top bar, window bars with the window's own
   actions); overlays, quick settings and notifications as views.
6. Strips in tilolang over `tilo-desktop-policy`; shell packages a person can replace and
   switch in Settings; the §5 settings.
7. Looks: theme finishes beyond block, default and glass (pixelated, shader), hover and press
   responses, and fonts, icons and sizes. All of these are theme roles, so a view never names
   a look.

## 9. Open

- Whether a person can give one app or type a different frame (a minimal frame around a video
  editor in an otherwise classic desktop): mechanically a per-type frame rule, so cheap; the
  question is whether it fragments what "my desktop" means.
