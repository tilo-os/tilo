# Collaborative presence

**Date:** 2026-06-24
**Record type:** Historical design rationale. Current behavior and protocol details live in the collaboration and presence code.

## Purpose

Collaboration has two deliberately separate planes. The shared document is
durable CRDT state; presence is ephemeral, participant-local context. A cursor,
selection, pointer, focus target, or "where I am" indicator may help another
person navigate, but must not become document content or overwrite their view.

## Model

Presence is scoped to stable UI locations such as a widget, tab, workspace, or
application. A participant publishes only the context appropriate to that scope:
arrival and departure, pointer, caret, selection, focused element, and optional
view position. Identity controls colour, label, and privacy presentation.

The transport may deliver document changes and presence updates on the same
connection, but they have different lifecycle rules. Document changes converge
and persist; presence is replaceable, rate-limited, expires on disconnect, and
is safe to drop under load. A late join receives a document snapshot and only
the current presence projection, never a replay of transient motion.

## Interaction rules

Presence makes collaborators understandable without turning normal activity into
notification noise. Remote pointers, carets, selections, authorship marks, and
focus indicators must be visually distinct from local editing and remain
accessible without relying on colour alone. Following another participant is an
explicit, reversible view action; it changes the follower's viewport, not shared
state.

Applications publish semantic locations rather than screen coordinates wherever
possible. Coordinate data is a rendering detail, recalculated by each local
client, so different window sizes and layouts do not make a participant appear
to point at the wrong object.

## Boundaries and verification

Presence does not grant authority, carry secret content, replace an audit log, or
serve as a persistence mechanism. Authorization applies to the underlying
document action before it is replicated.

Verification covers convergence of the shared document, correct expiry and
replacement of presence, scope isolation, deterministic identity presentation,
and rendering of remote context in real multi-instance sessions.
