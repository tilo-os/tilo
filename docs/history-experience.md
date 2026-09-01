# Workspace history: glance, catch up, investigate

History is not a log viewer. It is the workspace's shared memory: a calm way to
notice that work is moving, understand what changed while you were elsewhere,
and recover the exact action when something needs attention.

The experience has three zoom levels. They are views of one action-derived
model, not three separately generated feeds.

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

The header says the scope and frontier: “Design review · 18 changes by 3
people”. Below it, three to five outcome cards show explicit work groups first:

> Prepare launch brief  
> Ada and Maren · Notes and Files · 12 changes · 28 min

Each card can expand in place to resources, then individual actions. Expansion
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
context and the existing structural diff summarizer remains their honest
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
- An outcome-first Catch up tab with three-to-five expandable cards and honest
  app/resource provenance, beside the existing granular Explore timeline.
- A private per-device, per-workspace seen frontier in the merged settings
  store. The frontier snapshots on open and advances on close; neither action
  writes to the collaborative document.
- Passive top-bar awareness: a short-lived center-card override for new work, a
  bounded new-change badge at the people cluster, and a click-through into
  Activity. The user's chosen clock/calendar/monitor view is preserved.
- A responsive explorer: wide desktops get lens, grouped-timeline, and
  provenance columns; narrow layouts retain one focused timeline. App and
  resource-kind lens choices compile into the same `HistoryFilter` as every
  other history entry point.

Generated prose remains intentionally optional. It should ship only with
source-frontier labeling and factual evaluation fixtures; every implemented
surface is already complete without it.
