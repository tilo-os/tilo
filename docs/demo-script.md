# The demo, by time

60s screen recording with voice over.

One continuous story, cut between three sessions. Agent edits are only ever
shown from someone else's desktop.

Every beat carries its status. **Only shoot a beat marked ✅.**

| Mark | Meaning |
|---|---|
| ✅ | proven live, read back from a session that did not make the change |
| ⏳ | being built now; not shootable until its proof is green |
| ❌ | does not work today |

---

## 0:00–0:08 · ada, empty desktop, opens the file explorer ✅

**On screen:** `ada` — an empty desktop. He opens the file explorer.

**Proven by:** `scripts/headless-examples/app-launch-proof.py`, which invokes
the app by address through the compositor's own applications provider and
then reads `frame_seq` back off the control socket. Both the file explorer
and the word processor paint.

**What a session has to set.** Isolating documents means repointing
`XDG_DATA_HOME`, and two stores that are machine state live under it:

- `QOS_APPS_DATA_DIR` → `~/.local/share/qos-apps` — without it the session
  enumerates zero applications.
- `QOS_RUNTIME_BUNDLE_DIR` → `~/.local/share/qos-runtime` — without it every
  launch fails on the runtime bundle the manifest pins.

**Still off:** the session runs with `QOS_LAUNCH_NO_SERVICES=1`, so apps
launch without the collab service. Handed a working collab descriptor an app
spins at ~86% CPU and never presents its first frame — see 0:34–0:44, which
is the beat that needs it.

---

## 0:08–0:16 · He invites agent and bob; both join ⏳

**On screen:** ada mints an invite. Cut to `bob` and `agent` joining.

**Working:** the invite/join flow itself, verified from the far side.

**Blocked on:** the token still has to be typed — there is no semantic join
action. For the recording that is a keystroke, which is fine; it is only a
problem if we claim otherwise.

---

## 0:16–0:22 · You see their cursors ⏳

**On screen:** back on ada — bob's and the agent's pointers and carets,
each in its own colour, with names.

**Have:** the rendering exists and is good — `ui/src/presence/render.rs`:
`draw_peer_caret_labeled`, `draw_peer_authorship`, `draw_peer_pointer`,
`draw_peer_selection`, `draw_peer_mist`, `user_color`.

**Blocked on:** the publish side reaching peers, and something shared for the
presence to be *about*.

---

## 0:22–0:34 · He searches for a word processor — it isn't installed — it installs as a qpkg, fast ❌

**On screen:** ada searches. The word processor appears as something the
machine does not have yet. He picks it; it installs in about a second and
opens. He starts writing.

This is the beat that carries "apps arrive without ceremony".

**Blocked on:** two things. Offering not-yet-installed apps in the same rows
lives on the `install-by-search` branch (another session owns it, not landed
here). And install speed has never been measured — we cannot say "fast"
until there is a number from a clean session.

---

## 0:34–0:44 · Cut to bob: the window opened there too, and ada's writing is appearing ⏳

**On screen:** `bob` — the same word processor window is open, and
ada's text lands live as he types, in his colour, with his caret.

Two separate pieces of state, and both must replicate:
- **which app is open** — the workspace's contents
- **the document** — the text

The user's rule: *the view does not follow, the state does*. bob keeps his
own scroll, selection and focus.

**Blocked on:** two things, both being built now.

*The document.* The shell is a relay, never a participant: shell-hosted tiles
write into `RecordStore` but nothing calls `App::with_records`, so typing
into one produces no network traffic at all, and an inbound record change is
fanned out to sockets without ever being loaded into a document. The demo's
word processor is an *external* app and already has that bridge, so this beat
may work through the real app path without the shell tiles being fixed.
Being tested.

*The window.* A workspace is shared, so a window opened by its host must open
for everyone in it. Today it does not: the per-room document projects only
`room`, `name`, `owner` and `capabilities`, while strips, groups and tabs
live in the tree-wide document, which is never exported. That export rule is
right — the tree-wide document holds *every* workspace, so exporting it would
leak the other rooms — but it is an argument about that document's shape, not
a reason a room's own members should not see the room's own windows. The fix
is to project this room's contents into this room's document, beside the four
fields already there.

---

## 0:44–0:52 · bob drags an image from the file explorer into the document ⏳

**On screen:** bob drags an image out of the file explorer and drops it
into the document.

**Blocked on:** everything above, plus drag-and-drop between two sandboxed
apps, and an image landing in the shared document as content rather than a
local path — a path would resolve on his machine and nowhere else.

---

## 0:52–1:00 · Cut back to ada: the image is there ⏳

**On screen:** `ada` — the image bob dropped has arrived.

Closes the loop: both directions, both kinds of content.

---

## The agent's beat · placement TBD

Unresolved: what the agent should *do*. It should be something a person
would plausibly want done and would not want to do by hand — not a toy.

Candidates, cheapest first:
- it is invited like a person and its cursor appears alongside the others
  (fits 0:16, needs nothing beyond presence)
- it edits the shared document by address while both humans watch it happen
  in their own sessions (fits 0:34, needs the same machinery as ada's
  typing)
- it acts on the dropped image — describing or filing it

Whatever it is, it must be shown **from ada's or bob's desktop**, never
from the agent's own. The point is that the other participants see it happen,
not that a script ran.

---

# What has to land, in order

1. **`qos-launcher` built, and apps actually launching.** Nothing above works
   without it — no file explorer, no word processor.
2. **Real-time document collaboration + presence.** Beats at 0:16, 0:34, 0:52.
3. **Workspace-contents replication** — "the window opened on bob too".
4. **Install-by-search + a measured install time.** Beat at 0:22.
5. **Drag-and-drop into a shared document, as content.** Beat at 0:44.

---

# Already proven, currently unused by this cut

These were verified live and are available if a beat needs them:

- **The action surface.** 126 typed addresses on the owner session, nothing
  positional. `action-api-demo.py`.
- **Enumeration is side-effect free.** Six enumerations leave state
  byte-identical.
- **A model driving the OS over MCP** — enumerate, invoke, refuse, with the
  effect read back over the control socket. `mcp-live-proof.py`.
- **The capability gate.** Owner 126, guests 6 — showing never gated, acting
  gated. `visibility.rs` plus the live three-session split.

---

# Things not to say

- **The "Ada" cursor in any footage shot before this was written is not a
  person.** `shell/src/shell/overlay_host.rs` painted a deterministic fake
  peer unconditionally — "this keeps the collab chrome visible in
  single-process development" — and nothing gated it. It is in the 57s cut at
  `.claude/demo-artifacts/qos-demo-57s.mp4` and in every screenshot taken
  before it was removed. Do not show that footage, and do not point at a peer
  cursor as evidence of anything until it has been re-shot.

- Not "it boots as a real OS" — no image is built.
- Not "install is fast" — never measured.
- Nothing about an agent being denied by policy in the same breath as an
  address that simply does not resolve. Both are real; they are different.

---

# Reproducing

```bash
cargo build --release -p shell --bin shell --bin qos-control
cargo build --release -p launcher --bin qos-launcher

scripts/headless-examples/action-api-demo.py     # the action surface
scripts/headless-examples/mcp-live-proof.py      # a model driving it
scripts/headless-examples/agent-collab-proof.py  # three sessions collaborating
scripts/yc-demo.py --record                      # the recorded cut
```

Each exits non-zero if a claim fails. `target/yc-demo/` is wiped on every
run; finished video is copied to `.claude/demo-artifacts/`.
