# The shell

A shell presents the workspace on one device for one person. It is a tilolang view, so people
choose, change or write their own. Builds on workspace-model.md (§1 decisions, §10 shells),
components.md and layout.md.

## 1. Decisions

- **A shell is a view** of the workspace's arrangement and resources: resources, tasks,
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
of one resource side by side, each with its own view state.

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

A desk style is a tilolang view of the person's arrangement, and nothing more. A person, or an
agent working for them, makes a new one the way they make any app: write a package, install
it, pick it. The four styles that ship (strips, frames, spaces, desktop) are written exactly
that way, and are the examples.

**What makes a view a desk style.** Its first parameter is the session's arrangement, the type
whose origin is `session.arrangement`:

```tilolang
use so.tilo.session as session
use so.tilo.shell as shell        // the shipped parts: bar, dock, notices, quick settings

/// My agents' work as a timeline, today's newest first.
view timeline(a: shell.Arrangement) {
    shell.bar(shell.desk)
    ui.List(a.places.newest(), |p| ui.Card(p.title) { place_view(p) })
}
```

There is no list of styles in code and no keyword. The type is the contract, as it is for every
view (one-app-model.md §2).

**Where styles come from.** Any installed tilolang package may declare desk styles. The session
reads installed packages from the person's package store the way it reads every tilolang app:
`package_store::Catalog`, then `Bundle::read(payload_root)` and `Host::load_bundle`. That is
`seeded_apps.rs`'s path, made the only path.
- The shell package `so.tilo.shell` is installed in the image's store like any app.
- `compile_shell()`'s built-in copy (`include_str!` of `apps/tl/shell`) stays only as the
  fallback described below. It is not the way the shell normally loads.

**Picking a style.** The person's `Viewing` record (`arrangement.tl`, `records: .own`) holds
the style as a view reference: package and view name. It is not the closed `Preset` enum,
which goes away.
- The style switch in the bar and the Desk page of Settings list every desk style of every
  installed package, by the view's doc line.
- The shell part's screen layer (`screen_layer()`, today fixed to `so.tilo.shell`'s
  `desk_view`) shows the referenced view.
- Switching keeps every place where it was, because the arrangement is the session's and the
  same in every style.

**What a style can do.** Read the session's resources (arrangement, places, windows, desk,
notices, people, the device) and call their actions, exactly as the shipped styles do. It can
show any resource's view in a place (`place_view`). It never draws trusted surfaces: the sheet,
asks, sign-in and lock stay the system's (§1). Looks stay the theme's: a style names roles, never
colours, so every theme reaches it.

**Two ways to change a desk.**
- **Without an agent.** A person picks a shipped style and adjusts it in Settings. The adjustments
  are the style's own actions on its own record: today `so.tilo.shell`'s `Shell` type in
  `chrome.tl` (`bar_on`, `set_bar_size`, `dock_on`, `set_dock_zoom`, `move_part`, `reset`, …).
  The Desk page of Settings shows the chosen style's settings view, the same way it shows any
  app's. A style an agent writes gets settings the same way: by declaring actions on its own
  record. Nothing in Settings is per style.
- **With an agent, in conversation.** The person asks the canvas chat for a change ("put the
  calendar where the dock is", "a desk that shows what happened while I was away"). The agent
  composes `so.tilo.shell`'s exported parts (bar, dock, notices, quick settings, places, window
  frames), or writes new views, and installs the result as described below.
  - **Make it mine** turns a shipped style into the person's own package: a copy of its source
    in their workspace, installed under their own package id, with the same parts.
  - The agent edits that copy. Updates to the shipped style never overwrite it.

**Agents make styles with the same commands a person types.**
1. The agent writes the package's files into the person's workspace.
2. It checks them: `tilolang check`, also available as a session action.
3. It installs them: the store's install action, which compiles, signs with the person's
   development key, and asks the person on the trusted sheet, because installing code is the
   person's decision.
4. It switches: `shell.show_style(v, style)`.
Every step is an action in the broker's catalogue, so the REPL does the same.

**Nothing can lock a person out.** If the chosen style fails to load, fails to compile against
the installed shell and session packages, or traps on its first frame:
- the session shows the image's built-in `so.tilo.shell` default;
- it raises a notice naming the style and the error;
- it keeps the person's choice, so a fixed reinstall comes back by itself.
A reinstall re-renders the style live; the session does not restart.

**Steps**
1. The shell loads from the store. `so.tilo.shell` is installed in the image, dev shells install
   it as they install the tilolang apps, and the built-in copy becomes the fallback.
2. Styles are views. `Viewing.showing` becomes a view reference, and the switch and Settings
   list desk styles from every installed package. The four presets become views found that way.
3. The fallback and the notice, each with a test where the style fails to compile and one where
   it traps.
4. Agent authoring, end to end: a test in which a scripted agent writes, checks and installs a
   style package and switches to it, and the desk draws it. Then the same live with a real model.

Themes change how parts look, never which parts exist. Styles change which parts exist and where.

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
- **Where a window goes.** For a surface it can't compose (a foreign or legacy window), a shell
  view puts a well where the content goes, and the app's own window shows there. A tilolang
  resource is composed as a view instead (below).

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

- **The canvas composes views; it opens nothing.** The four layers stay apart:
  - *Resource:* a mailbox, a message, a channel page. The workspace's arrangement is a resource
    too: which resources are open, through which view, where.
  - *Action:* putting the mailbox on the canvas as its `inbox` view at a point is an action on
    the **arrangement** (`shell:arrangement`, `arranging.tl`), never on the mailbox. Archiving
    a message is an action on the message. A person, an agent and the REPL show things the same
    way, attributed and undoable. No resource carries an action that shows it.
  - *View:* the canvas is a view over the arrangement. For each entry it calls the entry's view
    of its resource, the same views the app's own default view is made of: `mail.inbox(m)`,
    `mail.message(msg)`, `yt.channel(c)`. "Mail" is never placed. A mailbox's list, one message
    and the folder sidebar are three entries, laid out by the person or an agent. A composed
    view's buttons press its own app's actions, as child invocations carrying that view's
    author (`tilolang_runtime::foreign`).
  - *Theme:* an entry names a resource and a view id, never a look. An arrangement, or an
    agent's summary made of views, renders in each viewer's own theme.

  The mechanism: the shell's runtime runs another tilolang package's views inline, under that
  package's authority, reading that app's live readings. The host loads the other app's
  package with its module (`Runtime::load_foreign_package`): its views run in the shell's
  runtime as that package's code, so what they invoke carries its module as the author; its
  records stay its app's, read from the readings the session publishes (`Runtime::set_reading`,
  `tilolang_runtime::foreign`), and its actions are child invocations at its door. A view that
  `use`s the app calls its views by name (`mail.inbox(m)`, a `view:` node). The canvas, which
  can't `use` every app, finds the resource by the id the arrangement keeps and names the view
  as text:

  ```
  view place_view(p: ArrangementPlace) {
      if let r = placed_resource(p.record) {        // `resource(id)`: its type from where it is held
          if p.shown_as != "" or meta(r).opens {
              show(r, view: p.shown_as)               // that view, or its type's default
          } else {
              ui.WindowWell(place: p.key, min: 12rem)
          }
      } else {
          ui.WindowWell(place: p.key, min: 12rem)   // a window: nothing here to draw
      }
  }
  ```

  `resource(id)` finds the resource's type where it is held: this runtime's store, or the
  reading another app published of it (until that reading arrives it finds nothing, and the
  view runs again when it does). `show(r, view:)` draws it through the offered view of its
  package the place names (by id or name, taking the resource alone), or its type's default
  view. An entry made from a resource fills `record` and `shown_as`. `ui.WindowWell` stays only
  for surfaces that can't be composed (Xwayland, legacy native apps). `OpenEffect::Open` on
  every resource goes away once nothing needs it.

  Still to build: the surface host loading each installed tilolang package this way and
  following the readings of the resources its places name.
- **Finding what to place.** Text typed on the canvas ("my mails", "videos of the Rust
  conference") is a `find` over the system index and the apps' search providers, the same
  one the palette and the `tilo` tool use. The top result's primary view is placed at the
  caret. Results that are a service's own (a video site's channel page) are resources of the
  app that connects to it, so "videos of X" places that channel's page view from the YouTube
  client, not the client.

## 8.1 The conversation

The agent chat is a Chat conversation, not a shell part. An agent's work is the turns of the
conversations it takes part in (`crates/session/session/src/conversations.rs`): a person's
message the agent has not answered is a turn, and the kept agent's process (`so.tilo.agent`,
started per workspace by `Apps::start_agent`) answers it in the same channel. The shell owns
no run and no transcript; it places the channel like any resource.

- **Asking from the canvas.** The canvas entry reads "Find something, or ask". Enter calls
  `session.place_found(text, x, y)`. When the text is a question (`answers::question::asks`)
  or nothing matches it, the session asks instead of failing: it posts `text` as the person
  into their ask conversation in the workspace (a Chat channel the agent is a member of,
  made with Chat's own actions on the first ask, and the workspace's kept agent kept with
  `WorkspaceAgents` when it has none), then places that channel at (x, y), or brings its
  place forward when it already has one. Nothing here is new machinery: Chat's actions post
  and create, and the arrangement's `place_at` places.
- **The view.** The channel is placed with Chat's default channel view, a `#[view]` over
  Chat's channel type (one-app-model.md §2), drawn by the host from described components:
  messages, the composer, and each attachment drawn with its type's row view. The same view
  is what Chat's own window shows; the canvas gets no second chat.
- **Any action.** The agent's tools are the broker's catalogue. The session publishes its
  own actions there too (the arrangement's `place`, `place_at`, `open`; windows; notices),
  so an agent answering a person's turn can arrange the screen exactly as the REPL can. Each
  call is authorized per step against the turn's grants, attributed to the agent and the
  person who asked, and undone like any change.
- **What the agent shows.** What the agent wants to show is a resource it attaches to its
  reply (a message carries attachments). Pressing an attachment places it beside the
  conversation (§3). What opens unasked follows §5's "What opens" setting: by default what
  the turn created, and a view it answered with (`plan.view`), placed beside the
  conversation with `place_near`.
- **The REPL.** `tilolang repl` reaches the person's door and calls the same actions by the
  same names the agent's `invoke` does, so the person types what the agent would. Its
  results are references that place the same way.
- **Looks.** Only shared components and theme roles, so every theme reaches the chat.

## 8.2 Steps

The tilolang shell is developed beside the Rust shell, not in place of it. No step removes or
reshapes a Rust shell or session part; where the session's published resources change, the
tilolang shell follows them.

1. *(landed; `shell_bar` off by default, bar only)* The tilolang surface host and system resources, with
   the top bar as a tilolang view: one bar per display, following hotplug, each stepping aside
   on a display a fullscreen window owns (`Compositor::fullscreen_outputs`).
2. The canvas composes views (§8, first point above): a package's views run inline in the
   shell's runtime, arrangement entries name a resource and a view, and the canvas places them
   from `find`.
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

## 8.3 One model

The shell is held to AGENTS.md's first principle: one type per concept and one declaration per
action, owned by whoever owns the state. This section is the working model for the tilolang
shell's code. Its arrangement and workspace parts are not settled (§9): they wait on what people
learn using a shared arrangement in the Rust shell.

### The session's resources

The session publishes what the shell reads as resources, one type per concept. Each type
carries its own actions:

| Type | What it is | Its actions |
|---|---|---|
| `Workspace` | a workspace on this device | `switch`, `create`, `join`, `rename` |
| `Window` | a window an app mapped (its id is the window's tab id) | `focus`, `close`, `move_to(workspace)` |
| `Notice` | a notification | `act(choice)`, `dismiss`; `clear_all` on the centre |
| `Person` | someone here | `follow`, `message`, `call`, `see_work` |
| `Setting` | a device setting quick settings shows | `set(value)` |
| `Surface` | the system surface over the desk (launcher, overview, search) | `show`, `close` |
| `Clock`, `TrayItem`, `Task` | the time, a tray item, a running task | their own, if any |

- The session describes these the way a native app describes its own (`surface.toml`, generated
  from what it publishes and checked for drift). A shell imports them with `use so.tilo.session
  as session` and calls the actions directly: `w.focus()`, `n.dismiss()`,
  `session.surfaces.launcher.show()`. Nothing in the shell turns a string into an action, and the
  shell declares no wrapper action around one of these.
- No reading bundles them. `Desk` (the old chrome's every input in one record) and the
  `desktop:desk`, `desktop:chrome` and `desktop:tab:N` addresses are replaced by the types above.
  `Room` and `ArrangementWorkspace` are replaced by `Workspace`, and `Win` by `Window`.
- Values the session derives (the time, which surface is up, whether the pointer is at an edge)
  stay unsaved (§7).

### The arrangement belongs to each shell

Each layout is a shell package with its own arrangement type in the person's own record
(`records: .own`), as §2 says. A place is a resource and a view, and never an app. The app follows
from the resource, and a foreign window's place names the `Window`.

```
type Place { resource: Text, view: Text = "" }              // shared by every shell (parts package)

// so.tilo.shell.strips
type Strips  { rows: List<Row> }
type Row     { name: Text, strips: List<Strip> }
type Strip   { places: List<Place>, width: Width = .normal, active: Int = 0 }

// so.tilo.shell.desktop
type Desktop { frames: List<Frame> }                         // list order is stacking order
type Frame   { place: Place, x: Int, y: Int, width: Int, height: Int }

// so.tilo.shell.canvas
type Canvas  { pins: List<Pin> }
type Pin     { place: Place, x: Int, y: Int, width: Int, height: Int }
```

- **Rust never reads these types.** The shell's view lays each place out. A place showing a
  resource draws it with `show(r, view:)`; a place showing a foreign window draws a
  `ui.WindowWell`, which reports its rectangle to `Wells`, and `WellPolicy` puts the real window
  there. This is how every layout places real windows: the strips' scrolling, the desktop's
  frames and the canvas's pan and zoom are all the view's layout.
- **Arranging is the shell's own actions on its own type**, declared once in its package:
  `place`, `move`, `close`, `resize`, `raise`. Each is attributed and undoable like any action on
  a record.
- **Every shell answers two actions under the same names**: `place(resource, view)` and
  `close(place)` on its arrangement root. The checker enforces this for a package of category
  `.shell`. The session publishes the person's active shell's arrangement root as
  `shell:arrangement`, so the agent, the REPL and a reference's activation call
  `arrangement.place(r, view:)` without knowing which shell is up. When an app maps a new
  window, the session calls `place` with that `Window`, so every window gets a place in whatever
  shell is active.
- Switching shells maps places across. Each shell has a `from(places: List<Place>)` that lays out
  the other shell's places in its own terms, so no place is lost and no id changes.
- The migration from `tilo_desktop_model`'s strips keeps every window, strip and canvas place and
  every id (the data invariants).

### Shells and their parts

- One package per layout: `so.tilo.shell.strips` (the scrolling WM with tabs),
  `so.tilo.shell.desktop` (the traditional desktop: frames, dock, the focused window's actions
  in the top bar) and `so.tilo.shell.canvas` (places with no fixed app, the conversation, typed
  `find`).
- All three use `so.tilo.shell.parts`: the bar, the dock, the window frame and its action bar,
  notices, quick settings, the launcher and `conversation(r)`. The person picks a shell per
  device in Settings, which is one setting naming a package. There is no `Preset` enum and no
  `if/else` over layouts.
- The person's bar and dock settings are structure only: which parts, their order, which edge,
  and when they show. How a bar or dock looks (floating or edge to edge, translucent or solid,
  its size) is the theme's, as roles `bar` and `dock`. A dock item is what the person pinned
  (a resource or an app). Whether it runs, its badge and whether it wants attention are read
  live from `Window` and `Notice`, never saved in the person's record.

## 8.4 Apps written in Rust on the canvas

A place shows a resource, whatever language its app is written in. There is one app model,
not a tilolang one and a Rust one with a bridge between them. A Rust app declares the same
things a tilolang package does: types (resources, with noun and app), actions, and named views
of a type that return described components the host draws. Its model and actions run where a
tilolang package's do, in the session's document host, over the same storage. The shell owns
windows and places for both. So a Rust app's channel page stands on the canvas exactly as
Mail's inbox does, with nothing the app does for it.

What a person sees: they type "youtube videos of mkbhd" on the canvas, and a frame stands where
they typed, showing that creator's page: their videos, their name, the subscribe action. There
is no Tube window or sidebar around it. Pressing a video places the video beside it.

The Rust SDK becomes another way to write that model. Its `ResourceApp`, `#[resource]` models,
`LiveRow` rows and per-app windows today differ from it in views, item resources, storage,
windows, search and permission checks. The SDK and every Rust app change once to the one
model. The design maps each piece onto the code: [one-app-model.md](one-app-model.md).

## 9. Open

- Shared or personal arrangement. A workspace whose arrangement everyone shares gives presence
  for free (cursors, viewports, following); a personal arrangement gives each person their own
  layout and makes presence something to rebuild per view. Which one, or which mix, is learned
  by using the shared arrangement in the Rust shell first.
- Canvas or scrolling strips as the native form of a shared workspace.
- What a workspace is: an audience and agent scope, a place people share, or a person's focus
  on one piece of work; and where things that are a person's own (music playing, device
  settings) stand while they work in a shared one.
- State that follows a person between devices: resources and their types everywhere; a
  person's position and progress in a resource (reading position, where they are in a chat),
  expressed as a reference into the resource so another view can show it; pending actions
  (a draft is a `send` not yet run, a half-typed search a `search` not yet run), kept while
  what they were typed into is open. Device classes that show different things are a person's
  setting.
- Sharing a view in a call: followers see the sharer's open things, layout, position and
  pointer in their own theme, pointers placed by the element under them; their own state is
  untouched and moving their view ends following.
- Agents' presence derived from their actions (the element an action targets, the range an
  edit changed), never from driving a pointer.

- Whether a person can give one app or type a different frame (a minimal frame around a video
  editor in an otherwise classic desktop): mechanically a per-type frame rule, so cheap; the
  question is whether it fragments what "my desktop" means.
