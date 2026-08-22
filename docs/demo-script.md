# The demo — shot list

The technical grounding for every claim is in **`plan/yc-demo.md`**, which
reads each beat against the code. This file is the shot list: what is filmed,
in order, and whether it can be filmed today.

One continuous story. Agent work is only ever shown from someone else's
desktop — the point is that the other participants see it happen, not that a
process ran.

| Mark | Meaning |
|---|---|
| ✅ | built and proven live, read back from a session that did not make the change |
| ⏳ | built, proof in flight |
| ❌ | not shootable |

---

## 1 · Empty desktop, ada opens Files ✅

A first boot is genuinely empty — one workspace, no windows, no collaborators,
no invented documents. Files opens from the palette.

There is no on-screen affordance for opening the first window; the palette is
a chord. Accepted for the demo.

## 2 · ada invites bob ✅

Palette, "Invite peer to workspace". The token is a signed ed25519 blob
carrying the room id, the owner's address and an expiry.

The token has to be typed — there is no Copy button and no paste in the
connect prompt. Fine on camera; only a problem if we claim otherwise.

## 3 · ada invites Claude ✅

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
`target/desktop-peer-cursor/bob-sees-adas-cursor.png`.

The overlay was never the problem — it was mounted and drawing all
along. The session only put its cursor on the wire *as a side effect of
a local app publishing one*, so two people on an empty desktop, which is
exactly this beat, published nothing. The pointer is now level-triggered
from the frame loop, and a stopped one is restated once a second, so
someone who joins after the last mouse movement still sees who is there.

## 5 · Search "word processor" → not installed → installs → opens ✅

The palette offers apps this machine does not have yet in the same rows as the
ones it does. Installing verifies an ed25519 signature and a BLAKE3 digest,
then runs it.

Timed, on twelve clean sessions: a median of **6.7s** from the keystroke to
the app's own first frame, spread 5.0–9.5s — roughly a quarter fetching, two
quarters verifying and unpacking, one quarter starting and painting. That is
a 446 MB debug package over a loopback registry, on a machine that was busy;
most of it is the package's size, not the mechanism. So say "a few seconds",
never "instant" — a viewer can time this one off the recording.
Re-measure with `scripts/headless-examples/install-open-timing.py`.

## 6 · Cut to bob — the window opens there too ✅

A workspace is shared, so its windows are shared. The room document
carries that room's strips, groups and tabs, each entry written only by
the peer that owns it. Proven on the receiving screen:
`target/peer-window-drawn/bob-sees-adas-window.png` — tab pill,
window card and content, not a stub.

What does **not** travel is view state — focus, scroll, active tab.

Two breaks in series had kept this blank. The changed-flag from
`apply_room_changes` was discarded, so nothing bumped the chrome
revision; and once that was fixed, the resync still skipped any strip
with no widget row, which is every strip a peer's window arrives in.
Either alone produces "assertion green, screen blank" — and any
unrelated click that bumped the revision made the window appear, which
is why it looked intermittent rather than broken.

Keep this beat on qOS's own apps. They replicate through the document
and are drawn from it; a foreign Wayland surface has nothing to
replicate and degrades to a card reading "coming soon".

## 7 · ada's writing appears on bob's screen ⏳ — one path works

**Two packaged word processors converge when both are opened from the
palette.** Proven from the receiving side:
`/tmp/qs-gpxaor_7/shots/bob-received.png` — bob's page carrying text he
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

**It does not work yet on the path this demo actually takes.** ada's
word processor is the one from beat 5 — installed, then opened — and it
logs `collab connected=false`, while bob's, opened from the palette,
logs `true`. Same asymmetry across four runs. A peer's caret arrives and
paints; the document never does.

That is a product bug, not a demo one: installing an app and opening it
is what a person does the first time they use anything, and on that path
collaboration fails *quietly* — presence works, so the room looks alive
while nothing is shared. Under fix. This beat is not claimed until a
frame shows the text arriving on the other machine.

## 8 · An image into the document ✅ — but mind which Files

Two different things are called Files, and only one of them can start a
drag.

The compositor's **own** file manager — a widget inside the compositor,
in the first-boot demo group — is the one the built drag works from: the
compositor promotes that in-process drag to a server-side
`wl_data_device` drag the moment the pointer reaches a client, which then
receives the offer and the drop. Proof: `shell/tests/clipboard_e2e.rs`,
`…drops_onto_a_client` and the `QOS_LIVE_APP_DND=1` word-processor lane.

The **launched** Files app — `dev.qos.file-explorer`, a sandboxed Wayland
client, which is what a session opens from the palette and what
`scripts/yc-demo.py` films — cannot. Neither the app nor the toolkit
opens a `wl_data_source` anywhere (`grep -rn 'start_drag\|wl_data_source'
apps/file-explorer/src ui/src` finds nothing but in-process tab dragging),
so there is no client drag for the compositor to promote.

So a recording whose Files is the launched app takes the toolbar
**Insert image** route instead: the word processor asks for a file and
the compositor's own portal chooser answers
(`apps/word-processor/src/widget.rs:2250`, `shell/src/portal/file_chooser.rs`).
That is the better beat anyway — the app cannot read the folder, and what
it receives is the bytes the OS handed it. Either way the image ends up
as `ScalarValue::Bytes` in the document, so it replicates exactly like
text, which is what makes it arrive on the other screen.

## 9 · Claude does something ✅ — two endings, pick one

`qos-agent` joins the document the way a second copy of the app would:
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

Two endings, both live, both attributed:

- **The table** — bullets become a formatted table.
  `…/scratchpad/table-03-after.png`
- **The structural pass** — a flat draft becomes a document: title,
  section headings, prose intact. `…/scratchpad/structure-03-after.png`.
  The instruction was "make this a proper document"; it never named an
  action.

Still to prove: the edit arriving on a *second* session. The agent's own
copy is not the shot.


---

# Things not to say

- **The "Ada" cursor in footage shot before 2026-08-22 is not a person.**
  The compositor painted a fake peer unconditionally so the collaboration
  chrome stayed visible in single-process development. Any cut from
  before that date contains a collaborator who does not exist.
- Not "install is instant". It is a few seconds — median 6.7s — and a
  viewer can time it off the recording. That honesty is the point.
- Not that a beat works because its assertion passed. Six of nine beats
  passed every byte-level check today and showed nothing on screen.
- Nothing about beat 7 until a frame shows the text on the other machine.


---

# Reproducing

```bash
cargo build --release -p shell --bin shell --bin qos-control
cargo build --release -p launcher --bin qos-launcher

scripts/headless-examples/app-launch-proof.py                 # apps launch and paint
scripts/headless-examples/app-collab-proof.py                 # what crosses between sessions
scripts/headless-examples/room-index-proof.py                 # room contents replicate
scripts/headless-examples/word-processor-convergence-proof.py # two apps converge
scripts/headless-examples/mcp-live-proof.py                   # a model driving the surface
```

Each exits non-zero if a claim fails. A session needs `QOS_APPS_DATA_DIR`,
`QOS_RUNTIME_BUNDLE_DIR`, its own `XDG_RUNTIME_DIR` and `QOS_PEER_KEY_PATH`,
and a short session root — an AF_UNIX path over ~108 bytes fails to bind.
