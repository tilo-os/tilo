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

## 1 · Empty desktop, Quinten opens Files ✅

A first boot is genuinely empty — one workspace, no windows, no collaborators,
no invented documents. Files opens from the palette.

There is no on-screen affordance for opening the first window; the palette is
a chord. Accepted for the demo.

## 2 · Quinten invites Xaver ✅

Palette, "Invite peer to workspace". The token is a signed ed25519 blob
carrying the room id, the owner's address and an expiry.

The token has to be typed — there is no Copy button and no paste in the
connect prompt. Fine on camera; only a problem if we claim otherwise.

## 3 · Quinten invites the agent ✅

A different mechanism, and the more interesting one: an agent is not a peer.
It is a capability principal (`agent:<id>`) granted through the permission
model, and it reaches the document as a broker client.

## 4 · Both join — you see their cursors ✅

Real presence across machines: caret, pointer, selection and the run a peer
just typed, each in that peer's colour with their name. Proven by a screenshot
taken from the *receiving* machine, the peer labelled by its own id.

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

## 6 · Cut to Xaver — the window opens there too ✅

A workspace is shared, so its windows are shared. The room document carries
that room's strips, groups and tabs, each entry written only by the peer that
owns it.

What does **not** travel is view state — focus, scroll, active tab. Xaver sees
the window arrive without his own view moving, asserted on the same object in
the same run.

## 7 · Quinten's writing appears on Xaver's screen ⏳

The last piece. Deltas and presence cross, but two independently created
documents had no way to learn of each other: the only full-state frame was
fanned out locally and never republished. A snapshot path to the room closes
it. Proof in flight.

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

## 9 · The agent does something ✅

`qos-agent` joins the document the way a second copy of the app would: same
`(app, room, record)` route, asks the room for state, holds a live replica. No
window, no pointer. Addressed inside the document — "Scribe: turn the bullets
into a table" — it edits through the word processor's **own exported actions**,
under its own actor. Its changes are attributed to it in the history strip and
its caret appears in the others' windows while it works.

Its vocabulary is scripted, and the honest framing is the stronger one:
anything that can speak an app's actions can drive that app without competing
for the screen. That is the resource and action model paying out, not a model
call.

---

# Things not to say

- **The "Ada" cursor in footage shot before 2026-08-22 is not a person.** The
  compositor painted a fake peer unconditionally so the collaboration chrome
  stayed visible in single-process development. It is in
  `.claude/demo-artifacts/qos-demo.mp4` and in every screenshot from before it
  was removed. That cut needs re-shooting regardless — it also predates apps
  launching.
- Not "install is fast" — not measured yet.
- Not "it boots as a real OS" in this cut. The image builds in CI; no boot
  footage is part of this.

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
