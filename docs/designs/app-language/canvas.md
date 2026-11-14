# The canvas: point somewhere and say what you want there

The canvas is one 2D surface the person works on directly. There are no windows and no fixed
applications on it: what stands there is content, the views of resources (an inbox, a note, a
list of tasks, a channel's last messages, an answer), placed by the person or by their agent.
Its one gesture is **point somewhere and say what you want there.** It is the desk style
`so.tilo.shell:frames` (`apps/tl/shell/canvas.tl`) of the tilolang shell (shell.md §6, §8).

## 1. Pointing

| Pointing | What the words carry | What the answer fills |
|---|---|---|
| **Click free space** | the point, and what stands near it | content standing at the point, at its own size |
| **Drag a rectangle on free space** | the rectangle | that rectangle: the content is laid out to it |
| **Right-click a thing** | the thing: a place, a resource in it, a field | beside it, or the thing itself when the words change it |
| **Select things** (drag a rectangle over them, or shift-click) | the selection | beside the selection |

A caret appears where the person pointed and the first keystroke goes there: no button, no mode,
no dialog. The rectangle being dragged shows as a light outline; on release the caret stands at
its top left, inside it. Escape, or pointing elsewhere while nothing is typed, takes the caret
away. Enter sends. The words go to the person's agent with what was pointed at; the session
answers a plain find itself (`session.place_found`): "my mails" puts the inbox there with no
model call.

Right-click keeps its menu: typing filters the menu's entries the way the palette filters
actions, Enter on a match runs it, and words that match nothing ask the agent about that thing.

## 2. Content, not windows

A place on the canvas is a resource's view, drawn as itself:

- **No frame.** No title bar, no window border. A place is its view on the canvas ground, with
  the theme's surface role behind it (rounded, a soft edge, the theme's elevation), so the
  canvas reads as one surface with things on it, not a desktop of windows.
- **Controls when they matter.** Hovering or selecting a place shows a small handle: its title,
  the resource's own actions (the same list the agent sees), pin and close. Drag the handle to
  move it; drag an edge or corner to resize. Touch: tap selects, which shows the same handle.
- **Views at their size.** A place is laid out at its own size and drawn through the camera, so
  zooming out shows the same layout smaller, never a squeezed one. Opening something never
  changes the person's zoom; it pans when the new content would stand off-screen.
- **A foreign window** (a client app with its own surface) is the one thing that keeps a frame,
  because the shell cannot compose it.

## 3. Answers stand where they were asked

- The agent's reply is drawn on the canvas at the caret, not in a chat window: the person's words
  as a quiet line, the agent's reply under it, its steps folded under that. This is a view of
  the turn (the conversation's message pair), placed like any content. The full conversation is
  still the Chat channel underneath (shell.md §8.1) and opens as content when asked for.
- What the agent shows is content too: the views it answers with fill the rectangle the person
  dragged, or stand beside the reply when they only clicked. A view the agent generates
  (tilolang it writes) is placed the same way and stays live.
- Beside means adjacent: what an answer shows takes the free spot touching its turn (trailing
  side, then below, then leading side, then above), the one most inside what the person sees,
  made smaller when only a smaller room is free there, and goes elsewhere only when no spot
  adjacent to the turn is free.
- Following up is pointing again: click under a reply to continue that turn, right-click an
  answer to refine it in place.
- Nothing modal: an answer in progress shows where it will land and never blocks the rest of
  the canvas. Nothing already standing moves to make room.

## 4. The agent has the whole session

The agent acts through the same published actions the person's gestures and the REPL run:
placing, moving, resizing and closing content (`shell:layout`), finding resources, every
app's actions on its resources, and the desk style itself. It changes how the canvas works by
writing the person's own style package (shell.md §6, "Make it mine"): its views are tilolang the
agent edits, checks and installs, and the desk switches to it. Every step is attributed and
undoable.

## 5. Looks and scale

The canvas ground, a place's surface, the caret, the rectangle's outline and the handle are
theme roles; a view never names a look. The canvas draws at the output's scale factor (a
windowed development session takes the host output's scale, fractional included), so text is
sharp at the screen's real size.

## 6. In code

- **Pointing** is `ui.Map`'s: `press` reports a click on free space with its canvas point,
  `frame` reports a dragged rectangle on free space in canvas coordinates, and `context` reports
  a right-click with the place under it. A press on a place goes to the place, never the map.
  Pan is a middle drag, two fingers or the arrow keys; zoom is ctrl+wheel or a pinch.
- **The caret** is the canvas view's state: the pointed point or rectangle, and a `ui.MapPin`
  there holding the input. Submit calls
  `session.place_found(text:, view: .none, strip: .none, x:, y:, width:, height:)`. A find
  fills the rectangle with the found view; a question posts to the person's agent conversation
  and places the turn's view at the point, recording the rectangle as the turn's anchor so the
  agent's `place` steps fill it (`beside`/`within` from the turn, as `open` takes `beside`
  today). The map reports what the person sees as they point (`seen`), and the caret passes it
  on (`seen_x`, `seen_y`, `seen_width`, `seen_height`): the turn's steps that stand beside it
  take the free spot adjacent to it, judged against what the person sees (`free_place_within`).
- **Places** are `ui.MapPin`s holding the resource's view with no `ui.WindowFrame`; the handle
  is a shell view shown on hover and selection.
- **The arrangement** records each place's resource, view, point and size (`ArrangementPlace`);
  placing never moves what is there (`free_place_near`, `free_place_next_to`). What is found at
  a click stands with its top-left at the click, narrowed (to a window's least width) to the free
  room to its right before the next place or the view's edge (`free_width_at`); only when not
  even that fits does it take the nearest free spot. A place put with no size of its own, at a
  click or by the agent beside its turn, hugs what it shows (`CanvasPlace::hugs`); a rectangle's
  room keeps its size.

## 7. The bar it is held to

What a person sees is judged against these, in shots at the device's real scale, before anything
is shown to anyone.

- **The thing, never the app.** Words resolve to the most specific resource they name, and the
  canvas places that resource's own view: "my mails" is the inbox's message list (Mail's
  `inbox` of the person's mailboxes, unified), never `mail(m: Mail)` with its account sidebar;
  "my notes" is the list of notes; "tasks due this week" is those tasks. An app's root view (the
  whole app) is placed only when the words name the app ("open Mail"). A view placed on the
  canvas carries no app chrome of its own: no sidebar, no app toolbar, no "File" menu. Those are
  the resource's actions, reached through the handle and right-click. An app whose resources have
  only app-shaped views gets content views for them (a notes list, a task list for a set of
  tasks).
- **The canvas is the whole screen.** In this style there is no dock and no desktop chrome: the
  bar keeps only what a person needs on any screen (the time, notices, the person). The desk
  style is chosen in the style menu and Settings, not in a control that is always visible.
- **Free space invites writing.** The pointer over free space is a text cursor. An empty canvas
  says once, quietly, at its centre: "Click anywhere and say what you want". Gone after the first
  ask. The ground is the theme's canvas role, with a grain or grid so faint it only says "surface".
- **An answer reads like an answer.** The person's words are a quiet line; the reply is prose at
  body size, set like text, not like a panel; the steps are one quiet line ("Read 3 mails") that
  unfolds; the conversation is reached from the handle. While working, the line says what is
  happening ("Reading your inbox…"), not a step count. The card hugs what it holds.
- **Things arrive from where they were asked.** A place grows out of the caret or rectangle it
  answers (the theme's motion), never pops. Pan and zoom follow the hand at the display's refresh
  rate.
- **Everything stays in view.** A new place, turn or answer never stands partly off-screen: the
  canvas pans (never zooms) so the newest turn and its answer are in view together, and keeps the
  turn in view as its words grow. The person's turn wins over the agent's later placement: the
  camera never chases a place the agent puts off-screen at the cost of the turn. The agent's views
  stand adjacent to their turn (§3), sized so the turn and they fit on the screen together where
  they can, and the camera pans (never zooms) to show both; when both cannot fit, the turn wins.
  Panned to them, the camera frames the turn and its views together: centred top to bottom when
  they fit, else from their top, never hanging at an edge with the screen empty opposite.
  A card hugs what it holds up to the visible area and no further:
  its text wraps and the card scrolls.
- **Captions name the thing.** A place's caption is what it shows, by the name of the view it
  stands through ("All inboxes", "Notes"), never the first record or account of its app's root.
  A place under another keeps its caption clear of it: they stand the caption's band and the
  canvas's gap apart.
- **Places hug what they show.** A list found at a click is as tall as its rows up to its room:
  a scroll region says what its content comes to when it is asked, so no card stands with an
  empty foot.
