# Demo video — the issue list

Every known defect between here and a finished video. Updated 2026-08-23 13:47,
from the 13:31 recorded run plus frames read by hand.

**An item is done only when a frame shows it.** Code landing is not done — that
distinction is the whole reason this file exists.

## Blocking the shoot

| # | Issue | State |
|---|---|---|
| 1 | Beats 2 and 3 do not use the panel's Invite. The script opens the panel, prints a hardcoded gap claiming no Invite row exists, closes it, and invites by address — so the flow the owner asked for is never on camera | fixing |
| 2 | Stale gap: "joining has no semantic op — the token had to be typed". Paste and Join are both addressable and work | fixing |
| 3 | Stale gap: "the agent cannot change the shared document by address". It can; that text describes a different call | fixing |
| 4 | A window sits 788px past the right edge, and "both windows wholly on screen" fails | fixing |
| 5 | Every other `gap()` in the script is unaudited and may be equally stale | fixing |

## Fixed in code, not yet shown on screen

| # | Issue |
|---|---|
| 6 | A session painted its own name as a peer, over its own document |
| 7 | Placeholder read `dev.qos.file-explorer / waiting for first frame` — now a card with the app's name and a quiet indicator |
| 8 | Windows stacked vertically instead of scrolling horizontally |
| 9 | The roster never listed a peer who joined |
| 10 | Invited and joined looked identical — now an empty seat that fills in place |
| 11 | The invite card showed a 900-character base64 wall |
| 12 | The host's roster showed `peer@…` rather than a name |
| 13 | The Files breadcrumb exposed our own source tree |
| 14 | Files reported "0 items" on a populated home |
| 15 | Files rows responded ~17px above where they were drawn |
| 16 | A `ui` app could not start a drag at all |
| 17 | The drag chip said "Image" rather than the filename |
| 18 | The agent adopted the file manager's document and died on a merge collision |
| 19 | Typed capitals were silently lost on the client keycode path |

## Known, deliberately not fixed

| # | Issue | Why |
|---|---|---|
| 20 | Under load a window shows "This app isn't responding", then the app appears | 10s backstop; the final shoot runs on a quiet box |
| 21 | `execute_snapshot_*` aborts the whole test process | wgpu reads a thread-local during teardown after it is gone; pre-existing, unrelated, and it makes a full-suite green unobtainable for anyone |
| 22 | Apps are 4.16 MB against the letter's "<2 MB" | shared toolkit landed (24.8 → 4.36 MB). The rest is spellcheck's 708 KB embedded word list and moving qdoc into the shared object |
| 23 | A peer's *external* window shows a placeholder rather than streamed pixels | out of scope; every app in this demo is ours |

## Needs the owner

| # | Item |
|---|---|
| 24 | The drag take, driven by hand. Staging is verified in dry-run. Ask only once everything above is clear |

## Architectural findings worth keeping

These came out of building the video and outlive it. None is demo-blocking;
all are real.

**A document change does not say who made it.** `CollabEvent::CRDTIncremental`
has no user field, and the capnp broker stamps unsigned frames with an
explicitly empty `from_user_id`; only `SignedCRDTIncremental` carries an id.
Meanwhile presence says who somebody is and where their caret sits, but not
that anything changed. So the two halves of "who edited what" exist in
different messages and nothing joins them. Closing it needs one of: publishers
moving to the signed path, a user id threaded through `CRDTIncremental`, or an
actor-id-to-presence-id mapping that does not exist today. Until then any
"show me who changed this" feature is an approximation.

**Nothing can redeem an invite without a pointer.** `peer.connect` is declared
with `params: &[]` — it raises the connect card and stops. The only way to
finish joining is to click Paste and then Join. For an OS whose claim is that
an agent never has to look at a screen, that is a hole: the demo had to
synthesise two clicks into the agent's session, and the position those clicks
left behind was then republished to the whole room once a second as a phantom
cursor. A join action that takes the token removes the need for any of it.

**Bytes lived inside the document.** `put_blob` hashed content — and then wrote
the payload into the CRDT under that hash. A 146 KB image was therefore a
146 KB change, over the publish ceiling, dropped; and because Automerge holds a
change whose ancestors are missing, every peer's document froze permanently at
that point while both ends reported themselves connected. The fix is the shape
`fs/` already uses: reference plus metadata in the document, bytes in a
content-addressed store, fetched on demand.

Worth recording alongside it, because it is the reason the same mechanism is
safe in one place and not the other: **chunking a blob is safe in a way
chunking a change never was.** A blob is self-verifying against its hash, so a
lost chunk is detectable, retryable, and degrades one image to a placeholder. A
lost CRDT change is undetectable and freezes the document forever.

**A guessed rectangle standing in for a real one.** `ui/src/widget/conflict_picker.rs:494`
draws its scrim as `self.rect.inflate(2000.0, 2000.0)` in a hardcoded `rgba8`
rather than the theme's scrim token, relying on downstream clipping to bound
it. It works today at the sizes we run. Two ways it does not: a display larger
than the guess, and a palette where a fixed black is wrong. The modal that
landed tonight takes the viewport it is given and reads the token, which is the
shape to copy.

**Focus-ring follow-ups**, on branch `focus-ring-outside-the-control`.

The ring now draws outside the control, 2px band with a 2px gap, clearing 3.0
against every surface in all fourteen presets. Two things it deliberately did
not change:

- **Clipping containers must reserve `FOCUS_RING_RESERVE_PX` (4px).** A control
  flush against `Scrollable`'s clip loses its ring entirely — measured at 0px
  surviving. Changing `Scrollable`'s layout shifts every scrollable in the OS,
  so it wants a deliberate decision rather than a drive-by.
- **`Toggle` has the identical defect** — it strokes `border_focus` on a track
  that is `accent_primary` when on, so a focused on-switch measures 1.00:1. It
  is a one-line adoption of the now-public `stroke_focus_ring`, but a pill at
  `radii.full` is a different geometry decision.

## Rules learned the hard way

- **A stale binary invalidates a run.** Five verifications died this way, twice with an agent judging its own fix against a build that predated it. The rig now refuses to record against one.
- **A stale *payload* is worse.** Rebuilding breaks the hardlink the registry relies on, so the old app is served silently. Copy payloads explicitly.
- **The git index is shared.** Never `git commit` without pathspecs — it sweeps up other agents' staged work.
- **`git add <file>` is not isolation** when two agents are inside one file. Split the diff and stage your own hunks.
- **`TaskStop` does not stop a distrobox-routed build.** The wrapper dies; the container-side cargo keeps compiling and holding memory.
- **A gap describing a limitation that has since been fixed is worse than no gap** — it reports a working feature as broken, and has already sent people to "fix" things that worked.
- **Twelve times in two days the test or the claim was wrong rather than the code.** Make a check fail for the right reason before believing it passes.

## The word processor was not the reason typing went nowhere

A beat that typed without clicking first put nothing in the document, and
that was written down here as the app opening with its caret outside the
body. It is not what the app does, and the entry sent work at the wrong
half of the stack.

Driven directly, the word processor opens with the caret at the first
position of the body, paints it there, and takes keystrokes with nothing
clicked — asserted on the document that comes back out
(`apps/word-processor/tests/opens_ready_to_type.rs`) and read off the
framebuffer, in three palettes, beside the geometry that decides it (the
`opens_ready_to_type` module in `widget.rs`). The widget holds keyboard
focus from the moment it is built, and `ui` never takes it away.

One real caret fault came out of looking: `bind` runs at the top of every
frame and checked the caret against the *body's* blocks, which never
contain a running head's. A caret placed in a header band was dragged back
into the body one frame later, so the band could not be typed into at all.
It now checks the region being edited.

What is still unaccounted for is the shell side of that beat: a client
gets keys only while `client_input.keyboard_focus` names its surface, and
`sync_keyboard_focus` drops it to `None` whenever chrome captures input or
the active tab is not a foreign surface — after which every key is
swallowed with no error and no mark on screen, which is exactly what was
seen. Nothing in the app can put that right.

## A resting pointer stops being published while its owner types

Click into a document, type for thirteen seconds without touching the
mouse, and your pointer disappears from every other participant's
screen. Their view of you goes empty at exactly the moment you are most
active in the room.

Found because a beat asserted a peer's pointer and started failing once
the demo stopped typing its notes twice — the earlier version clicked
between the two passes, which refreshed the pointer and hid this.

The caret is still published and still carries the name, so a peer
writing in a document is not invisible. But a pointer that is merely
resting is not gone, and the codebase already distinguishes those
(`PointerEvidence::Resting` / `Actuated`, `PRESENT_GRACE`). Most
collaborative tools keep a still cursor on screen until its owner
leaves. Worth deciding deliberately rather than by timeout.

Related: the state dump publishes `pointers` and `roster` and no
carets (`shell/src/compositor/native.rs`), so a harness cannot assert
the presence that is actually live while somebody writes — only the one
that is not.

## The invite card has a band of nothing in it

The card now says what it should — a title, the person it is for, when it
expires, the code, and a Copy button — and the explanatory sentences are
gone. What is left is spacing: between the "Copied" line and the button
row there is roughly fifty pixels of empty card, so it reads as
unbalanced rather than composed.

It matters more than a spacing nit usually would because this card is
held longer than almost any other frame: it carries a code somebody has
to read off the screen, so the eye is on it and has time to notice.

Measured at 1920x1014 in `target/v13/shots/02b-copied.png`: card
y≈390..625, "Copied" baseline y≈530, button row centred y≈588.

## Nothing installs a tracing subscriber in the apps

`ui` and every app on it call `tracing::warn!` and nothing collects it.
There is no `tracing_subscriber` installed anywhere in the toolkit or in
the apps, so every warning app code emits is discarded before it reaches
a log.

This is how a refusal stayed inaudible for a day. The app↔shell door
answered two request kinds by warning and dropping them; the warning was
the intended signal that something was unimplemented, and it went
nowhere. The two `WARN` lines that do appear in an app's log come from
the launcher, not from the app.

Anything relying on `warn!` to surface a problem in app code is
currently relying on nothing.

## A proof that forced the transport it was proving

`scripts/headless-examples/photograph-crosses-proof.py` sets
`QUICK_COLLAB_SOCK` in the launched app's environment, which forces the
app onto the legacy JSON collaboration backend. A real session uses the
capnp descriptor; `native.rs` says so in as many words — "in a real
session nothing dials it".

So the script proved content crossing on the one transport that already
worked, while the transport every real session uses dropped the request
on the floor. It passed, repeatedly, against a defect that was total.

Worth auditing the other `scripts/headless-examples/` proofs for the
same shape: a harness that arranges the conditions under which the
answer is guaranteed.

## A screen reader announces one table as four

`accessibility_tree` gathers a table's cells by **adjacency**, the same
assumption the layout pass made until it was fixed. When a peer types
concurrently into the same anchor, the merge interleaves their lines
between the cells — block order is a fractional key, ties break by block
id, and the two replicas zipper — so the cells are no longer adjacent.

Visually that produced four stray boxes instead of one grid, which is
fixed. The accessibility tree still reports four one-cell tables, so
somebody using a screen reader gets a worse version of the same defect
and no way to see that it is one.

Worth fixing with the same shape as the layout fix: gather by the
`table` id the cells share, wherever they sit.

## The agent took every app's deltas as its own — fixed

`Editor::merge_incremental` (`apps/agent/src/apply.rs`) applied a foreign
delta straight into the live document and reported success whenever
`load_incremental` did not error. The agent joins with an empty app id
**by design**, so the broker hands it every application's frames and a
delta belonging to another document was tried against this one as a
matter of course.

Automerge does not reject a change whose ancestors are missing — it
*queues* it and returns `Ok` — so the error was answering a different
question, and three things went wrong at once. It reported an edit that
had not happened, so the agent surveyed and decided on somebody else's
keystroke. Because this replica is asked first and its answer stops the
search, the delta never reached the replica it did belong to, which then
went stale while the agent believed it current. And the parked dependency
can never be met, so `missing_ancestors` — read straight off
`get_missing_deps`, and the thing that makes the agent announce its page
is not whole — stayed non-zero for the rest of the run after one foreign
frame.

Fixed by checking the delta's ancestors against this replica before
anything touches it, on a throwaway load of the delta rather than a copy
of the document — which is also what answers the cost objection that kept
this open: the delta is one edit, the document is a whole page, so no
per-frame document clone is needed. Noticing a genuine hole is
unaffected, because that arrives in a peer's whole-document answer rather
than in a delta; both tests that cover it still pass.

Left standing, and narrower than it looked: a delta carrying a change
with *no* dependencies would still merge cleanly, because automerge
documents have no identity and merging any two is always legal. It is not
reachable through the room as built — a peer's deltas name its own
earlier changes — and separating that case from a genuine hole needs
document identity that does not exist today.

Related, same file: the `pending_ops()` guard in `Editor::apply` is not
the same guard that was removed from `apply_foreign` and cannot lose an
edit — what to send is decided on the heads. What it costs is the wall
clock, and that is real: `AutoCommit` closes an open transaction to
answer `get_heads`, `save_incremental`, `fork`, `merge` or
`diff_incremental`, and the change it writes on the way out carries no
advisory time. Measured — the same edit is stamped through an action that
only writes and unstamped through one that reads first, landing in the
history as `time_ms: 0`, which `VersionMeta` defines as "time unknown".
`commit_stamped` cannot date a change automerge has already made, so
there is no fix at that line; it is now reported when it happens instead
of passing in silence.

## The agent decides on whichever app answered first

`a_discovered_operation_reaches_the_file_explorer_too`
(`apps/agent/tests/room_surface.rs`) fails about one run in five, and the
cause is in the agent rather than the test.

A room answers a late join one document at a time, and the agent acts on
the first arrival that carries a line addressed to it. When the word
processor's page wins that race, the agent surveys a room it has only
half of — `surveyed the room … operations=9 apps=1` — the instruction
names an operation belonging to the file explorer, whose state has not
arrived, nothing matches, and the run ends having written `I can't
"rename plan.md to \"launch.md\"" here` into the page. When the explorer's
state wins instead, the same run reads `apps=2`, finds
`files.entry:…#rename` and performs it. Nothing else differs; adding
enough logging to slow the first frame down flips it green.

The principle is already written down one function away. `act_in_chat`
refuses to consume a summons whose subject names an app it is not holding
— "would be a claim about timing dressed up as a claim about the
request" — and leaves it for the next turn. The document path has no
equivalent: an instruction nothing currently answers is refused
immediately and permanently, and with `once` that is the whole run.

Extending that patience to the document path is the fix, and it is a
decision about when the agent is allowed to say no rather than a
mechanical one, which is why it is recorded here rather than taken. It
has to keep the refusal — an instruction that genuinely names nothing
must still be answered in the page rather than silently outlived — so
what changes is when it is written, not whether.
