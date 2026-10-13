# History: glance, catch up, browse, and replay

History is not a log viewer. It is the workspace's shared memory: a calm way to
notice that work is moving, understand what changed while you were elsewhere,
and recover the exact action when something needs attention.

The experience has three zoom levels. They are views of one action-derived
model, not three separately generated feeds.

What counts as history, and how Catch up, the timeline and per-resource history
divide the work, is settled in [search-and-history-views.md](search-and-history-views.md).
For an app's Resource, Person or agent, and Task group lenses and pinned causal
comparison, see [Browsing an app's managed History](managed-history-browsing.md).
These views select evidence; they do not grant access to another document.

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
- A small count on the collaborator cluster means “new since your last activity
  review”, not “all unread events”. Leaving the catch-up surface advances the
  private per-device, per-workspace seen frontier.
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

The current runner uses the user's Ollama configuration, including
`TILO_HISTORY_LLM_MODEL` for model selection and `OLLAMA_HOST` for the endpoint.
An endpoint can be local or remote: placement is the user's choice, not a
claim the History UI makes. History does not yet consume the agent provider
profile picker: the model gateway authorizes text transforms only for the agent
app. It does not expand that allowlist or introduce provider credentials.

Generated summaries identify the actual runner/model, locale, projection scope,
committed frontier and coverage. They consume bounded excerpts of the first 24
briefs, up to 64 allowed IDs per brief, and accept at most six unique citations.
Citation membership checks only that IDs were supplied, not factual correctness
or entailment. Citation buttons use the existing Explore evidence navigation;
`history.summary.read` and `history.summary.source.0` through `.5` expose the
same views to automation. Each action binds the exact scoped evidence and a
revocable presentation epoch, so a cached citation cannot target a replacement
summary or pane. The prose is static text for assistive technology, never a
history entry. Scope or frontier changes revoke pending generation and stale
actions; pending, unavailable and invalid-output states retain the deterministic
cards and recorded history.

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

## 4. Replay: recorded workspace topology

Replay shows causal checkpoints of workspace strips, groups, and tab labels.
It does **not** capture full desktop snapshots, historical app pixels, document
contents, or media. Where a source did not record local geometry or active-tab
selection, the presentation explicitly uses neutral layout rather than passing
current geometry off as historical.

Previous/Next, Play/Pause, and a shared slider navigate the same retained
checkpoint selection. The slider exports its range and value through AccessKit;
`room.activity.replay` and the `room.activity.replay.*` actions expose replay without
pointer coordinates. Entry and frontier seeking identify the source as well as
the checkpoint. Inspect reports missing or unavailable checkpoints rather than
fabricating a successful snapshot. A refresh preserves causal selection rather
than silently applying an old ordinal to another entry.

Playback is viewer-local and read-only. It stops when Activity is hidden;
retained actions are refused after workspace, host, or pane replacement. It
never edits the live workspace or another room's document.

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
  reporting, and shared rename/split/merge/boundary request validation.
  Native organization edits are private to the device projection (see below);
  they do not rename collaborators' groups.
- Workspace aggregation across the tree, live records, and validated cold local
  records. Cold reads are bounded and read-only; missing/corrupt records remain
  visible as incomplete coverage rather than silently disappearing.

### Native local organization and collaborative authority

The native Activity pane reads `WorkspaceTree::doc`, the device-wide projection.
`commit_room_change` stamps that document; it is not an admitted room-owner
transaction. Peer publication instead exports `room_docs` after projecting the
workspace cache. Root-level named-range records written only into the device
projection therefore are not a durable collaborative room edit. Likewise, a
borrow of a cached record does not establish the caller's current write role.

Native Undo/Restore remain available through their existing paths. Names,
splits, merges and boundary moves in `tilo:group` are local organization, not
collaborative source edits: `state.automerge` persists them, while
`try_sync_room_doc` projects explicit workspace fields and does not copy these
root markers. The pane labels this device-only scope.

The shared `apply_history_editorial` entry point serves local UI and trusted
in-process semantic callers. Execution requires a mutable live `WorkspaceTree`
whose active room exactly matches the request, or independently revalidated
owner admission. Read-only admitted snapshots and native observation clones
have no local writer capability. Requests bind the reviewed frontier, active
workspace and desktop-tree-only causal bounds; display attribution is never a
grant. The native host does not grant guest room-owner admission. Task commands
continue to use their independent authenticated broker.

The native `history.groups.inspect` and `history.groups.edit` actions share the
pane's local organization path. Requests bind the current room, exact causal
frontier, source, and reviewed group bounds. Name, clear, split, merge, and
boundary movement preserve action attribution; stale requests and requests for
another room are refused. Inspection reveals only the active pane's admitted
bands, not private group names from other workspaces. Host bindings are weak
and are revalidated after pane replacement or revocation.

Collaborative organization remains unavailable: there is no native authenticated
room/record editorial owner executor with durable receipts. Local semantic
actions do not grant remote callers room-owner authority. Collaborative edits
would require exact room/record origins, current owner admission and revocation
checks, and an owner receipt; local projection access must never substitute for
those.

Generated prose remains optional, source-labeled, and subordinate to recorded
evidence; every deterministic surface remains usable without a model.
