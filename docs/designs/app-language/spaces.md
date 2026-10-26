# Spaces: views instead of apps

**Goal: one truly new experience.** A person sees and works with **views of resources**, not
applications. Asking brings a view. An agent composes new views from existing ones, and those
are kept, shared and changed like anything else. Apps remain only as packages that supply
types, actions, default views and connections to services.

Out of scope until this works:
- whether the surface is a 2D canvas, windows or strips;
- what each person sees of a shared space.

The model below doesn't depend on either answer.

## 1. The model

1. **The unit is a placed view:** a view of a package (`notes.page`, `youtube.video`, an
   agent-composed `ideas`), its arguments (resource references and values), and its lens state
   (scroll, selection, playback position). It is live: its actions work, and it updates as its
   resources change.
2. **A space is a resource.** `Space { views: Collection<Placed> }` is synced, collaborative,
   versioned, undoable and attributed like any resource. People and agents change it with the
   same actions:
   - `place`, `replace`, `arrange`, `remove`;
   - `group`.

   A canvas, windows or strips are each just *a view of a space*. That is why the layout
   question can wait.
3. **Apps become packages.**
   - A package supplies types, actions and views.
   - An outside service becomes types plus an adapter. YouTube becomes `Video`, `Channel`,
     `Playlist` and a playback view.
   - "Opening an app" means placing its default view of some resource.
4. **Agent answers are placed views.** There is no separate chat answer that later has to
   become something. What the agent composes lands in the space, live, authored by "you via this
   task". Keeping it means it stays. Changing it means asking, or editing it directly, and
   its source is plain tilolang anyone can open.
5. **Composed views are packages too.** A view an agent writes (`ideas`, `watch_next`) is saved
   as a small package owned by the person. It is reusable on other resources ("do the same for
   the product video"), shareable, and versioned. Authority follows authorship: a view by
   someone else runs with the least of their authority and yours.

## 2. Interaction

- **Summon.** Typing or saying `youtube`, `launch plan` or `jane's mail` resolves through search
  (`search.md`) to a resource and places its default view. A package name places its home
  resource, such as YouTube's feed. A query places a result list.
- **Ask anywhere.** Right-clicking any view, or a selection in one, opens a chat field carrying
  that context (`agents.md` §2). The answer is placed next to it.
- **Across views.**
  - **Selection is one across the space,** so "add these to the board" works on what is
    selected in another view.
  - **Dragging is semantic:** a clip onto a card attaches the clip's anchor, not pixels.
- **Agents work visibly.** An agent's presence, caret and the views it is placing are shown
  while it works, as a collaborator's would be.

## 3. The first experience

Two scenarios, end to end, real data, in a nested session:

1. **"Ideas for the launch video."**
   - **Placed:** the agent gathers from the person's notes, chat and footage in one program
     (`agents.md` §1). It places an `ideas` view: cards with looping clips and sources.
   - **Worked on directly:**
     - the person drags a card to reorder;
     - asks on one card for "a shorter version";
     - selects a clip in the teaser view and says "use this instead".
   - **Kept:** the view is kept. Later, "the same for the product video" reuses it.
2. **"YouTube", then "something good to watch about compilers".**
   - **Placed:** `youtube` places the person's feed. The question makes the agent search
     YouTube through the adapter's types, read transcripts and rank by the person's watch
     history. It places a `watch_next` view: picks with a reason each and the moment worth
     jumping to.
   - **Plays in place:** clicking plays the video in the space at that moment.

**Both are done when:**
- a person can do them without instructions;
- every step is an action an agent could also take;
- they run at native refresh;
- the composed views look like they were designed, not generated.

## 4. What it needs

| Need | Where it comes from |
|---|---|
| Views placeable by reference with arguments and lens | `components.md`: a view is a component; placement stores its instance |
| Many packages' views live in one surface | one engine per process exists; each package in its own instance, rendered into one scene |
| Summon | `search.md`'s index, plus "place the default view" |
| Agent programs | `agents.md` §1, on main's door and agent participant |
| YouTube | an adapter package: Data API types, transcripts, playback through Tilo's browser engine |
| A presentation of a space | a deliberately plain one for now, chosen as the least commitment, not the answer |

## 5. Open, after the first experience

- Canvas, windows or strips, and how spaces relate to workspaces.
- What each person sees of a shared space: their own arrangement or everyone's.
- How a kept composed view is found again: by search, by its space, or by name.
