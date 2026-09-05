# The qOS app contract: good by default, extendable at every layer

**Status:** design approved 2026-07-30. Implementation follows.

## Purpose

Every qOS app should be *good by default* along five axes, without the app
author writing the machinery for any of them — and yet each axis must be
extendable for the rare app that needs to reach under it. Most apps override
none; nearly none override all.

The five pillars:

1. **Strong UI primitives** — shared `ui` widgets; never hand-rolled.
2. **Resource / action model** — state changes are Actions; the app publishes a
   resource tree. Not obscure per-app logic.
3. **Collaborative** — real-time multi-participant by construction.
4. **(De)serializable** — freeze/thaw the whole app for memory management,
   reboot, and crash recovery.
5. **Accessible** — an AT-SPI tree, *and* drivable as a CLI and by AI agents,
   both of which ride the same action/resource surface.

## Core architecture: two things, one primitive per thing

An app is a **`Widget` tree** that owns exactly two pieces of live state, each
with a single sync-and-serialize mechanism:

### ① The State document — the collaborative truth

- An Automerge **CRDT**. Mutated **only** by `Action`s (`qos_actions`); the app
  publishes what's mutable as a resource tree via `Widget::resources()`.
- **Synced** live to every participant (collaborators *and* the user's own
  devices) as CRDT deltas over the compositor collab transport
  (`SyncIncremental` / `SignedSyncIncremental`).
- **Serialized** with `doc.save()`. Collaboration and freeze/thaw are the *same*
  operation applied over the wire vs. at rest.

### ② The View — a lightweight presence layer, never a CRDT

A plain serializable struct per participant: scroll offset, caret, selection,
which resource is open, viewport, view mode, pane layout. It is **never merged**
and has two independent uses:

- **Broadcast → presence.** Each participant publishes their View so others can
  *see* where they are (when useful) and *attach* to follow them (when they
  want). A device is just another participant here; no View is ever synced *into*
  another View. Presence cursors/carets are a projection of peers' Views.
- **Serialize locally.** `view.serialize()` lets freeze/thaw and reboot restore
  *your* View exactly — not merely the data, the place you were looking. Never
  sent to anyone.

### Everything else is a combination, not a new subsystem

| Requirement | State document | View |
|---|---|---|
| Real-time collab | shared, live-synced | each peer's own; broadcast to presence |
| Multi-device (one user) | synced to your other device | independent per device; devices appear in presence |
| Freeze/thaw + reboot (#4) | `doc.save()` | `view.serialize()` |
| Follow / see-where-others-are | — | attach viewport to a peer's broadcast View |
| CLI / AI agent (#5) | drive via Actions | move the View to "look at" a resource — no state mutation |
| Undo | one boundary per dispatched Action | — (view moves aren't undoable) |

**Explicit non-goal:** Views do not sync between a user's own devices. People use
different devices for different things; the device is a presence participant you
*can* follow, not a mirror.

## The contract surface (the `Widget`/`App` trait, extended)

Each pillar is a hook with a batteries-included default and one override point:

| Pillar | Default (free) | Single extension point |
|---|---|---|
| 1 UI primitives | compose shared `ui` widgets | implement `Widget` for a custom widget |
| 2 Actions/resources | `resources()` → tree; dispatch via `qos_actions` | implement `Action` directly; custom `ActionRegistry` entries |
| 3 Collaborative | State doc synced by the shared transport | custom `ActionTransport` / sync strategy (e.g. server-authoritative) |
| 4 Serializable | `freeze = state.save() + view.serialize()`; `thaw` restores both | override the serialize hook (e.g. a browser serializes its own tab/session state, not a CRDT) |
| 5 Accessible | a11y nodes from the widget tree; CLI/AI from `resources()` | custom `accessibility()` node; custom resource projection |

The point: an app opts into a *deeper* layer without opting *out* of the rest.
A browser can supply custom serialization and still get actions, presence, and
a11y for free.

## What changes concretely (implementation)

Mostly **promotion**, not new invention:

1. **Promote the View to first-class.** Today view state is scattered across
   `get_state`/`set_state` (used only for undo restoration) and the decorative
   `PresenceCtx`. Introduce one serializable `View` type the App owns; capture the
   standard fields (scroll/caret/selection/open-resource/viewport/mode) with no
   per-app code; let a widget contribute custom View fields.
2. **Make presence a projection of Views.** The existing presence broadcast
   (`PresenceCtx`, remote cursors) becomes "publish my View; render peers' Views."
   Add explicit **attach/follow**: bind the local viewport to a chosen peer's View.
3. **App-level freeze/thaw.** `freeze(app) -> bytes = state.save() + view.serialize()`;
   `thaw(bytes)` restores both. One override hook (`serialize_state`/`restore_state`)
   for apps whose state isn't a plain CRDT (browser). Wire freeze into OS
   memory-eviction of idle apps and into reboot/crash recovery.
4. **Tighten pillars 2 & 5 adoption** (the fleet migration, already scoped):
   every app publishes `resources()` and routes interactions through Actions, which
   also lights up CLI + AI-agent drivability for free.

## Testing

- **State:** an Action dispatched through the envelope mutates the doc, records one
  undo boundary, and replays byte-identically on a second doc (collab guarantee).
- **View:** `freeze` then `thaw` restores scroll/caret/open-resource exactly;
  presence renders a peer's broadcast View; attach/follow moves the local viewport
  to the peer's.
- **Serialization:** round-trip freeze→thaw of a populated app equals the original
  (state + view); a custom-serialization app (browser) round-trips via its
  override.
- Assert on the real path — no `#[ignore]`, no weakened assertions (the covenant).

## Sequencing

1. View type + freeze/thaw core in `ui` (this design) — foundational.
2. Presence-as-View-projection + attach/follow.
3. Fleet action-model migration (per `docs/action-model-recipe.md`), which also
   completes pillar 5 (CLI/AI) per app.
4. Desktop ↔ app component unification (parallel workstream).
