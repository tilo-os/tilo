# The canvas: point at something and start typing

The canvas is a 2D surface of windows of views (workspace-model.md: workspace, window, window
state, lens, strip, app). There are no fixed applications on it: what's there is what the person
or an agent placed. Its one gesture is **point at something and start typing.**

## 1. Three ways to point

| Pointing | Context the typing carries | Where the answer lands |
|---|---|---|
| **Free space:** place the cursor anywhere empty and type | the position, and what's near it | at that position |
| **Right-click anything** and type | that thing: the window, the view, the resource under the pointer, the field | beside it |
| **Select (drag to select) anything** and type | the selection: several windows, resources in a list, text, a region | beside the selection |

Typing starts immediately: no "Ask" button, no mode switch. A caret appears where the person
pointed, and the first keystroke goes there.

## 2. Right-click keeps the menu

Right-click opens the normal context menu, with its entries as today. Typing filters those
entries the way the palette filters actions (one model: the context menu is the palette scoped to
what was clicked). Enter on a matching entry runs it; when nothing matches, or the person keeps
typing a sentence, it becomes an ask to the agent with that context. So "rename" renames, and
"make a table of these by due date" asks.

## 3. Answers

- An answer is a window of composed views (first-experience.md), placed where the person
  pointed and drawn by the UI host like any window. Text answers are a view too.
- It's live: the view is generated tilolang and updates as the resources change.
- It's kept: the person can keep it in their own app and reuse it, or discard it.
- Follow-up typing on an answer (right-click it, or select within it) refines it in place.
- Actions the answer proposes show as suggestions with previews; asks still ask; one undo per
  agent step (agents.md).

## 4. Rules

- **Nothing modal.** The caret and an answer in progress never block the rest of the canvas.
- **Stable layout.** An answer arriving doesn't move what's already there; it takes free space
  near the anchor.
- **Touch parity.** Tap empty space puts the caret there and raises the keyboard; long-press is
  right-click; drag to select works with a finger.
- **Native refresh rate** while typing, panning and zooming.

## 5. What it builds on

- Context menus and the palette's filtering (one model, scoped).
- Tilo's existing agent stack for model calls, tools, asks and Tasks.
  No new LLM plumbing.
- The `tilo` tool and agent programs for what the agent does (agents.md).
- The UI host for drawing many apps' windows on one surface (ui-host.md, step 3).

## 6. In code

- **The arrangement** is the workspace's (`tilo_desktop_model::arrangement`): its kind (strips or
  canvas) and each window group's place on the canvas, in whole canvas pixels. Groups stay in their
  strips; the canvas only adds where they stand. Commands `SetArrangement` and `PlaceGroup`
  change it. The room's layout record carries the kind on the workspace entry and each place on
  its group entry, so two people moving different windows both keep their move; the desktop
  document records it for history and undo. Moving someone else's window on the canvas is a move
  for the moderation rule, as moving it along a strip is.
- **Placing** never moves what is there: a new window takes the free space nearest its anchor
  (`Arrangement::free_place_near`, `free_place_beside`).
- **Drawing:** `GroupRow` lays its windows out at their places through a camera (pan and zoom);
  `StripDeck` lays every strip's row over the whole deck while the canvas shows. A client window
  keeps its canvas size and is drawn scaled when zoomed. Pan and zoom are each person's lens, kept
  on their device.
- **Pointing and answers** are the shell's canvas layer (`overlay_host/canvas.rs`). Every ask goes
  through the existing composer's agent path as a Tasks issue the agent works on, and its page
  opens as a window at the anchor. `CanvasAnswers` pairs an answer's window with its anchor; an
  answer that is a view window places itself through the same pairing.
- Shown with the actions "Show the canvas" and "Show the strips" (`workspace.show-canvas`,
  `workspace.show-strips`).
