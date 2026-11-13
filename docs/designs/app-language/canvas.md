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
- Following up is pointing again: click under a reply to continue that turn, right-click an
  answer to refine it in place.
- Nothing modal: an answer in progress shows where it will land and never blocks the rest of
  the canvas. Nothing already standing moves to make room.

## 4. The agent has the whole session

The agent acts through the same published actions the person's gestures and the REPL run:
placing, moving, resizing and closing content (`shell:arrangement`), finding resources, every
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
  today).
- **Places** are `ui.MapPin`s holding the resource's view with no `ui.WindowFrame`; the handle
  is a shell view shown on hover and selection.
- **The arrangement** records each place's resource, view, point and size (`ArrangementPlace`);
  placing never moves what is there (`free_place_near`, `free_place_next_to`).
