# Workspace history: glance, catch up, investigate

History is not a log viewer. It is the workspace's shared memory: a calm way to
notice that work is moving, understand what changed while you were elsewhere,
and recover the exact action when something needs attention.

The experience has three zoom levels. They are views of one action-derived
model, not three separately generated feeds.

What counts as history, and how Catch up, the timeline and per-resource history
divide the work, is settled in [search-and-history-views.md](search-and-history-views.md).

## 1. Glance: ambient, quiet, current

The top bar's collaborator cluster is the passive surface. It answers “what is
moving?” without demanding a context switch.

- An avatar gets a short-lived activity ring when that participant completes a
  meaningful action group. Keystrokes and pointer motion never pulse it.
- The center carousel gains a workspace-activity card. Its one line is the
  newest `ActivityBrief::glance()`: “Ada and Maren · Prepare launch brief”. It
  may rotate in for a few seconds after a remote group settles, then returns to
  the user's chosen clock/calendar/system view. Repeated changes to the same
  group update the line instead of restarting the animation.
- A small count on the collaborator cluster means “new since you last opened
  activity”, not “all unread events”. Opening the catch-up surface advances the
  per-user, per-workspace seen frontier.
- Clicking the line opens catch-up with that group selected. Clicking an avatar
  opens the same surface filtered to the person. There is no notification for
  ordinary progress; mentions, permission changes, failures, and requested
  review remain notifications.

This surface uses deterministic action labels and self-described purpose. An
LLM is never allowed to turn low-confidence raw diffs into a confident status.

## 2. Catch up: outcomes before operations

Opening Activity starts on **Since you looked**, a readable overview rather
than the raw timeline.

`Super+H` opens or closes Catch up from anywhere; `Super+Shift+H` goes straight
to the full explorer. `Ctrl+H` remains app-owned (for example, browser
navigation history). The same workspace surface remains available from the
top-bar activity glance, the People menu, and command search; document-owning
apps may additionally expose a local History control.

The header says the scope and frontier: “Design review · 18 changes by 3
people”. Below it, three to five outcome cards show explicit work groups first:

> Prepare launch brief
> Ada and Maren · Notes and Files · 12 changes · 28 min

Every collapsed card already shows who, when, and the affected app/resource;
the user should not need to expand a card to orient themselves. Each card can
then expand in place to resources and individual actions. Expansion
preserves spatial context; it does not navigate to a second, unrelated log.
Cards offer three actions:

- **Open** the current resource in its owning app.
- **Compare** the state before and after the group, using the resource-type
  renderer registered with the system.
- **Undo group**, which appends inverse actions and never rewrites history.

Users and agents can name, rename, split, merge, or move boundaries. Explicit
`ActionGroup` IDs (issue/task/agent-run/session) are the default boundary;
manual names win. The existing time/author/op-family heuristic is only the
fallback for legacy direct CRDT edits.

An optional generated paragraph can sit above the cards, labeled “Summary” and
carrying its source frontier. It is a cache over the facts, never stored as the
facts themselves. Changing filters regenerates it; stale or unavailable AI
removes the paragraph without degrading the cards.

## 3. Investigate: a faceted, lossless timeline

“Explore all history” turns the catch-up surface into a wide content panel. On
desktop it uses three columns; on narrow screens these become push navigation:

```
[ lenses ] [ grouped timeline ] [ action / diff detail ]
```

The lenses are counts over the current result set:

- people and agents;
- app;
- resource kind and individual resource;
- path/directory, file type, and MIME type;
- action family;
- explicit work group;
- date range and workspace.

Selecting several values within one lens is OR; separate lenses combine with
AND. Filter chips are URL/intention-addressable so search, accessibility, and
agents can open the exact same view. Legacy entries are visible under
“Unclassified”; they never masquerade as matching semantic facets.

The middle column remains grouped by default. “Show every action” is a density
control, not another mode. Selecting a row opens a detail pane with immutable
attribution, action input (redacted where policy requires), affected resources,
before/after diff, causal parents, and undo/restore affordances. Permission and
redaction events retain their stricter audit treatment.

## A resource's History panel

Inside an app, History is a panel at the window's trailing side over one
resource: the document the app has open, or a scope the app declares beside
it (Files: the folder and the selected file). It reads the resource's action
log, one row per change, keyed by the change's content hash, so a row means
the same thing on every member's device and after a restart. Selecting a row
previews that version in place, read-only, with Restore and Restore just this
change.

What the panel adds on top of the log, and where each piece lives:

- **Who and which work.** A row carries the author's face, the agent mark for
  software acting for someone, and the bot chip for a bot. Changes that share
  an explicit work group (a task, an agent's run, a shortcut) band together
  under its label, and the band leads with the kind's icon and says the kind
  to assistive technology.
- **Named groups.** Selecting changes and choosing Name these changes in the
  panel's foot names that run; naming it again renames it, and an empty name
  removes it. The name is a `tilo:group:*` record in the resource's own
  document, written through the same door as a restore (the person's write
  permission, the live frontier, the owner's writer), so it syncs end to end
  with the content and is neither a row nor an undo step of its own.
  `history.name` does the same for agents and scripts.
- **Generated prose.** A model may rewrite deterministic facts into a sentence;
  it never decides grouping, attribution or counts. It runs on a member's own
  device, off the UI thread, through the provider that runs there (a loopback
  profile from the provider settings, such as the local model service's). A
  provider off the device never sees workspace evidence; a server can only
  take part as an explicit member of the workspace, like any other member.
- **What each change did.** While History is open, the app asks the shell,
  over its shell-ui connection, to describe a few of the newest undescribed
  changes: each change's summary, the item and purpose it named, and a few
  words of what it wrote, never who made it. The app holds no model. The
  answer shows in place of the change's own summary, which stays the
  fallback, and the owner writes it into the resource's document as a
  `tilo:describe:*` record keyed by the change (when no edit is open, through
  the restore door), so it syncs end to end and no other member's device asks
  again. Like a name, it is neither a row nor an undo step. A member without
  write permission sees what it was told while History is open.
- **Coming back.** Entering a workspace with others' changes since the private
  seen frontier opens Catch up: outcome cards per work group, the people and
  agents involved, and a summary sentence when a local model is available.

Not there yet: a resource's History does not include its child resources
(the highlights inside an ebook live in their own records); bands are not
described, only changes; Catch up summarises per work group, not per
resource.

## The action record

`ActionEnvelope` now carries optional `ActionContext`:

- human action label;
- app id and label;
- addressed resource id, kind, and label;
- explicit purpose supplied by the person or agent;
- stable work-group id, label, and open kind;
- open key/value facets such as path, directory, file type, MIME, or repository.

The executor serializes this context into the Automerge change message under a
versioned prefix. That makes provenance part of the durable, content-addressed
change and therefore identical on replay. Old envelopes deserialize with no
context and the existing structural diff summarizer remains their
fallback.

The action door populates resource/action context automatically because it
already resolves both. `ActionHost::dispatch_with_context` is the direct-UI
path for apps; hosts can enrich it with app, purpose, group, and facets. The
history source reads context back into `HistoryEntry`. `HistoryFilter` and
`ActivityDigest` are shared read models for the explorer, catch-up panel, and
top-bar glance.

## Trust, privacy, and retention

- Purpose is quoted as self-description, never presented as system inference.
- Generated summaries link to the exact action frontier they summarize.
- Sensitive action arguments do not enter `ActionContext`; facets are metadata
  and must pass the workspace's disclosure policy before dispatch.
- Redaction removes protected payload display but leaves the auditable fact that
  a redaction occurred.
- “Seen” is private per-user state. It is not collaborative and does not create
  history about reading history.
- Compaction may replace old detail with signed checkpoints, but group titles,
  attribution, counts, resource addresses, and restore horizons remain.

## Implemented foundation and surfaces

- Semantic action context, durable commit messages, filters, explicit grouping,
  shared digests, and a generic facet-count index.
- An outcome-first Catch up tab with three-to-five expandable cards and clear
  app/resource provenance, beside the existing granular Explore timeline.
- A private per-device, per-workspace seen frontier in the merged settings
  store. It retains full Automerge heads per record document and explicit
  room-scoped tree exceptions, so delayed/concurrent changes remain unread and
  bounded projections cannot make old work unread again. The frontier advances
  on close; reading never writes to a collaborative document.
- Passive top-bar awareness: a short-lived center-card override for new work, a
  bounded new-change badge at the people cluster, and a click-through into
  Activity. The user's chosen clock/calendar/monitor view is preserved.
- A responsive explorer: wide desktops get lens, grouped-timeline, and
  provenance columns; narrow layouts use keyboard/accessibility-addressable
  Timeline, Browse, and Details push navigation. Overflow controls keep every
  contributor, lens, and outcome reachable at short heights.
- Exact causal comparison with generic before/after patches, stable selection
  across live feed reorder, provenance-aware Undo refusal, partial-Undo
  reporting, and collaborative rename/split/merge controls.
- Workspace aggregation across the tree, live records, and validated cold local
  records. Cold reads are bounded and read-only; missing/corrupt records remain
  visible as incomplete coverage rather than silently disappearing.

Generated prose remains intentionally optional. It should ship only with
source-frontier labeling and factual evaluation fixtures; every implemented
surface is already complete without it.
