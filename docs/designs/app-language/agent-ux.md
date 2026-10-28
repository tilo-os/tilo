# Tilo's agent: point, type, get a view

This improves the agent Tilo already has. Model calls stay in `tilo-model-gateway`; the Tasks
agent (`apps/agent`) keeps its loop, tools, budgets and Tasks issues; asks stay in
`action-policy`. What changes is how a person reaches it and what comes back. The canvas
(canvas.md) is the end state; everything here works on today's desktop first.

## 1. Point and type, everywhere

Today the way in is the quick composer (Super+C, or "Tell an agent…" in context menus), a
popover with pickers. The menus themselves take no typed input.

- **Every context menu takes typing.** A query field at the top filters its entries, with the
  ranking `filter_menu` already has (prefix, then substring). Enter on a match runs it. A
  sentence, or no match, turns the menu into the composer with what was clicked as the subject.
  The "Tell an agent…" row goes away: typing is the way.
- **Same for the window menu and desktop-thing menus** (today they offer only "Tell an
  agent…").
- **One subject rule.** Right-click on an item: that item. Right-click inside a selection: the
  selection. tilo-app rows and `ContextMenuArea` (what tilolang lists use) do the same.
- **Select and type:** with a selection in focus, typing a printable character opens the
  composer on it where the window doesn't take text input itself.
- **The palette's ask carries context:** the focused selection or window, not just a title, and
  its answer shows in place.
- **The free-space caret** comes with the canvas.

## 2. The composer

- **Anchored, not covering.** It sits beside its subject and never hides it; on a phone it's a
  bottom sheet that rises with the keyboard.
- **Pickers stay out of the way.** Agent, model and "decide / answer here / run as a task" are
  defaults the person can change, shown as one quiet line, not three controls before typing.
- **Working shows work.** The streamed text appears as it comes (the loop already streams every
  100 ms), with the steps taken so far and a link to the run in Tasks. Stop is its own control,
  away from Send.
- **Answers are rich.** Markdown, resources as chips that open, actions taken as rows that undo,
  suggestions as cards with previews. The subject's context is shown as what it is (a resource
  chip, an excerpt), never as raw markdown.

## 3. Answers as views

- The Tasks agent's loop gets one more way to finish: answer with a view. Same tools, prompt,
  budget. A view answer is a tilolang block (`run-block`'s `{"kind":"view"}`).
- It's kept live, not snapshotted: the block is stored as a resource (as tilolang Chat's block
  parts are) and rendered through `embed()`, with the agent as code author. Suggestions inside it
  are invocation cards; asks still ask.
- It opens as a window, placed beside the subject (`SelectionSeed.area`, which the composer
  already uses).
- Keep: the block resource goes into an app the person owns, and is reused like any view.
  Discard: undo.
- Where the block runs: in the tilolang host whose author is the agent task (ui-host.md step 2b).

## 4. One agent

The Tasks agent, relay agents and external MCP agents are three runtimes that ask the person
three ways. They converge on one: the broker's decision and receipts, the person's agent answers,
one ask model with editable drafts, one run group for undo. External agents keep the door
(`tilo_*` tools, later the `tilo` program tool) as their interface.

## 5. Order

1. Point and type in menus, plus the composer's layout, working state and rich answers (sections
   1 and 2). Independent of tilolang.
2. Answers as views, after the tilolang host (step 2b).
3. One agent: relay agents' standing and asks first (separately tracked), then the rest.
4. The canvas.
