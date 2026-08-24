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
- [!] **Invite button missing from the collaborators panel** —
      regression from the composer rebuild. `Join` survives, `Invite`
      does not. Found by driving the real path, not by any unit test.
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
      front of the launch, not the launch itself.
- [x] **Boot bumper** — cut from existing footage, 3.2s, pillarboxed
      to 1920x1080, joined ahead of the demo at exactly 100.000s.

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
