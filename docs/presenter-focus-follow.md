# Presenter focus & follow (spatial screen-sharing)

## The problem this solves

In a shared Tilo workspace, invited peers are already **co-present**: the compositor
streams the same spatial strip of real surfaces to every participant and arbitrates
input (host-wins). Everyone already sees the same windows. So a conventional
"screen share" — capture a screen, re-encode it to H.264, show it as a flat video
tile — is, for co-present peers, both **redundant and worse**: lower fidelity, no
interactivity, and a video *of a thing everyone can already see natively*.

Two genuinely different needs hide under the one word "screen sharing":

1. **Presenter focus/follow** — among co-present peers, "let me show you this" means
   *directing attention*, not shipping pixels: the presenter points everyone at a
   specific surface/group and their views follow.
2. **Flat video share** — for a participant who is **not** co-present (a remote
   contact on another device, a non-Tilo viewer, or a deliberate single-window share
   without granting workspace access), a real encoded video stream is correct.

Need 2 is already implemented and reuses the shared video stack
(`crates/collab/tilo-callstream`, re-exported by the compositor and consumed by chat).
**This document specifies need 1**, which is not built yet. An earlier "Follow host"
toggle was stubbed and removed (the `follow_host_control_no_longer_exists` phase of
`calls_start_join_leave_and_mute_through_shared_voice_membership` in chat) precisely
because it had no real model behind it — this is that model.

## Model

A **focus** is a reference to something already in the shared scene — most often a
`WindowGroup` in the presenter's strip, optionally narrowed to a single tab/surface
or a rectangle within it. It is **not** a pixel buffer; it is a pointer into the
spatial tree every peer already replicates.

State lives on the call/session presence, not in any app's document:

```
FocusPointer {
    presenter: PeerId,          // who is driving
    target: FocusTarget,        // WindowGroup(id) | Surface(id) | Region(surface, rect)
    epoch: u64,                 // bumped on every change, for last-writer-wins
}
enum FollowState { Following, BrokenAway }   // per-follower, local
```

`FocusPointer` rides the **existing presence channel** — the same
`CollabEvent::Context` / `PresenceCtx` path that already carries remote cursors and
per-object presence (see `docs/architecture/collaboration.md` and the app↔chrome boundary). It is
small (an id + an epoch), gossiped like any other presence datum; no media path is
involved.

## Behavior

- **Start focusing.** The presenter picks a group/surface (a "Spotlight this"
  affordance on the window-group chrome, and a "Present this" entry in the call).
  This sets `FocusPointer` on the session and marks the presenter as driving.
- **Follow.** Each follower defaults to `Following`. On every `epoch` bump the
  follower's compositor **navigates its own viewport** to bring the focus target into
  view — reusing the ordinary strip machinery: scroll the strip so the target group
  is active/centered, switch workspace if the target lives on
  another workspace, select the focused tab. Followers see the **real remote surfaces**
  (already streamed for co-presence), not a video — full fidelity, and they can read
  their own peripheral context around it.
- **Break away.** Any follower action that moves their own viewport (scroll the
  strip, switch workspace, focus another group) flips them to `BrokenAway` locally — they
  are never yanked around. A persistent **"Following <name> · Re-sync"** chip shows
  while someone is presenting; tapping it returns to `Following` and snaps to the
  current focus. New `epoch`s do not move a broken-away follower until they re-sync.
- **Hand-off / stop.** The presenter stops (clears `FocusPointer`), or spotlights a
  different group (new target, bumped epoch). Only one presenter at a time; a second
  "Present" request either queues or takes over per the session's host policy
  (host-wins, consistent with input arbitration).
- **Region focus** narrows the follow to a rectangle (e.g. one pane) — the follower's
  viewport zooms/pans to that rect within the already-streamed surface.

## Where the follow control lives

On the **presenter's tile / the spotlighted group's chrome**, not a global toolbar —
"follow the host" is a property of *who is presenting*, so it belongs next to them.
This is the fix for the removed global toggle. In the call overlay, the presenter's
participant tile carries the "Presenting" badge; each follower's tile shows a small
following/broken-away indicator so the presenter can see who's with them.

## What to reuse vs. build

Reuse:
- **Presence transport** — `CollabEvent::Context` / `PresenceCtx` gossip carries
  `FocusPointer`. No new network path.
- **Strip / workspace navigation** — the compositor already knows how to make a
  group active, scroll the strip, and switch workspaces; follow = "navigate to *that*
  group" driven by a remote pointer instead of local input.
- **Co-presence surface streaming** — followers already receive the target's pixels
  because they're in the shared workspace; nothing new is encoded.

Build:
- The `FocusPointer` presence datum + its gossip (small).
- Compositor: apply a remote `FocusPointer` to the local viewport for `Following`
  peers; detect local navigation → `BrokenAway`; the "Spotlight this" chrome affordance.
- Chat/call overlay: the "Present this / Stop", the presenter badge, and the
  per-follower **Following · Re-sync** chip.

## Boundary with flat video (need 2)

Focus/follow requires the target to be **co-present** (the follower already has its
surfaces). When a participant is *not* co-present, or the source is a single window
the presenter won't add to the shared workspace, fall back to the flat
`tilo-callstream` video share (already implemented) shown as the featured call tile.
The call UI chooses automatically: co-present target → focus/follow; otherwise →
video. Both can be active for different participants in the same call.

## Testing

- Presence: a `FocusPointer` set by the presenter reaches a follower's session state
  with the right target + epoch; a stale (lower-epoch) update is ignored.
- Follow: applying a remote `FocusPointer` makes the target group active in the
  follower's compositor state (assert on the real strip/active-group path); a
  cross-workspace target switches the active workspace.
- Break-away: local navigation flips the follower to `BrokenAway`; subsequent epochs
  do not move them; re-sync snaps to the current target.
- Boundary: a non-co-present target routes to the `tilo-callstream` video path instead.

## Status

Design only. The flat-video half (need 2) is implemented and landed
(`tilo-callstream`). Focus/follow (need 1) is unbuilt; this is the spec for it.
