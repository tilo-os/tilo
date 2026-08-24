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

## The word processor cannot be typed into until it is clicked

Opening the app and typing puts nothing in the document. The caret is not
in the body on open, so every keystroke goes somewhere that is not the
page, silently — no caret, no error, no indication that the window which
is plainly focused is not taking input.

Found by a beat that deliberately typed without clicking first, on the
assumption that a document app opens ready to be written in. It does
not, and the cost was not just the beat: a person opening a new document
and starting to type is the most ordinary thing anyone does with a word
processor, and the app's answer is to discard it.

The demo now aims a caret into the page first, which is what a person
would end up doing after the first attempt vanished. The app should not
require it: opening a document should put the caret at the start of the
body, the same way `Ctrl+End` now puts it at the end.

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

## A scrollbar is drawn on an empty desktop

At the right edge, x≈1913 in a 1920-wide frame, there is a white rounded
thumb about forty pixels tall. Sampled off `target/v13/shots/
01-empty-desktop.png`: the run reads (226,226,227) against a (20,20,21)
background — the highest-contrast element in the opening frame apart
from the top bar, and it indicates nothing. There is one strip, no
overflow, and nothing to scroll to.

It is in every frame of the film, so it is not a corner case: it is the
default state of the desktop. On the opening shot in particular the eye
goes to the brightest thing on a near-empty screen, and that is a
scrollbar for content which does not exist.

Worth deciding whether the strip-stack indicator should appear at all
below overflow, and whether at rest it should be this loud when it does.

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
