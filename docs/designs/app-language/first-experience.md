# The first experience: windows of views, composed by people and agents

**Goal: one truly new experience.** A person works with windows showing views of resources
(`workspace-model.md`). Asking brings a window. An agent composes new views from existing ones,
and those are kept, shared and changed like anything else. An app is a set of default
capabilities:
- its types and actions;
- its default views;
- its connections to services.

Out of scope until this works:
- how the workspace presents its windows (strips, a 2D canvas, free windows);
- what each person sees of a shared workspace.

## 1. The model, in existing terms

Everything here is `workspace-model.md`'s six concepts. Nothing is added to them:
- **A window shows one resource through a view,** with window state and each person's lens.
  The workspace's windows are shared, synced, undoable and attributed. People and agents open,
  replace, arrange and close them through the same actions.
- **An app supplies types, actions and views.** An outside service is an app whose types an
  adapter fills. The YouTube app has `Video`, `Channel`, `Playlist` and a player view. "Opening
  YouTube" opens a window on its home resource, the person's feed.
- **An agent's answer is a window.** Nothing has to be converted from a chat answer into
  something real. What the agent composes opens as a window in the workspace, live, authored by
  "you via this task".
  - **Keeping it** means not closing it.
  - **Changing it** means asking, or editing it directly. Its source is plain tilolang.
- **A composed view belongs to an app of the person's own.** A view an agent writes (`ideas`,
  `watch_next`) is saved in the person's own app. It can be reused on other resources ("do the
  same for the product video"), shared and versioned. Authority follows authorship: a view by
  someone else runs with the least of their authority and yours.

## 2. Interaction

- **Asking opens a window.** Typing or saying `youtube`, `launch plan` or `jane's mail`
  resolves through search (`search.md`). An app's name opens its home resource, a resource opens
  in its default view, and a query opens a result list.
- **Ask anywhere.** Right-clicking any window, or a selection in one, opens a chat field carrying
  that context (`agents.md` §2). The answer opens beside it in the lane.
- **Across windows.**
  - **Selection is one across the workspace,** so "add these to the board" works on what is
    selected in another window.
  - **Dragging is semantic:** a clip onto a card attaches the clip's anchor.
- **Agents work visibly.** An agent's presence and caret, and the windows it opens, are shown
  while it works, as a collaborator's are.

## 3. The two scenarios

End to end, with real data, in a nested session:

1. **"Ideas for the launch video."**
   - **The window:** the agent gathers from the notes, chat and footage in one program
     (`agents.md` §1). It opens an `ideas` window: cards with looping clips and sources.
   - **Worked on directly:**
     - the person drags a card to reorder;
     - asks on one card for "a shorter version";
     - selects a moment in the teaser's window and says "use this instead".
   - **Kept and reused:** the view is kept. Later, "the same for the product video" reuses it.
2. **"YouTube", then "something good to watch about compilers".**
   - **The window:** YouTube opens on the feed. The question makes the agent search through the
     YouTube app's types, read transcripts and rank by the person's watch history. It opens a
     `watch_next` window: picks with a reason each and the moment worth jumping to.
   - **Plays in place:** clicking plays the video in the player view at that moment.

**Both are done when:**
- a person can do them without instructions;
- every step is an action an agent could also take;
- they run at native refresh;
- the composed views look designed, not generated.

## 4. What it needs

| Need | Where it comes from |
|---|---|
| Views usable with arguments, composable, any app's | `components.md`: a view is a component |
| Windows of many apps' views in one workspace | each tilolang app in its own process today. Agent-composed views run with the person's own app. |
| Asking opens a window | `search.md`'s index, plus opening the default view |
| Agent programs | `agents.md` §1, on main's door and agent participant |
| YouTube | a YouTube app: Data API types, transcripts, the player through Tilo's browser engine, an account sign-in through the existing account custody |

## 5. Open, after the first experience

- How the workspace presents its windows: strips, a canvas, or free windows.
- What each person sees of a shared workspace.
- How a kept composed view is found again: search, its lane, or by name.
