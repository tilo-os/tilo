# The first experience

**Goal: one truly new experience.** You point at something and say what you want. An agent
composes a view from the resources involved and it opens as a window, live. You keep it, change
it, reuse it, or throw it away.

## 1. In existing terms

Nothing new beyond workspace-model.md, canvas.md and agent-ux.md:

- **An answer is a window** showing a composed view, authored by "you via this task". Keeping it
  means not closing it; changing it means asking again or editing its tilolang source.
- **A kept view goes into an app of your own** and can be reused on other resources ("do the
  same for the product video") and versioned. Sharing views with other people is out of scope
  for now; you share resources, which others see through their own views.
- **Outside services** are types whose origin is the service, with get and search capabilities
  (convergence.md §5). The YouTube app has `Video`, `Channel`, `Playlist` and a player view.

## 2. Interaction

- Point and type: in free space, on anything (right-click), or on a selection (canvas.md).
- Typing a name (`youtube`, `launch plan`) opens it: an app's name opens its root resource, a
  resource its default view, a query a result list.
- Selection works across windows ("add these to the board"); dragging carries meaning (a clip
  dropped on a card attaches that moment).
- Agents work visibly, with presence like any collaborator.

## 3. The two scenarios

End to end, with real data:

1. **"Ideas for the launch video."** The agent gathers from notes, chat and footage in one
   program and opens an `ideas` window: cards with looping clips and their sources. You reorder
   cards, ask one card for "a shorter version", select a moment in the teaser and say "use this
   instead". Later, "the same for the product video" reuses the kept view.
2. **"YouTube", then "something good to watch about compilers".** YouTube opens on your feed.
   The agent searches through YouTube's capabilities, reads transcripts, ranks by your watch
   history, and opens a `watch_next` window: picks with a reason each and the moment worth
   jumping to. Clicking plays it in place at that moment.

Done when: someone can do both without instructions; every step is an action an agent could
also take; they run at native refresh rate; the composed views look designed, not generated.

## 4. What it needs

| Need | From |
|---|---|
| Views composable from any app | components.md |
| Many apps' views drawn cheaply in one place | ui-host.md |
| Agent programs on the door | agents.md, the `tilo` tool |
| Answers as views, point-and-type | agent-ux.md, canvas.md |
| Reaching native apps' data and actions | action-system.md |
| YouTube | a YouTube app: API types, transcripts, the player through Tilo's browser engine, sign-in through existing account custody |
