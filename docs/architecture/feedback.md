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

Recording does not need a room administrator. In a room the user
administers, on the machine that governs it, the recorder's admission is an
ordinary agent grant in the room's governance. Anywhere else, including a
shared room the user only views, it is the user's **device delegate**
(`WorkspaceTree::admit_device_delegate`). That is a membership held on the
user's own device and never written into governance, like a host-delegated
admission. It asks no more than the user's own role allows. It stands only
while the user keeps that role, so a person who is removed takes the
recorder with them.

Everyone in the room still sees it. The device announces the delegate as a
`RecordingDelegationV1` (`collab_types::recording_delegation`). The record
is signed with the device key and carries the person's certificate for that
device. It travels like a host admission: on the room's topic, retained by
its hosts, and told again to a peer that joins. Every member checks the
following before listing the recorder as an agent in its roster and
collaborator popover:

- the signature;
- that the certificate is the person's, unexpired and unrevoked;
- that the person still holds a role in the room;
- that the room allows recording delegates.

Stopping publishes a signed stop, which takes the recorder off every list.
An older start replayed later cannot revive it, because the latest record
per device, agent and room is the one that holds. Members re-check the
records whenever governance changes, so a person who loses their role drops
off.

Whether a room allows recording delegates is room governance: Workspace
Settings › Agents › **Members may record**, on by default. It is projected
and replicated like the rest of governance. While it is off, members'
devices refuse the delegation, and the recording device refuses to start
and says why. A recording that is running when it is turned off ends.

A room's app routes are encrypted channels, so what the recorder sees is
ciphertext until it is opened. The shell's participants (the recorder, a
replay peer, record custody) reach a channel's content keys the way an app
does, through this device's channel service, and within the same bounds
(`shell::participant_keys`):

- Keys reach only a participant admitted to the room, only for that room's
  channels, and never leave the device.
- Reading and writing are separate handles. The recorder, an Observer, holds
  an open-only handle and can never write in a room it records. Sealing goes
  only to a replay peer, for its own replay room, and to record custody, for
  the rooms it serves a record in.
- Nothing is cached. Every open and seal asks again whether the participant
  is admitted, so revoking the recorder's admission drops its keys at once.
- A public room keeps no group key. Its content is plaintext, and that is
  what the recorder keeps and a replay peer or custody writes there.

The recorder keeps what it opens. The trace therefore holds the plaintext the
user saw, and it stays on the device like everything else in it until review
redacts it.

Two gaps were real, and both are fixed in the toolkit because follow mode and
freeze/thaw have them too:

- **Presentation state inside widgets.** Open menus, popovers, dialogs, toasts
  and error banners were not part of the View, so a follower never saw them
  and a thawed app lost them. The View now carries the open overlays by
  semantic id.
- **Attempts that changed nothing.** A control invoked through normal input
  now publishes the same `Interaction` lifecycle as a broker invocation,
  attributed by the broker to the `Person` who pressed it. A click that was
  rejected (permission denied, disabled, invalid input, no change) or that
  failed is therefore visible to a follower and kept by the recorder, even
  though it left no document change. Peers draw a person's attempt only when
  it did not happen; the rest of the lifecycle is known but not painted.

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

A capture-protected app (a password manager, an authenticator, an app that
asked for protection) contributes nothing to a session: the recorder drops
its snapshots, ops, presence, interactions, inspect keyframes, surface
geometry and frames at intake, for explicit recordings and the recent buffer
alike, and keeps only a marker that it was present and protected
(`session_trace::ProtectedApp`, in the trace's environment). Review lists it
with the capture placeholder instead of steps, and replay does not launch it
but shows the placeholder in the replay room.

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

A feedback recording captures pixels for the check above by default. They
stay on the device and are offered only where they diverge, unless the user
chooses to keep them with the recording or to capture none. The compositor
renders each recorded app surface alone, never the desktop or another app,
at the scale it renders at, once a second and on damage, and the recorder masks each frame with the surface's inspect
bounds before it is encoded, then again against the finished trace. Frames
are lossless PNG keyframes, because a lossy codec would make the check
compare codec noise. They form one bounded blob that the `session` record
references with relation `frames`. Pixels that no semantic node describes
are flagged per region as unexplained and are never guessed at. The check
compares pixels per element with a perceptual tolerance that ignores
anti-aliasing, and each diverging region comes with a small cropped
evidence frame.

## Replay is following a past session

Replaying a `session` record creates an ephemeral **replay room**. The shell
launches the recorded app into it, sandboxed as usual. A replay peer in the
shell publishes the snapshot, ops and presence as the original participant,
and the viewer's app follows that participant. Replay is therefore the
production follow path, fed from a record instead of the network. Scrubbing
reloads the snapshot and fast-forwards. Inspect works on the replayed app like
on any other, so the team can pick an element inside a report.

The replay room's routes are encrypted channels like any room's. This device
governs the room, the peer seals what it publishes for each channel, and the
replayed app opens it with the keys it is handed for its own launch. When the
room closes, its channels and their state are forgotten.

A replayed app does not show a seek all at once: presence reaches it at once,
a document change once the app has opened and merged it. The offscreen check
therefore captures each frame the app draws after a seek and settles on the
first that matches the recorded one. If none matches within the settle
window, it takes what the app shows then. A replay diverges only when it
never reaches what was recorded.

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

A model, reached through the model gateway's fixed `ReviewTrace` verb, does
two jobs: it proposes redactions for undeclared text and image content (OCR
plus the local vision model), and it writes the step titles. Review is a
sensitive task: it runs on a model on this device unless the user has
explicitly allowed otherwise in Settings. The gateway owns the prompt and the
answer schema, and it rejects any answer that does not address the request.
The model never receives anything the sensitivity layer already marked
`Secret`, since that is never part of a trace. A model in Tilo's own
supervised runtime, which has no network egress, sees the rest of the trace.
Any other model sees each declared `Personal` value, wherever it occurs, as a
typed placeholder such as `‹name›`: those values are redacted anyway, so it
has nothing to add about them.

Review always starts from a deterministic floor (`tilo-feedback-review`):
declared `Personal` values, wherever they are repeated, plus pattern detectors
for email addresses, phone numbers, IBANs and card numbers (checksum-verified),
credential-shaped tokens and secret URL parameters. Model proposals are
merged on top and can never remove or lower a floor proposal. Titles are
masked last, so no title shows a value that is being redacted. Without a
model, the floor is the review, and step titles come from action labels.

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

How the pieces fit:

- **Class.** `ChannelClass` (`collab-types`) is room governance, the
  `channel_class` key of the room document. Only the room's authority writes
  it, and only when the room is created. No member takes a batch that would
  change a class it already holds, so an encrypted room can never turn
  public. Public rooms refuse encrypted ops and every kind of MLS traffic.
  The shell reports the class to members (`tilo-control public-room status`).
- **Standing invite.** A standing invite is the ordinary signed room invite
  with its signed `standing` flag set. It is capped at Commenter, names no
  recipient, and is never consumed. Redeeming it still binds the redeeming
  device's own certificate. Revoking it stops new joins and keeps the people
  it already admitted.
- **Owner and host.** The team's user root owns the room and a team device
  governs it. The host never holds that root. The owner gives the host one
  attenuated delegation: an `InviteToken` whose subject is the host device,
  scoped to this room and capped at Commenter.
  - When a device redeems the invite at the host, the host signs a
    `HostAdmissionV1` and retains it in the room.
  - Every member checks the admission against the room owner before granting
    it, so the room works while the team is offline. A host can admit
    Commenters and do nothing else.
  - Members of a public room do not exchange room state with each other. The
    authority's checkpoint, retained by the host, is that state.
- **Private feedback.** Private feedback uses `DirectRoom::between(reporter,
  team)`. The shell reads the team principal from
  `/usr/share/tilo/community.toml`, or from `TILO_COMMUNITY_CONFIG`. The host
  binds that pair's room on first use and admits exactly those two people.
- **Member by default.** Being in the community is the point of the alpha,
  so nobody is asked. Once a session has an identity, a community is
  configured and the network is up, the shell redeems the standing invitation
  and opens the team's direct room (`ShellNetwork::keep_community`, the one
  path native, other distros and hosted sessions share). It asks the hosts
  again with back-off while they cannot be reached. A refusal by the
  community's host is final for that invitation
  (`tilo.community.refused_invite`); a release that ships another one is
  asked once more. First setup says it in one line.
- **Leaving.** `tilo.community.membership` (`auto` or `left`) is the person's
  choice, kept in the settings store as a personal setting, so it follows
  them to every device. A device that learns `left` leaves the room: its
  workspace goes from the desktop and the device stops following it. Nothing
  joins it again until the person chooses "Join the community". Both are
  offered as shell actions (`community.leave`, `community.join`), in
  Settings > Users and as `tilo-control community leave|join`.

The real community is stood up and run by `scripts/community-ops/tilo-community`
on the team's own release desktop. The governance acts are admin-gated actions
of that desktop, offered in the palette and, for agents and peers holding
Admin, through the action broker: create the public room, delegate it to its
host, mint and revoke its standing invitation, and make someone an Editor.
The tool names each act and then checks what it produced. The delegated
host is room governance (`delegated_hosts_v1`, written only by the room's
authority), and every invitation the room mints names it. It does the pairing, delegation and deployment bundle itself, with the
release binaries, and the team's root never leaves the desktop. The relay
(`os/containers/tilo-relay/community`) receives only its paired room-host
identity and a public record of what it hosts (`tilo-relay-admin community
export` / `install`). `community.toml` is a build input: release images and
packages refuse to build without a configured one.

Votes are reactions: Commenter already means "comment or reaction". Status
(open, planned, fixed in generation N) is a typed chat action that only an
Editor can take. "Fixed in generation N" is matched against the reporter's
image generation, so the reporter is told when their build contains the fix.

## Triage

The triage agent (`tilo-agent --triage`, `apps/agent/src/triage`) is an
ordinary agent member of the community room and of each private direct room.
It is not a backend:

- **Membership.** Workspace Settings › Agents › *Add the triage agent*
  admits it through the ordinary agent admission flow, with the Contributor
  bundle (`workspace_settings::triage_agent_grants`): a membership that lets
  it comment, and `Comment` on Chat. The grant is on Chat, not on
  `#feedback`, because a channel is not an artifact a capability can name.
  What the agent writes is one comment record per reply, each its own
  artifact, and in a direct room it answers conversation messages rather
  than a channel. It runs from a team device (only an administrator may
  launch a delegated agent session) and shows with the `AGENT` label.
  Removing it revokes those grants; the broker tells its Chat at once, and
  the agent stops.
- **How it acts.** It runs Chat headless as its own participant. It reads
  what that Chat holds (`chat::hand`): each report's address (`reference`),
  its recording's summary and its thread, never its prose as an instruction.
  It acts only by asking that Chat for its own published actions. A
  participant that may not write the conversation writes comment records in
  every channel, so the agent's replies are its own records, in `#feedback`
  and in a direct room alike. The broker checks them like any member's, and
  the agent can never write the conversation: no status, no approval.
- **Nobody speaks through its Chat.** Chat's posting actions carry the
  participant tag (`ActionSpec::as_participant`). The broker delivers such an
  action only when the caller is the participant the app runs for, so a
  reporter cannot post in the agent's name by invoking the agent's Chat.

What it does, one report at a time, oldest first:

- **Duplicates.** A report that matches an earlier one gets a reply in its
  thread that links the earlier report and suggests "duplicate of". The match
  weighs the reference target, the app, the recording's apps and layer, and
  word overlap. A model, when there is one, decides among the candidates the
  structure found; it can name no other. The status stays an Editor's.
- **Drafts.** Any other report gets a reply that carries a drafted forge
  issue (`Attachment::Issue`). The draft holds a title, the report's words,
  its address, and the recording's facts: apps, steps, layer and generation.
  Everything in it passes the review floor again, the people the report
  mentions are declared personal, and who reported it is left out.
- **Filing.** An Editor approves a draft with Chat's `approve-issue`. That
  action writes the conversation document, which only an Editor may write.
  The agent then files exactly that draft through the accounts service, over
  the descriptor the launcher hands it (`accounts = "optional@1"`). The
  service holds the vault and the network, and files with the team's
  `forgejo` account, which Settings › Accounts › Forgejo adds with its token
  in the vault. The agent then replies with the issue's address.
- **At most once.** Before it files, the agent writes a durable intent: the
  approved draft it is about to file. The issue carries a marker made of the
  room and the draft. Custody searches for that marker before it files, and
  returns the issue it finds instead of filing another. An agent that stopped
  between filing and replying finds its intent and the issue, and only
  replies.

Each reply carries an address: another report for a suggestion, a draft for
a draft, a `forge.issue` for a filing. These addresses are how the agent
knows what it already did, after a restart or next to another copy of
itself.

The model is reached through the AI hub under the `feedback.triage` task.
Public `#feedback` content is open. A direct room's content is asked for as
sensitive (`assign::resolve_for`), so it runs on this device unless the
person allowed otherwise. Without a model, triage keeps working: the
deterministic match decides, and the draft comes from a template.

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
