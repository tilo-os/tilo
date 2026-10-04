# The demo — shot list

This is the executable demo's shot list: what is filmed and in what order.
Assertions in `scripts/demo/run.py` are the source of truth for whether a beat
passes; this document explains the intended story and the operator's cuts.

One continuous story. Agent work is only ever shown from someone else's
desktop — the point is that the other participants see it happen, not that a
process ran.

| Mark | Meaning |
|---|---|
| ✅ | built and proven live, read back from a session that did not make the change |
| ⏳ | built, proof in flight |
| ❌ | not shootable |

---

## Structure the owner settled on 2026-08-23

**One perspective, one excursion.** Quinten's screen carries the story. There
is exactly one cut to Xaver, placed where it proves the most — his machine
showing the replicated window *and* Quinten's text arriving, in the same shot —
and one cut back. The beats below originally interleaved a switch at nearly
every step; a viewer spends each of those re-orienting rather than watching.

**A caption names whose screen this is**, at the bottom of frame, on every
segment: the person and their relationship to the room. Two machines running
the same OS are otherwise indistinguishable across a cut. It is drawn into the
recording, not added to the shell — a caption, not desktop chrome.

**The agent has no cursor and no cut.** A mouse pointer is an instrument for
pointing at something you are looking at, so giving one to an agent that never
looks at a screen contradicts the claim this video exists to make. The agent's
presence is a caret in the document at the moment it acts, and its arrival is a
chip appearing in Quinten's roster. There is no shot of its "view" because
there is nothing to shoot — and that absence is the demonstration. The run
asserts zero frames come from its session, and fails if any do.

Beat 4's heading below therefore means Xaver's cursor. Beat 6's "cut to Xaver"
is the one excursion, and beat 7 happens inside it rather than after a return.

**The boot bumper ends where the desktop arrives**, not after it: the demo
opens on an empty desktop, so holding one first shows the same picture on both
sides of a cut.

## 1 · Empty desktop, Quinten opens Files ✅

A first boot is genuinely empty — one workspace, no windows, no collaborators,
no invented documents. Files opens from the palette.

There is no on-screen affordance for opening the first window; the palette is
a chord. Accepted for the demo.

## 2 · Quinten invites Xaver ✅

Palette, "Invite peer to workspace". The token is a signed ed25519 blob
carrying the room id, the owner's address and an expiry.

Nobody types it. The invite card mints the token onto the system
clipboard and says so where the host is looking — "Copied — paste it on
the other machine" — beside a **Copy** button; the connect prompt on the
other machine has a **Paste** button that fills itself from the
clipboard, and a **Join** that redeems it. `scripts/demo/run.py` clicks
all four and asserts the round trip: the token that leaves the card is
read back off the clipboard, and the prompt is asserted to hold exactly
it, so a copy that silently truncated fails the join rather than being
papered over.

Both cards also show the same eight-character check code, spaced in two
groups, with "reads the same on their screen" beside it on one machine
and "check it matches the sender's screen" on the other. That is what a
person compares out loud; the token itself is never read aloud.

*(Until 2026-08 this said the token had to be typed. It did, once.)*

## 3 · Quinten invites Claude ✅

A different mechanism, and the more interesting one: an agent is not a
peer. There is no token and no network handshake — it is a capability
principal granted into the room, through the same palette, beside
"Invite peer to workspace".

The grant is real: `CapOverride::membership(Subject::agent(id),
Scope::Room(..), Verb::Write)` through the same entry point the control
socket uses, so it passes the production authority check and replicates
to the room's other members. Revoking it removes the row.

Agents are read back as a separate list from people, not as extra rows in
the roster, so an agent can never be rendered as a person. The roster
draws a dim `agent` tag beside the name — which is why the name itself
can be `Claude` without a reader having to guess what it is.

## 4 · Both join — you see their cursors ✅

A peer's cursor and their name, on the other person's desktop, with
nothing open on either side. Proven from the receiving machine:
`target/desktop-peer-cursor/xaver-sees-quintens-cursor.png`.

The overlay was never the problem — it was mounted and drawing all
along. The session only put its cursor on the wire *as a side effect of
a local app publishing one*, so two people on an empty desktop, which is
exactly this beat, published nothing. The pointer is now level-triggered
from the frame loop, and a stopped one is restated once a second, so
someone who joins after the last mouse movement still sees who is there.

**Open defect: the agent gets a cursor too, and must not.** Level-
triggering is per *session*, and the agent is a session, so it publishes
a pointer it has never moved — a pill reading `Claude` sits on Quinten's
desktop in `target/v1/shots/05-word-processor.png`. Nothing in the demo
script moves it (there is no `agent.pointer_move` anywhere in
`scripts/demo/run.py`), so this is a `shell` fix. The run reports it as a
gap rather than shooting around it. A pointer is an instrument for
pointing at something you are looking at; the agent's presence should be
a caret in the document at the moment it acts, and nothing else.

## 5 · Search "word processor" → not installed → installs → opens ✅

The palette offers apps this machine does not have yet in the same rows as the
ones it does. Installing verifies an ed25519 signature and a BLAKE3 digest,
then runs it.

Timed, on five clean sessions: a median of **1.87s** from the keystroke to the
app's own first frame, spread 1.81–2.18s. That is a 24 MB package over a
loopback registry, on a machine carrying a load average of 5.5.

Fetching and verifying have stopped being the story: 0.07s to fetch, 0.10s to
check the ed25519 signature and the BLAKE3 digest and unpack, and 1.70s — over
nine tenths of the wait — for the app itself to start and paint its first
frame. Shrinking the package further buys almost nothing now; the remaining
second and a half is the word processor's own startup.

It read as 6.7s, and on a busy machine 16–21s, for as long as the payload was
a 449 MB **debug** build — the packaging was moving and hashing eighteen times
the bytes it needed to. Build it with `scripts/demo/apps-live.sh`, which packs
an optimised profile by default; `TILO_DEMO_PROFILE=debug` reproduces the old
number if you want to see it.

Say "about two seconds", never "instant" — a viewer can time this one off the
recording. Re-measure with `scripts/headless-examples/install-open-timing.py`.

## 6 · Xaver's screen — the window, and what Quinten wrote in it ✅

A workspace is shared, so its windows are shared. The room document
carries that room's strips, groups and tabs, each entry written only by
the peer that owns it. Proven on the receiving screen:
`target/peer-window-drawn/xaver-sees-quintens-window.png` — tab pill,
window card and content, not a stub.

What does **not** travel is view state — focus, scroll, active tab.

Two breaks in series had kept this blank. The changed-flag from
`apply_room_changes` was discarded, so nothing bumped the chrome
revision; and once that was fixed, the resync still skipped any strip
with no widget row, which is every strip a peer's window arrives in.
Either alone produces "assertion green, screen blank" — and any
unrelated click that bumped the revision made the window appear, which
is why it looked intermittent rather than broken.

Keep this beat on Tilo's own apps. They replicate through the document
and are drawn from it; a foreign Wayland surface has nothing to
replicate and degrades to a card reading "coming soon".

**In the film this runs after beat 7, not before**, and the proof does
not care: it asks whether a window crossed against a baseline taken
before beat 5 opened one, so it can be asked at any point afterwards.
Asking it here is what buys the single shot the excursion exists for —
the replicated window *and* Quinten's sentence in it, at once, instead of
a cut for each.

## 7 · Quinten's writing appears on Xaver's screen ❌ — not on any frame yet

**Two packaged word processors converge when both are opened from the
palette.** Proven from the receiving side: the `bob-received.png`
shot shows bob's page carrying text he
never typed, with alice's caret and pointer.

That took fixing something subtle. The op stream carries signed
envelopes, deltas and whole documents with nothing on the wire saying
which, so the reader guessed by trying to decode each frame as a signed
envelope and skipping what failed — discarding every unsigned frame,
including the full-state snapshot by which two independently created
documents learn of each other. Presence rides a separate stream and was
never affected, which is why presence crossed while the page stayed
blank, and why this read as a routing bug for days. Frames now carry a
kind tag.

**The `connected=false` asymmetry is fixed.** Quinten's word processor is
the one from beat 5 — installed, then opened — and it used to log
`collab connected=false` where Xaver's, opened from the palette, logged
`true`. The capnp server held no network handle, so a sandboxed app
could not publish at all. Merged 2026-08-23.

**What replaced it: the answer is right and the screen is still blank.**
Run `target/v2` asserts `xaver's own word processor holds what quinten
typed` and passes — and `target/v2/shots/06b-xaver-window-and-text.png`,
taken on his screen seconds later, has both his pages reading
"Start writing…". His screen does not change by one *pixel* across the
whole beat — `06-xaver-window.png` and `08-xaver-received.png` differ as
files (the PNG encoder is not deterministic) and decode identically:
zero pixels moved, against 535,205 for a page that really did gain the
text.

The cause is a broker fan-out. It computes an addressed channel key and
then routes room-wide anyway — visible as `room_wide=true` beside
`addressed=<room>#app:so.tilo.word-processor` in any session log — so a
handshake addressed to the word processor is answered by whatever else
is in the room. `[app so.tilo.file-explorer] recv Arrived; replying full
state 4904B` is the file manager answering for a document it does not
own. The reader therefore gets the text from an app that is not the one
drawn, and the claim passes without Xaver's word processor holding
anything.

**And underneath it, a membership check against the wrong id.** Each
word processor's document has a cluster id of its own, and the check
that decides whether an incoming update belongs to this session compares
it against the *room* ids the session knows. So a document update is
dropped as not-ours. From `target/v3/xaver/compositor.log`:

```
PROBE is_in_room cluster=0b506812-…  in_room=true   known=["0b506812-…"]   x25
PROBE is_in_room cluster=cf8d0374-…  in_room=false  known=["0b506812-…"]   x9
```

`0b506812` is the room; `cf8d0374` is the other machine's copy of the
document. Nine updates arrived and nine were dropped, which is exactly
"presence crosses, the document does not" — presence rides the room
cluster and passes the same check.

The same fan-out defect hangs beat 9: the agent addresses the word
processor, every app in the room answers, and it waits out its timeout.

`scripts/demo/run.py` now hashes Xaver's screen before and after and
raises a gap when the socket says yes and not a pixel moved — so this
cannot pass silently again. **This beat is not claimed until a frame
shows the text on the other machine**, which no frame yet does.

## 8 · An image into the document ✅ — run, but not in this cut

**The launched Files app starts a real client drag now.** The toolkit
grew one: `ui::dnd::start_file_drag` reads the file, offers its bytes
under a mime type, and hands the gesture to the window
(`crates/sdk/ui-core/src/dnd/mod.rs`), and `apps/file-explorer/src/files_app.rs:997`
calls it. The compositor promotes that to a server-side
`wl_data_device` drag, and the receiving client gets the offer and the
drop. `scripts/demo/run.py` asserts both halves off the compositor's own
log — the source handing the gesture out, and the compositor taking it —
so a run where the toolkit quietly reported it started nothing fails
here rather than producing a drag that carries nothing.

*(Until 2026-08 this said neither the app nor the toolkit opened a
`wl_data_source` anywhere, and sent people to the toolbar **Insert
image** route instead. That route still exists and still works —
`apps/word-processor/src/widget.rs:2250` against
`crates/session/shell/src/portal/file_chooser.rs` — it is simply no longer the only
one.)*

What makes the beat worth running is what crosses. Files and the word
processor are two sandboxed clients in two mount namespaces, so the path
one is holding means nothing in the other's world: the drag carries the
file's **bytes**, brokered through a compositor pipe, and neither app is
ever given a path into the other's. The image lands as
`ScalarValue::Bytes` in the document, so it replicates exactly like
text.

**It is run and asserted, and it is not in the one-minute cut** — it is
the longest beat in the script by a distance, most of it spent moving a
pointer slowly enough for two processes to hand a drag between them, and
none of the claims this cut supports is about drag and drop. It deserves
its own minute rather than fifteen seconds of this one.

## 9 · Claude does something ⏳ — blocked on the same broker defect

`tilo-agent` joins the document the way a second copy of the app would:
same `(app, room, record)` route, asks the room for state, holds a live
replica. No window, no pointer. Addressed inside the document — `Claude:
make this a proper document` — it edits through the word processor's
**own exported actions**, under its own actor, attributed in the history
strip.

**A real model chooses the action.** One bounded call, given the
document, the instruction, and the catalogue of actions the app exports;
it answers with one catalogue id and a reason. Validated twice — a
JSON-schema enum built at runtime from that catalogue, then re-checked
against it before dispatch — and the catalogue itself is verified against
the app's own `encode()` names, so the agent cannot offer an action the
app does not have. Typical decision 2.8–4.2s.

A refusal, a timeout, a missing key or an answer outside the vocabulary
falls back to the keyword vocabulary, so the beat always completes. Every
run records which path decided, and the harness asserts it — a fallback
run can never be written up later as a live one. That matters more now
that the name on screen is Claude.

**It does not currently complete.** `tilo-agent` joins, receives the
document, and then waits out its timeout without applying anything —
because every app in the room answers a handshake addressed to the word
processor, so it is offered documents it did not ask for. Same broker
fan-out as beat 7; `scripts/demo/run.py` quotes the `room_wide=true`
lines from the session log when the timeout fires, so nobody has to
re-diagnose a hung process.

Two endings, both live, both attributed, both from before that:

- **The table** — bullets become a formatted table.
  `…/scratchpad/table-03-after.png`
- **The structural pass** — a flat draft becomes a document: title,
  section headings, prose intact. `…/scratchpad/structure-03-after.png`.
  The instruction was "make this a proper document"; it never named an
  action.

The edit arriving on a *second* session is asserted:
`scripts/demo/run.py` reads Xaver's own word processor back over his
session's broker and checks the row text survived into the table, not
merely that a table exists. It is asserted and **not** filmed from his
screen — the same edit lands in the document already on camera, and an
agent's work turning up in *your* document the way a colleague's does is
the claim. The agent's own copy is never the shot, and now cannot be:
the run fails if any frame came from its session.


---

# Things not to say

- **The "Ada" cursor in footage shot before 2026-08-22 is not a person.**
  The compositor painted a fake peer unconditionally so the collaboration
  chrome stayed visible in single-process development. Any cut from
  before that date contains a collaborator who does not exist.
- Not "install is instant". It is about two seconds — median 1.87s on a
  loaded machine — and a viewer can time it off the recording. That
  honesty is the point. (The 6.7s figure that used to stand here was the
  449 MB debug payload; see beat 5.)
- Not that a beat works because its assertion passed. Six of nine beats
  passed every byte-level check today and showed nothing on screen.
- Nothing about beat 7 until a frame shows the text on the other machine.
- Not that the agent "has no cursor" — today it has one, and that is a
  bug being carried, not a property being claimed. See beat 4.


---

# Reproducing

```bash
# The demo rig needs debug-assertions on for TILO_TEST_CONTROL, and
# refuses to record a binary older than the sources under test.
cargo build --profile demo -p shell --bin shell --bin tilo-control
cargo build -p launcher --bin tilo-launcher
cargo build -p agent --bin tilo-agent                # beat 9
cargo build -p app-cli --bins --examples      # beat 5's registry
cargo build -p tilo-install --bin tilo-install
scripts/demo/apps-live.sh                           # the payload it serves

scripts/headless-examples/app-launch-proof.py                 # apps launch and paint
scripts/headless-examples/app-collab-proof.py                 # what crosses between sessions
scripts/headless-examples/room-index-proof.py                 # room contents replicate
scripts/headless-examples/word-processor-convergence-proof.py # two apps converge
scripts/headless-examples/mcp-live-proof.py                   # a model driving the surface
```

Each exits non-zero if a claim fails. A session needs `TILO_APPS_DATA_DIR`,
`TILO_RUNTIME_BUNDLE_DIR`, its own `XDG_RUNTIME_DIR` and `TILO_PEER_KEY_PATH`,
and a short session root — an AF_UNIX path over ~108 bytes fails to bind.
