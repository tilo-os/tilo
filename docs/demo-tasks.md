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
      in text, the app of its last action. Proximity decides the
      *subject* of a request that states none; it never decides whether
      to act. `Carried` beats `Recent` beats refuse, and recency is
      measured **message-to-action**, not against the wall clock, so a
      loaded machine cannot change the reading.
      **Measured 5 of 8 green.** The three red all pass their
      `acted_on()` assertion — the agent does the right thing to the
      right resource — and fail on the reply. Diagnosed to transport,
      not apply, by counting answers *put on the wire* against replies
      the room can see: `1` against `[]`. Fix committed, **unverified**:
      its run was blocked in turn by three other agents' in-flight
      breakage in shared crates.
- [~] **Chat across peers** — messages replicate and are attributed;
      `@mention` is structured, not styled text. Mention list landed
      (`0f87b4afe`); replication unverified.
- [~] **Say something to a selection** — select anything, right-click,
      compose beside the context menu; the message carries the
      resource address, and a channel selector shows where it is going.
- [x] **An image editor exists at all** — and the brief that asked for
      one was wrong. `apps/imager` is the **Disk Imager**, a ~5,800-line
      USB/OS-install wizard; its history has never been anything else,
      and adding photo editing to it would have destroyed working
      functionality. There was no image viewer anywhere in the fleet,
      but `ui::widget::ImageViewer` existed in the toolkit and no app
      used it. So `apps/photos` (`dev.qos.photos`) now opens, fits,
      rotates, crops, copies to the clipboard and saves.
      Two calls worth keeping: a crop is stored in the **original's**
      coordinates, so it keeps meaning the same region however often the
      image is later turned; and **copy is deliberately not an action**,
      because an action replays on every peer and a clipboard write
      modelled as one would reach into the selection of every machine in
      the room. The document holds a path plus crop and rotation and
      never the pixels — asserted under 4096 bytes with a 4000x3000
      image open.
      **The demo scripts still name an image editor that does not
      exist**; they need `dev.qos.photos`.
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
      Storage is now a map keyed by invite id, but a name is still
      **spent by the first peer to redeem** — see the agent's chip
      under "Open, on the payoff frame", which is the same fault seen
      from the other end.
- [~] **Typing before clicking is discarded** — the word processor has
      no caret in the body on open, so the first thing anyone does with
      it silently does nothing.
- [x] **A mark over an empty desktop** — I called it a scrollbar; it is
      the strip-stack *focus* indicator (`StripRibbon`), one segment per
      strip. The count was measured correctly all along: `draw` merely
      skipped `count == 0`, so a lone strip painted its single focused
      segment at full weight. Now hidden below two strips, gated inside
      the widget rather than at the paint — gating the paint alone would
      trade a visible mark meaning nothing for an invisible one that
      still takes clicks. Resting weight is a ladder now (idle 46 <
      hover 96 < focused-rest 150 < focused-hover 224), so maximum
      contrast is spent only under the pointer. Measured in all three
      themes, before and after.
- [x] **Clicks moved no pointer** — and the cause was worse than a
      teleporting click. **The compositor drew no pointer at all in the
      headless path**: the only code that renders a cursor into the
      scene ran solely during drags. DRM has a hardware cursor plane and
      winit sits under the host's pointer, so headless was the one
      backend with nobody drawing one — and it is the backend that gets
      photographed. The snapshot path had the same hole for its own
      reason: it re-renders the scene, so no backend's cursor plane is
      ever in a readback. Clicks now travel through one shared planner,
      so there is no longer a way to plan a click that does not first put
      the pointer where it is about to happen. Proven on film: travelling
      at 5.45s, arrived on the control at 5.65s, press ring at 5.86s.
- [x] **Six seconds before the first app** — **my diagnosis was
      wrong** and the measurement said so: it was not mostly hold. The
      beat was 1.06s of empty desktop, 0.85s to a window frame, then
      2.2s of the compositor's opening spinner, *ending while it was
      still spinning* — the first app was never on screen inside the
      beat at all. Two real causes: `launch()` returned on the first
      committed buffer, which is not "the window is up"; and applying
      the strip share on the first frame invalidated the buffer the
      client had just drawn, buying a second round trip of placeholder.
      Files is now fully rendered at 3.4s where the baseline was still
      spinning past 3.9s. Separately measured and stated: real launch
      latency is 1.9s unrecorded, 3.3s under recording, because
      snapshotting is ~78% of a filmed compositor's loop — the camera's
      cost, not the product's.
- [x] **Boot bumper** — cut from existing footage, 3.2s, pillarboxed
      to 1920x1080, joined ahead of the demo at exactly 100.000s.

## Verified open, awaiting confirmation

- **Four peer-pointer tests have never executed.** The fix is proven on
  film (glyph counts, above) and its `ui` unit tests pass 104/104, but
  `shell/src/shell/peer_pointer_tests.rs` has been blocked from running
  by a succession of other agents' transient compile breakage. Proven by
  frames is better than proven by tests alone, but this wants both.

## Found in a sweep — one cleared, two still live

I recorded these as cleared on the strength of a run reporting "every
claim held". That was wrong: **the claims do not check these things.**
Reading the frame afterwards showed two of the three still present. The
lesson is the one already written below and worth repeating because I
walked into it anyway -- a green run proves the assertions somebody
wrote, not the picture.

- [x] Stray bordered boxes between paragraphs. Genuinely fixed; the
      document now renders prose then bullets, in order.
- [x] **The agent's refusal quotes an empty string** -- `I can't ""
      yet.` Both halves are one fault and both are fixed: the
      instruction really was empty when it was read, because the line
      was still being typed, and the reply appeared to share the
      instruction's block because the rest of the keystrokes arrived
      after it. See "The one that was actually on the frame" below.
- [ ] **`peer@143` and `peer@728` in the page margin** -- the
      unresolved-name fallback, twice, on the page rather than in the
      roster.

### Why that sweep proved less than it looked like it did

The recording filmed a **six-hour-old agent**. `target/demo/qos-agent`
was built at 04:04 while five of its sources -- including the new
`listen.rs` -- were newer. The refusal text in the frame is the *old*
string listing the *old* four-entry vocabulary, on a branch where the
catalogue had already been replaced by runtime discovery.

`scripts/demo-apps-live.sh` refreshes the app payloads the registry
serves. **It does not rebuild the agent.** So a sweep that rebuilds the
shell and runs that script still films a stale agent. Rebuild
`-p agent` explicitly as part of every sweep.

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

## Verified by looking, this round

- **An agent is distinguishable from a person, in both surfaces.** The
  roster gives it a bot glyph and an `agent` tag beside its name, with
  the same `editor` role the people have -- a different *kind* of
  member, not a lesser one. The top-bar stack could not show an agent at
  all before: it derived from the peer list, and an agent holds
  membership by capability. It now does, and because a glyph does not
  survive a 15px chip (the eyes merge, the antenna goes), the
  distinction there is **silhouette** -- a squircle among circles, which
  reads even overlapped.
- **Four different applications in one tab group** -- Notes, Files,
  Terminal, Document on one tab bar. That is the property no other
  desktop has, since a tab elsewhere belongs to its application. Worth
  building the strip beat around.
- **The strip ribbon appears when it should**: present on a workspace of
  three strips, absent on one.

## Open, on the payoff frame

Found by reading a frame of a run that reported **every claim held** --
the claim is "the document was written into", and it was, with this:

> Claude: I can't "make this a run sheet" here. I can: Insert table
> (word-processor.document#insert-table); Insert page break
> (word-processor.document#insert-page-break); Align left (...); ...

- [x] **Raw action addresses written into human prose.** Cleared, and
      checked by looking rather than by reading the fix. Refusals now
      name operations in the app's own words, two or three of them, and
      the address goes to the log line beside it (`fa3134e9c`). Every
      way of not answering carries labels: `NoMatch` holds them,
      `plan::unmatched` lowercases them, `describe_offers` caps the list
      at three. **Verified on the real path**: `no_addresses_written`
      fails if `#` or a resource id reaches any block, and it runs inside
      four passing `room_surface` tests, two of which exist to provoke a
      refusal.
- [x] **"make this a run sheet" is refused, and it used to work.** The
      cause was not the recipe. `Recipe::from_keywords` has known
      `run sheet` since `d6f677aac`, and
      `an_editorial_change_is_still_answered_on_the_document_path` now
      passes end to end over the broker the compositor binds: the
      bullets become a table carrying their own text, with no refusal.
      What actually produced the frame was **the line being read while
      it was still being typed** -- see below. The frame that recited
      six addresses was a *stale* agent besides; the binaries it was
      recorded against predate both fixes.
- [!] **The agent's own chip reads `peer@764`** while `xaver` beside it
      resolves. `2e5ef06ef` is a real fix for a real fault -- a peer can
      no longer write its own name -- but it is **not this one**, and it
      takes away the only thing that was accidentally covering it.

      The mechanism is read off the code rather than guessed, and it is
      not the one written under "second invitation loses its name"
      above. Storage is already per peer *and* per invitation:
      `room_peer_names` is a map, and so is
      `ShellNetwork::pending_invites`, keyed by invite id. The fault is
      that **a name is spent by whoever redeems first**.
      `name_invitee_on_arrival` looks the arriving peer's quoted
      invitation up, names them, and then `pending.remove(invite)` --
      and in the demo xaver and the agent's session redeem **the same
      token** (`yc-demo-live.py::bring_the_agent_session_in` says so:
      "The same token: it is a bearer credential for one room"). xaver
      arrives first and takes the name; the agent arrives second, finds
      nothing pending, and stays a number. Which is exactly the frame:
      one chip resolved, the one beside it `peer@<n>`.

      So the fix is not more slots. It is either a second invitation
      minted for the agent's session under its own name, or a name that
      a bearer token does not hand to the first arrival and then forget.

      **Not verified by looking** -- reading the chip needs a full
      three-session run, and this is being worked separately.

### The one that was actually on the frame

- [x] **A line still being typed was read as a request.** `Claude:` is a
      complete address with nothing asked after it, and it exists for as
      long as it takes to reach for the next key. The agent is handed a
      delta per keystroke and re-read the page on every one, so it
      answered that -- which is where `I can't ""` came from. The empty
      quotation was never a formatting fault: the instruction really was
      empty at the moment it was read, and the reply landed on the
      instruction's own block because the remaining keystrokes carried on
      arriving after it.

      The second half is worse because it is silent. Reading the line
      also marked it handled, so the request the person finished typing
      was **never looked at again** -- a run that produced the empty
      refusal and nothing else looks, in a log, exactly like a run that
      had nothing to do.

      This is why the scripted film rendered correctly against the same
      binaries that failed live: `yc-demo.py` types the instruction,
      settles, snapshots, and only then starts the agent. `yc-demo-live.py`
      starts the agent first and lets a person type, which is the
      arrangement that gets filmed.

      Fixed: an addressed line becomes a request only once it has stopped
      moving, nothing is consumed before then, and an address with
      nothing after it is not a request at all. Proven by a test that
      types the instruction one character at a time, with a pause after
      the colon, across the broker both transports share.

The lesson, again and at cost: a claim proves the assertion somebody
wrote. "The document was written into" is true of a refusal, of a wall
of jargon, and of the best sentence in the film. Only the frame tells
them apart.

And this time the mechanism is known rather than guessed at. The live
rig's claim came off `applied .*` in the agent's log -- and a refusal is
applied too, as one `SpliceText`, logged in the same words as the table.
The scripted rig's came off `qos-agent` exiting 0, which a refusal also
does. Neither could ever have failed on any of these three. The live
rig now asks the document instead: the run sheet's content is on the
page, and no refusal is.

### What the model actually does now, measured

Six live calls against the demo's own page, through the gate a real run
uses (`live_model.rs`, which surveys real addresses off the word
processor's tree rather than a stand-in):

- **6 of 6 chose the run sheet and drafted it.** `["Time","Item"]`,
  doors 18:00, sound check 18:30, and the seating figure **left out** —
  which is the judgement the beat is about, and the thing a split on
  separators cannot make, because `Seating: 240` has a colon in it like
  the others.
- **5 of 6 raised the contradiction.** "The prose says doors open at
  seven but the notes say 18:00, and the sound check at 18:30 falls
  after doors open, which would not work in practice." That is the
  second half of the payoff, and it is not certain: one run in six
  drafted the table and said nothing. The schema already carries every
  lever anyone found for this — the field is required, nullable and
  described — so the remaining variance is the model's, and the honest
  statement is a rate rather than a property.
- **1 of 6 drafted a blank row**, `["", ""]`, which passed every bound
  the gate applied. Fixed above; the five runs after it are clean, and
  whether they were clean because the fix dropped a blank row or because
  none was drafted, the page cannot tell the difference and neither can
  this note.

So the beat works. What it is not is deterministic, and a take should be
watched rather than assumed.

### Verified red, and not by me

- **`a_discovered_operation_reaches_the_file_explorer_too` fails.** The
  rename never reaches the explorer's shared buffer. Inherited untested
  and failing at this commit; the settle window above is provably a
  no-op for it (the page is seeded whole, so the instruction is neither
  empty nor moving). Cause not established.
- **Four agent test targets had stopped building**, one of them for the
  same reason `2e5ef06ef` exists: `ChatDoc::seed_starter` lost the
  arguments that let a peer name itself. Fixed by having the *room* name
  the participant, which is load-bearing rather than cosmetic --
  `me_in_chat` resolves the agent's own id by matching that name. A
  `--lib` run compiles none of these.

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
