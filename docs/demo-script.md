# The demo, by time

A ~60s screen recording. No narration track — the recording carries itself
with the session name and its role on screen, and nothing else. If anything
is said over it, it is said live and in your own words.

Every beat below carries its verification status. **Only shoot a beat marked
✅.** The others are listed so it is clear what is deliberately not shown.

| Mark | Meaning |
|---|---|
| ✅ | proven live, by a script that reads the claim back from a session that did not make the change |
| ⏳ | being built right now; not shootable until its proof is green |
| ❌ | does not work today; do not show, do not narrate |

---

## 0:00–0:10 · One room, three sessions ✅

**On screen:** cut between three full-frame sessions — `ada`, `bob`,
`agent`. ada mints an invite; the other two join. Each shows the same
room.

**Label:** `ada · hosts the room` → `bob · joined by invite` →
`agent · driven only by (provider, id)`

Three real compositors: separate identities, separate keypairs, separate
homes, separate control sockets. Not a screen share.

**Proof:** `scripts/headless-examples/agent-collab-proof.py` §1–2. Each guest
reports the host's room out of its own state.

**Caveat:** joining still needs the invite token typed. The prompt is opened
by address; the token is not a semantic action yet.

---

## 0:10–0:25 · What an agent sees ✅

**On screen:** the action surface, listed. A flat set of addresses —
`actions:window.new.terminal`, `applications:dev.qos.calculator`,
`notes:15` — 126 of them on the owner session.

The point, made visually rather than said: these are **names, not
coordinates**. Nothing in that list is a screen position. No screenshot, no
hit-test.

**Proof:** `action-api-demo.py` asserts the surface is non-empty and that no
entry carries a position.

---

## 0:25–0:35 · Looking changes nothing ✅

**On screen:** the surface enumerated repeatedly; the session does not move.

Enumeration returns inert descriptors carrying no invoke handle, so
surveying cannot cause an effect even by accident.

**Proof:** `action-api-demo.py` §2 — chrome state byte-identical across six
enumerations.

---

## 0:35–0:45 · The agent acts, by address ✅

**On screen:** cut to `agent`. It picks one entry by id and invokes it. The
effect lands. A counter shows **synthetic input events: 0**.

The address is the entire input — no click, no keystroke.

**Proof:** `agent-collab-proof.py` §3; zero-input proven by a counter on the
session rather than asserted.

---

## 0:45–0:55 · The edit appears in the other sessions ⏳

**On screen:** cut to `agent` typing into a shared document via a
parameterised action. Cut to `ada` — the text is there, tinted in the
agent's colour, with its labelled caret. Cut to `bob` — same.

**Status:** NOT SHOOTABLE YET. The agent's edit currently lands only in its
own document. Each session binds a scratch record keyed by its own tab id,
so the three sessions are editing three different documents. Being fixed
now: peers must share one record, edits must converge, and views must stay
independent.

The presence rendering this beat depends on already exists
(`ui/src/presence/render.rs`: caret + name, authorship tint, pointer,
selection, edge mist, stable per-user colour). What is missing is a shared
record for it to describe.

---

## 0:55–1:05 · Claude drives it ✅

**On screen:** Claude Code with the qOS MCP server attached. It enumerates,
picks an action, invokes it, and the desktop responds. Then it asks for
something that does not resolve and is refused — the refusal reaches the
model in the OS's own words.

**Proof:** `scripts/headless-examples/mcp-live-proof.py` — full MCP stdio
framing against a live compositor; the effect is read back over the control
socket, not from the tool's own reply.

---

## Optional closer · The OS scopes what each participant sees ✅

**On screen:** the same enumeration on the host and on a guest. Host: 126
actions. Guest: 6 — no `peer.invite`, no `window.new.terminal`, no
applications.

Showing is never gated (documents and overlay-opening actions stay visible
to everyone); acting is. This is the capability model, not a UI filter — a
hidden action is unselectable, not merely disabled.

**Proof:** `visibility.rs` unit tests plus the observed 126 / 6 / 6 split
across the three live sessions.

---

# Not in the demo

**Apps running.** ❌ 46 apps are registered, but `qos-launcher` is not built
in this worktree, so invoking `applications:dev.qos.calculator` returns
success and spawns nothing. Sessions also see zero apps unless
`QOS_APPS_DATA_DIR` points at the registry. Both are fixable; neither is
fixed.

**qpkg install speed.** ❌ Never measured. To claim "fast" we need a clean
session, a real install, and a number.

**Booting the OS.** ❌ No image is built. `os/build.sh` is a full release
build plus a `sudo` disk step. The `image-boot-proven` branch has a proven
boot-to-desktop; a short pre-recorded clip from it is the cheap honest
option.

---

# Things not to say

- Not "the OS denied the agent by policy" for the refusal at 0:55 — that is
  an address that does not resolve. The capability gate is real, but it is
  the closer beat, not this one.
- Nothing from 0:45 until its proof is green.
- Not "it boots as a real OS" — not today.

---

# Reproducing

```bash
cargo build --release -p shell --bin shell --bin qos-control

scripts/headless-examples/action-api-demo.py     # the action surface
scripts/headless-examples/mcp-live-proof.py      # a model driving it
scripts/headless-examples/agent-collab-proof.py  # three sessions + the agent
scripts/yc-demo.py --record                      # the recorded cut
```

Each exits non-zero if a claim fails. `target/yc-demo/` is wiped on every
run — finished video is copied to `.claude/demo-artifacts/`.
