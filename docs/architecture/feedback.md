# Feedback

Feedback is not a telemetry pipe bolted onto the desktop. It is what Tilo
already does, pointed at the people who build it: **saying something about a
thing** ([`tilo-reference`](../../crates/collab/reference)), in a room, with a
record of what happened attached.

This document names the concepts feedback is made of. Where a concept falls
short, it is extended in place: every extension here also serves
accessibility, follow mode, agents, tests or freeze/thaw.

## A report is a remark

A `Remark` is `(text, address, destination)`. The address is a
`ResourceRef`, so the reader never has to guess what "this" meant. Feedback is
a remark with one of two destinations:

- **Private:** the reporter's `DirectRoom` with the Tilo team principal. Only
  the two of them are in that conversation. The team replies there, and
  replies arrive as ordinary chat notifications.
- **Public:** the `#feedback` channel of the **Tilo Community** room. Other
  people see it, react to it and reply in its thread. Because they have met
  there, they can message each other or invite each other to a workspace. The
  community room is how an alpha user finds other people before they have
  anyone to collaborate with.

The composer is the system's remark surface, not a feedback form. It opens on
whatever is picked (a UI element, a text span, a document, a recording) and
lets the user choose the destination. Posting feedback to Tilo and making a
remark to a colleague about a paragraph are the same gesture with different
destinations.

## Picking any part of any app

Every app already publishes its UI as a `SemanticTree`. The same tree drives
AccessKit, the `ui:` action resources and `Ctrl+K`. Picking extends it with
two things:

1. **Stable semantic identity.** A node id must survive a layout rebuild,
   otherwise a pick, a recorded step or a screen-reader focus points at the
   wrong control a frame later. Ids come from, in order, the app resource id
   (`r:`), `Widget::test_id`, then a keyed structural path. The Taffy node id
   is never used.
2. **Inspect.** The shell asks an app connection over the collaboration
   broker for the semantic node at a point, a node by id, or a subtree. The
   answer carries each node's role, label, value, sensitivity, bounds in
   surface coordinates, and `ResourceRef`, the last through `Referent` when the
   node stands for something in the document. This is the same
   request/response shape as an `Invocation`, and it is capability-checked the
   same way: reading a subtree needs `Read` on the room.

Pick mode is shell chrome. It highlights nodes under the pointer across every
app in the workspace. Each app's own tree supplies the geometry; nothing is
guessed from pixels. Clicking adds a node to the selection, and dragging
selects a region. The selection becomes the remark's reference.

## A recording is a silent follower

Recording adds no new stream. It is a participant with read-only access that
joins the room and keeps what it sees:

| What replay needs | Where it comes from |
|---|---|
| State at the start | The broker's late-joiner snapshot (`DocumentSnapshotRequested`) |
| Every change | The signed document ops the broker already fans out to every subscriber |
| Where the user looked | The View subset and caret/selection presence |
| What the user tried | `Interaction` presence, the semantic action lifecycle including failures |
| What the UI looked like, as structure | Inspect snapshots whenever the semantic tree changes |
| What the UI looked like, as pixels | Compositor frames (the second layer, below) |
| Where it ran | Build and image digest, surface size, scale, theme, locale |

The recorder is an **agent principal** with the Observer capability bundle
(`agents.md`). It is not a special code path. It shows in the collaborator
popover with the `AGENT` label for as long as it records, and removing it
revokes it. That is the recording indicator, the consent boundary and the
revocation path, all existing ones. It is admitted only to rooms the user
chose to record.

Two gaps were real, and both are fixed in the toolkit because follow mode and
freeze/thaw have them too:

- **Presentation state inside widgets.** Open menus, popovers, dialogs, toasts
  and error banners were not part of the View, so a follower never saw them
  and a thawed app lost them. The View now carries the open overlays by
  semantic id.
- **Attempts that changed nothing.** A control invoked through normal input
  now publishes the same `Interaction` lifecycle as a broker invocation, with a
  `Human` actor. A click that was rejected, or that failed validation, is
  therefore visible to a follower and kept by the recorder, even though it left
  no document change.

The result is a `session` record in the user's own room. It holds the
starting snapshot, the op log, the presence timeline, the inspect keyframes
and a reference to the frame blob. Automerge history cannot be served in part
(`rooms.md`), so what leaves the device is never the user's document. It is a
**redacted duplicate**: a new record with provenance to the original, built by
replaying the window into a fresh document.

Recording is always explicit (the user starts it) or retroactive over the
in-memory buffer of the last two minutes. The buffer holds presence, inspect
keyframes and op references only; the ops themselves are already in the
record's history. Nothing is written or sent before review.

## Two layers: structure, then pixels

Replay is exact for anything the semantic layer describes. Pixels are the
check on that claim:

- The compositor captures the recorded surfaces' frames, timestamped against
  the trace.
- Before sending, the device replays its own trace offscreen and compares the
  replayed frames with the captured ones, element by element, using the
  inspect bounds.
- **They match:** the problem is one of logic or UX. The trace is the report,
  and frames are not attached unless the user adds them.
- **They differ,** or the region has no semantic node (a web view, a legacy
  client): the problem is below the toolkit. It could be the renderer, fonts,
  the GPU driver, scaling or the compositor. The report is tagged as such, and
  the diverging regions are offered as frames to attach.

Frame redaction is exact rather than guessed. The trace knows where every
sensitive node was on screen, so those rectangles are masked.

## Replay is following a past session

Replaying a `session` record creates an ephemeral **replay room**. The shell
launches the recorded app into it, sandboxed as usual. A replay peer in the
shell publishes the snapshot, ops and presence as the original participant,
and the viewer's app follows that participant. Replay is therefore the
production follow path, fed from a record instead of the network. Scrubbing
reloads the snapshot and fast-forwards. Inspect works on the replayed app like
on any other, so the team can pick an element inside a report.

The same trace format is what headless tests and agents consume. A test can
record a real session and assert against its replay.

## Sensitivity is declared, not guessed

`AccessState.protected` was the only data classification. It becomes
`Sensitivity { Public, Personal, Secret }` on semantic nodes and on resources.
`Secret` keeps the old `protected` guarantees: it never reaches AccessKit,
inspection or a trace. `Personal` is kept locally and replaced with
shape-preserving placeholders in anything that leaves the device, so a
same-length name still shows a truncation bug. Shared widgets set the
sensitivity for their kind (password fields, contact fields, message bodies),
and apps mark the rest.

Undeclared free text is where the local model comes in (`Review`, below).

## Review, on the device

Before anything is sent, the user walks through the report. The walk-through
is generated from the trace, not narrated over a video: "You opened Mail.
You wrote to ▮▮▮▮▮. You pressed Send. *Couldn't send* appeared." Each step
shows the replayed state with its redactions, and the user can open,
re-redact, cut or keep each one.

A local model, through the model gateway and a device-local provider profile,
does two jobs: it proposes redactions for undeclared text and image content
(OCR plus the local vision model), and it writes the step titles. It never
receives anything the sensitivity layer already marked `Secret`. Without a
local model, review still works: declared sensitivity and pattern detectors
propose the redactions, and step titles come from action labels.

## Rooms, and why the public one is different

A report record lives in the reporter's own room. Sending it adds a reference
from the destination room, which is the ordinary sharing verb in `rooms.md`.
Retracting removes the reference. The limit that `rooms.md` states applies
here too: anyone who already synced the report keeps what they have, and the
composer must not suggest otherwise.

Encrypted channels rotate their MLS group key on every membership change. That
is correct for a team and unworkable for a public room that thousands of
people join and leave. The community room is therefore its own **channel
class**, public: ops are signed and validated exactly like any other, but not
encrypted, and joining is redeeming a **standing invite**. A standing invite is
a multi-use invite token with the Commenter role, which lets its holder post
and react. It is hosted on a Tilo-operated workspace host (`tilo-relay`).
Everyone in the room is a Commenter; the team are Editors, so they can set a
report's status.

Votes are reactions: Commenter already means "comment or reaction". Status
(open, planned, fixed in generation N) is a typed chat action that only an
Editor can take. "Fixed in generation N" is matched against the reporter's
image generation, so the reporter is told when their build contains the fix.

## Triage

The triage agent is an ordinary agent member of the community room and of each
private direct room. It has the Contributor capability bundle, scoped to the
feedback channel. It reads remarks the way `apps/agent` already does, with the
reference as an address rather than prose. It groups duplicates, replies with
links to earlier reports, and drafts forge issues that a person approves.

## Verification

The acceptance test is a local nested setup:

- a `tilo-relay` hosting the community room;
- a reporter shell and a team shell, each with its own identity;
- at least Notes, Chat and one more app in the reporter's workspace.

The reporter picks an element, records a flow that ends in an error, reviews
and redacts it, and posts it publicly. The team shell sees it in `#feedback`,
replays it, inspects the element, votes, replies and sets its status. The
reporter receives the reply and the status change. Every step is driven
through `tilo-control`'s semantic half, so the same run works headless.
