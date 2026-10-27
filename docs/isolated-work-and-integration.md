# Isolated work and reviewed integration

**Status: investigation, no decision.** This page records what one can do
today, what the existing owners already decide, which product options are on
the table, and what is missing for each. It recommends an order of work and a
first experiment that does not add a history authority. It does **not** add
branches, copies, merges or new actions; implementation needs its own
decision.

Read [universal-version-control.md](universal-version-control.md) for the
native history slice that exists (pinned typed reads, `diff`, `review-restore`,
`restore-reviewed`), [history-experience.md](history-experience.md) for the
workspace experiences built on it, and
[`architecture/resources.md`](architecture/resources.md) for records, child
records and views. This page adds no capability to those; it names what they
already settle and what they leave open.

## The problem, stated without version-control words

Someone starts from a state the workspace agrees on and wants to try something
that does not land in the live resource until someone has looked at it: an
agent rewriting a chapter, a person restructuring a spreadsheet, a draft of a
release note that may be abandoned. They want to leave it, come back, show it,
throw it away without residue, and bring part of it in deliberately — knowing
whose work it was and whether what they reviewed is what would land.

The temptation is to answer with Git: branches, merges, pull requests. The
reason to hesitate is that the system already has a change graph, a reviewed
restore door and a permission model, and a second line model would have to be
reconciled with all three. So the question is not "which version-control
feature is missing" but "which existing owner can hold isolation, and what
does isolation mean when there is no line to isolate".

## What exists today

Everything below is in the tree; each line names the owner.

| Need | Today | Owner |
|---|---|---|
| Work from a known state | Checkpoints name a version; historical typed reads reconstruct it in place | `history_source::steps::checkpoint`, framework `read({version, item})` |
| Compare without touching the live resource | `diff` returns differing exact paths and both full head arrays; History renders typed before/after | `framework::history`, `history_source::steps::changed_items` |
| Bring selected work in | `review-restore` pins target and live heads; `restore-reviewed` applies only if the live frontier still matches, as one attributed, undoable change | `framework::history::review` |
| Keep concurrent work | reviewed restore patches only the selected paths; a peer's unrelated change survives | `history_source::steps::restore_paths` |
| Attribution | every change carries actor, time and the action's context (app, resource, item, work group, invoker, cause); agent steps are grouped by run | `tilo_actions::ActionContext`, `executor` |
| Undo without rewriting | undo appends the inverse change; a run's steps are named and taken back together | `history_source::steps::undo`, `Scope::Run` |
| A stretch of history on its own | `tilo_resource::recording` writes a window of a document's changes into a separate document with the original actors and entries | `tilo-resource::recording` |
| A workspace is a collaboration unit | each workspace has its own room, its own governance, roles and grants per member | `tilo-peer-state::room`, `action-policy` |
| A record can cross a room | `record::transfer` rehomes a record under a fresh id in another room, keeping the source id in `formerly`; `retire` stamps where it went; `install`/`catch_up` merge concurrent divergences | `ui-core::record::transfer` |
| Deterministic merge check | `fs::conflict_merge::merge_exact` merges two exact versions before publishing, or answers "needs review" without writing | `tilo-fs::conflict_merge` |
| Multi-record crash safety | a journalled transaction covers several record mutations and replays them after a crash | `fs::record_transaction` |
| Whole-workspace history | Activity joins the workspace tree, live records and validated cold local records, marking incomplete coverage | `desktop-surface::activity` |

Two consequences matter more than the table.

**There is nothing to branch from.** A record's document is the unit;
`record::transfer` already mints a fresh identity and keeps provenance, and
nothing looks a record up through `formerly`
(`ui-core::record/transfer.rs`, module docs). Copying a view or a window is
presentation, not state — the document is still the same document. So an
"isolated line" in Tilo is not a branch of one graph. It is either *another
record* whose content started from a version of this one, or *the same record*
with edits that nobody else can read yet.

**Review already has a door.** `review-restore`/`restore-reviewed` was built
for a different question ("bring back exactly these old items") but its
mechanism is the undoing integration needs: pin the target frontier and the
live frontier, refuse if the live one moved, apply as new attributed work. Any
integration story should reuse it rather than invent a second "apply a change
set" verb with its own staleness rules.

## The four candidate shapes

### A. A separate copy record, integrated back by reviewed extraction

*Shape.* "Try this elsewhere" copies the current version into a draft record
in the same room (or a workspace of its own), linked to its source. The draft
is ordinary: it has its own history, its own permissions, its own place in
Activity. Integration is a comparison between the draft version and the live
source, selecting exact items, and applying them there — `diff` then
`review-restore`/`restore-reviewed`, with the draft answering `read`.

*What it buys.* Isolation is total: the live resource is untouched, nothing
about it needs a new visibility rule, and abandoning a draft is deleting a
record that never reached anyone who was not granted it. Divergence is a fact
(they are different documents), not a mode.

*What it costs.* Content is duplicated, including retained media, and the
copy's own history starts at the version it was taken from. A referee has to
compare two resources instead of one. Integration is line-level application of
patches: it carries items a person selected, never the draft's identity or its
own renames/deletes as first-class operations. Child records of the source
(`architecture/resources.md` §2.6) are separate records, and each would have
to be copied and integrated in turn unless a multi-record transaction carries
them (which `fs::record_transaction` could).

*Gaps.* No verb creates a live copy of a record's current state (gap 1, below); no link type binds a draft to its source; no cross-resource comparison
view.

### B. Pending changes on the same record

*Shape.* Edits are written locally and held back from publication until
someone accepts them. Review happens in the resource's own History, where the
pending run is a work group; acceptance publishes, rejection discards.

*What it buys.* No copy, no second document, one history, and tags, search and
references keep pointing at one thing. Strong for agent work, where "the agent
proposes a run and the person accepts it" is the whole story.

*What it costs.* It is the shape that fights the existing model hardest. The
CRDT is the truth and everyone holding the record converges on it; holding
changes back is either (a) never writing them — then a crash loses them and
History never sees them, or (b) writing them to a document nobody may read —
then this device is an authority deciding who sees a change, which is exactly
the second history authority the project avoids. It also assumes a single
approver, and collapses when the person with the gate goes offline while
others keep editing.

*Gaps.* No pending-changes store exists. `ActionContext` has groups and
`caused_by`, so attribution and grouping would be easy; the isolation would
not.

### C. A workspace of one's own, and a deliberate move

*Shape.* Experiment in a workspace granted to one person (or one person and
an agent); when a version is worth keeping, carry it back — copy the record
across the boundary with `record::transfer`, or grant the other workspace's
members read access to that one resource.

*What it buys.* It is built entirely from what exists: a workspace is already
the unit of collaborators, permissions and host (`architecture/hosting.md`),
and transfer already rehomes a record losslessly. Nothing new decides
visibility.

*What it costs.* Coarse. A workspace carries windows, layout and unrelated
resources, so it is a heavy place to put one experiment. Moving a record back
gives the destination a new identity rather than integrating into the live
record, so it answers "keep this work" but not "integrate into that".
Permissions do not travel well: someone who saw the draft in the private
workspace is not reachable in the destination.

*Gaps.* A lightness layer for "a scratch workspace seeded with one resource";
nothing else.

### D. No isolation; review is a checkpoint

*Shape.* Accept what exists. A person or agent works in the live resource on a
checkpoint, reviews the frontier before the work against now, and restores
selectively if the outcome is unwanted. A run's steps stay visible as one work
group and are undone together.

*What it buys.* Nothing to build, nothing to reconcile. It is honest about the
model: this system's reversible experimentation is history, not parallel lines.

*What it costs.* Experimentation is visible while it happens. Others must
tolerate half-finished content in the shared resource — which is fine in a doc
two people co-author and bad in a published artifact, or in a resource with
downstream consumers (an index, a published page, an automation). Rejecting
work is "undo the run", which is coarse: it takes back *everything the run
did*, not "keep paragraph two".

## The questions the issue asks, answered against the code

**Is a branch-like line the right abstraction?** Not as a first answer. The
cheapest line-shaped thing is shape A (a draft record), and it reuses
`transfer`'s existing identity rules. A branch *inside* a record (shape B) is
the expensive one, and it is the one that would need a visibility authority.

**Is edit isolation sufficient with existing collaborator permissions?** They
are different questions and should stay so. Roles answer "may this person
change this resource": a Commenter cannot write the body, an Editor can, a
Moderator outranks for other people's content, a Viewer reads
(`action-policy::grant::Role`), and role changes take effect on the next
admission (`room::admit::may_write` reads the policy already held). Isolation
answers "may this change be seen yet", which no role expresses. Shape A needs
no new permission — the draft is a resource with its own grants. Shape B would
need a new concept, and it should not be smuggled into roles: "may propose" is
a grant, "whose proposal is pending" is history.

**What does integration mean for text, structured resources and
dependencies?** Three different answers, and only the first is solved today.

- Text and structured items: `steps::restore_paths` applies exact paths, so
  integration is selection, not text merge. A changed item is taken whole; two
  people editing the same paragraph is not a per-character merge, it is
  "choose one". That is defensible for structured content and poor for prose.
- Binaries and files: `fs::conflict_merge::merge_exact` already answers
  "deterministic merge" or "needs review" for exact versions, with the
  codec's own rules; that is the right owner for a file-shaped integration.
- Dependencies: an integrated item can reference a resource that is not in the
  destination, a child record that was not carried, or an id that will never
  exist there. Nothing today checks a reference after an integration; the
  draft keeps working because it carries its own children, the source does
  not.

CRDT convergence is not correctness, and the code already contains the proof
text for this: a converged document can still be the wrong code. Nothing in
this document proposes to solve semantic conflict; it proposes that
integration *states its coverage* (which items, which children, which
references) instead of claiming success.

**How should attribution, history, references, retained media, revocation and
stale review behave?**

- *Attribution* is per change: a copied record carries the original actors on
  the changes it inherits, because `record::transfer` forks the graph. Items
  applied across resources are one new change attributed to whoever applied
  them. The draft's
  author is therefore not hidden, but the source's history gains a change by
  the integrator, which History should describe as integration of another
  resource, not as authorship.
- *History visibility*: a draft's history is the draft's. If the draft is
  deleted, nothing in the source's history points at it, so a reviewer's
  evidence would be gone. An integration should therefore record the exact
  reviewed frontier as a fact of its change (the way `restore-reviewed`
  already pins `target_heads`), not a reference to a mutable name.
- *References*: `formerly` is provenance only and is never looked up
  (`record/transfer.rs`). Anything that rewrites ids across resources would be
  a new authority; the cheap alternative is that integration refuses to leave
  a dangling reference rather than repairing one.
- *Retained media*: `retire`/`retirement` marks a moved record so no live
  copy stays editable. Media a draft retained is the draft's; abandoning it
  must evict that content, or a discarded experiment keeps costing storage on
  every member's device.
- *Revocation*: removing a member from the source workspace takes effect at
  the next admission (`room::admit`), but it says nothing about the copy of
  the content that member already holds in a draft of their own workspace. No
  mechanism can truly un-share bytes; the system's answer must be that a draft
  is a *separate resource with its own grants*, so revoking the source does
  not silently revoke or preserve the draft — the draft owner decides, and
  their own grants still bound what they can read.
- *Stale review*: `review-restore` already refuses any new committed change,
  even an unrelated one, and refuses ABA (`universal-version-control.md`,
  "Exact selection and conservative staleness"). That is the conservatism an
  integration wants, and it is measurable: `history_review.rs` exercises it at
  the native action door.

**Can a design compose existing owners, certified routes and durable
publication without another history authority?** For shape A, yes, with one
qualification: a draft is a record in the same room, so its history is already
in Activity's aggregation (`desktop-surface::activity`) and its changes travel
on the record's own route; nothing new decides who sees what. The qualification
is that *creating* the draft is a write in the source room that other members
will see (a record appears) — unless drafts live in a workspace of the draft
owner, which is shape C. For shape B, no: it needs a visibility decision that
does not exist, and adding one would be a second truth about a document.

## Owner, storage and security gaps

1. **No verb creates a live record from a version.** `RecordSet::duplicate`
   (`ui-core::record`) mints an empty record to be checked out and filled by
   the app; `transfer::rehome` is the lossless copy (fork, re-scope, fresh id,
   `formerly`) but refuses to run in place (`from_room == to_room`) and needs
   a durable store to land in. "Copy this version into a new draft record" has
   no owner.
2. **Child records are not carried.** A parent's document references child
   records by id (`architecture/resources.md` §2.6); a draft that copies the
   parent alone breaks those references, and one that copies the children has
   several records to keep in step. `fs::record_transaction` is the only
   existing thing that commits several records as one durable unit, and it is
   built for signed file mutations, not for record drafts.
3. **Cold history covers what is here, not what a draft means.** Activity's
   cold reads (`desktop-surface::activity::index`,
   `shell::cold_record_reads`) index records on this device and mark
   incomplete coverage; a draft living on someone else's host is outside that
   view, so a reviewer's "before/after" would silently be partial.
4. **Tree-level references are workspace-local.** A record is addressed by
   `RecordId` inside a room (`record::transfer` module docs); an integration
   that crosses rooms must translate identity, and any references the content
   carries are left pointing nowhere. Nothing validates that today.
5. **Revocation of unread content has no story.** `RecordSubscriptionKey`
   (`fs::record_subscription`) is workspace plus record; removing a member from
   the source workspace does not touch a copy they hold elsewhere, and no
   durable receipt of what was read exists.
6. **No evidence a reviewer looked.** `ActionContext` records cause
   (`caused_by`) and group, and `architecture/action-evidence.md` records that a
   host admission is *not* proof of human consent. A "reviewed integration"
   therefore needs a decision about whether review is a fact in history (a
   named human step) or an assertion to be trusted, before any action is
   spelled.

## Recommendation

1. **Do not add branches.** Shape B is ruled out by current architecture: it
   needs a visibility authority over a document, which contradicts "the CRDT
   is the truth and the room decides who may write". Shape D remains the
   correct default for co-authored work.
2. **Build the smallest thing that makes shape A real**, in one slice, once
   its own decision is made: a way to start a draft record from a pinned
   version, with a link back to the source and its copied children; the
   integration door is the existing `review-restore`/`restore-reviewed` applied
   across resources, which needs only a `read`-like source and the same
   staleness rule.
3. **Judge it by one workflow first**: a draft in the same room, taken from a
   checkpoint, integrated item by item into the live source with the source
   edited meanwhile. If reviewers find they wanted the draft to be invisible
   to other members, that is shape C's question (a private workspace), not a
   new visibility rule.
4. **Keep semantic conflict out of scope.** Integration selects exact items and
   states coverage; a person decides what "correct" means. Any future merge
   check belongs with `fs::conflict_merge` for file-shaped content, not in a
   generic CRDT merge.
5. **Write down, before implementing, what a discarded draft must release**:
   its retained media and its evidence. Otherwise "abandon safely" is a data
   retention promise nobody keeps.

## Explicitly not decided here

Whether drafts live in the same room or in a private workspace; whether they
are records at all; how a draft's link to its source is named in the action
door; whether review is itself a recorded step; how references across
resources are validated; and the verbs' spelling and arguments. All of those
belong to the product decision this page informs, and none is implied by
`universal-version-control.md`'s existing slice.

## Evidence index

- Reviewed exact-item restore and its stale/ABA refusal:
  `crates/sdk/tilo-app/src/framework/history/review.rs`,
  `crates/sdk/ui-core/src/history_source/steps.rs` (`restore_paths`),
  `crates/sdk/tilo-app/tests/history_review.rs`.
- Copy/move identity across rooms:
  `crates/sdk/ui-core/src/record/transfer.rs` (`rehome`, `install`,
  `catch_up`, `retire`, `retirement`, `provenance`).
- Empty-record copy:
  `crates/sdk/ui-core/src/record.rs` (`RecordSet::duplicate`).
- Deterministic merge and its refusals: `crates/system/fs/src/conflict_merge.rs`
  (`preflight_exact`, `merge_exact`, `MergePreflight`).
- Crash-safe multi-record commit: `crates/system/fs/src/record_transaction.rs`.
- A window of history as its own document:
  `crates/sdk/tilo-resource/src/recording.rs`.
- Roles and room admission: `crates/collab/action-policy/src/grant.rs`,
  `crates/collab/tilo-peer-state/src/room/admit.rs` (`may_write`).
- Workspace-wide aggregation with incomplete-coverage reporting:
  `crates/session/desktop-surface/src/activity/index.rs`.
- Action context and work groups: `crates/collab/actions/src/context.rs`,
  `crates/collab/actions/src/executor.rs`.
