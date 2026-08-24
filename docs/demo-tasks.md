# The demo: what is being built, and what is done

Working list for the rebuilt film — four applications, two people, an
agent acting across them. The beat plan it serves is written as
pseudo-code at the top of `scripts/yc-demo-live.py`; defects found and
not yet scheduled live in `docs/demo-todo.md`.

Status: `[ ]` not started · `[~]` in flight · `[x]` landed and verified
on the real path · `[!]` landed but unverified, or regressed.

## Features the beats depend on

- [x] **External actions door** — a participant that is not the app can
      enumerate its resources and invoke its actions, capability-gated,
      attributed to the caller. `ecd8752d2`.
- [x] **Agent reaches any app** — the hardcoded four-entry catalogue is
      replaced by runtime discovery of what the room's apps publish;
      the answer is validated against the live surface *at dispatch*.
- [~] **Agent notices what is near it** — `@mention` in chat, its name
      in text, the app of its last action. Proximity decides where it
      looks, never what it may do.
- [~] **Chat across peers** — messages replicate and are attributed;
      `@mention` is structured, not styled text. Mention list landed
      (`0f87b4afe`); replication unverified.
- [~] **Say something to a selection** — select anything, right-click,
      compose beside the context menu; the message carries the
      resource address, and a channel selector shows where it is going.
- [~] **Image editor usable** — open, edit visibly, get the result out.
      Crop/rotate/save glyphs landed.
- [~] **Four applications on the strip, and tabbing** — two in frame at
      a time; one tab group holding two *different* applications.

## Defects on the demo path

- [x] **Peer pointer duplicated per window** — the key was discarded at
      the capnp door and re-synthesised; position now carries the space
      it was measured in. **Verified on a real recording**: one glyph at
      40s, one at 50s having moved, none at 60s on the owner's own
      screen. Before the fix, two at identical y, 810px apart while
      their tiles were 812px apart. That the glyph *moves* is what
      proves it is tracking a pointer rather than being a duplicate that
      happens to be single.
- [x] **Invite composer hand-rolled** — rebuilt on the shared text
      input and buttons; both explanatory sentences removed.
- [x] **Invite button missing from the collaborators panel** —
      regression from the composer rebuild, fixed. Verified on the real
      path: `Invite at [1652,154,118,28], Join beside it`. It broke no
      unit test and was found only by driving the path a person takes;
      the top-bar affordance kept working throughout, which is why a
      spot check would have passed.
- [~] **Second invitation loses its name** — pending names were one
      slot rather than one per outstanding invitation, so a second
      invite overwrote the first and the redeemer became `peer@<n>`.
- [~] **Typing before clicking is discarded** — the word processor has
      no caret in the body on open, so the first thing anyone does with
      it silently does nothing.
- [~] **Scrollbar over an empty desktop** — a white thumb indicating
      content that does not exist, in every frame.
- [x] **Clicks moved no pointer** — a scripted click now travels to
      what it clicks, so the film stops looking like it operates itself.
- [x] **Six seconds before the first app** — the opening hold was in
      front of the launch, not the launch itself. Now measured at 1.4s
      from asked-for to on-screen.
- [x] **Boot bumper** — cut from existing footage, 3.2s, pillarboxed
      to 1920x1080, joined ahead of the demo at exactly 100.000s.

## Verified open, awaiting confirmation

- **Four peer-pointer tests have never executed.** The fix is proven on
  film (glyph counts, above) and its `ui` unit tests pass 104/104, but
  `shell/src/shell/peer_pointer_tests.rs` has been blocked from running
  by a succession of other agents' transient compile breakage. Proven by
  frames is better than proven by tests alone, but this wants both.

## Found in a sweep, since cleared

Seen in frames of a rehearsal against 04:01 binaries; **all three are
gone on current HEAD**, confirmed by a full run in which every claim
held. Kept as a record of what a sweep catches that a log does not.

- Stray bordered boxes between paragraphs, with the opening prose drawn
  inside one of them.
- The agent's refusal quoting an empty string: `I can't "" yet`, while
  the instruction plainly said what it wanted.
- `peer@131` in the page margin -- the unresolved-name fallback.

## Found in a sweep, to verify against current HEAD

Seen in frames of a rehearsal recorded against 04:01 binaries, so each
may already be fixed by work that landed since. Verify before fixing --
inventing a fix for a defect that is gone is its own waste.

- **Stray bordered boxes between paragraphs**, with the opening prose
  drawn inside one of them, low and to the right instead of first.
  Dispatched. Not the image placeholder -- that is gated on a non-empty
  hash. Note the scripted film against the *same* binaries rendered
  correctly, so it may be specific to the path the live script takes.
- **The agent's refusal was malformed**: `I can't "" yet` -- an empty
  quoted string where the instruction text should be, while the
  instruction plainly said "make this a run sheet". The refusal also
  appears to have landed on the same block as the instruction rather
  than its own. The refusal path has since been rewritten around
  discovered offers, so check whether either survives.
- **`peer@131` in the page margin** -- the unresolved-name fallback,
  which is the second-invitation defect showing up on the page rather
  than in the roster. Already dispatched.

## Standing hazards, learned the expensive way

- **Legacy transport hides defects.** Three separate total defects have
  passed their tests because those tests exercised the legacy JSON
  collaboration dialect rather than the capnp one every real session
  uses. Any test or proof touching collaboration is suspect until shown
  to traverse capnp.
- **Component tests do not prove the product.** The missing Invite
  button broke no unit test; the peer-pointer fix passed its own tests
  while the live path stayed broken. Drive the whole path.
- **A capability the screen does not show is not in the demo.** Before
  filming any beat: what is on screen that would be different if the
  capability were absent? Two beats have already been rebuilt for
  failing that question.
- **Stale artefacts lie.** A rebuilt binary does not update the payload
  the registry serves; a shot on disk may predate the fix you are
  judging. Check timestamps before believing a frame.

## After every landing

Sweep the whole demo path — run it, read the frames, look for what
moved. The two worst defects found so far were both found this way and
neither had a failing test.
