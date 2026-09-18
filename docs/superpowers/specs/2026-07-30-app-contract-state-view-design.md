# Tilo app contract: state and view

**Date:** 2026-07-30
**Record type:** Historical design rationale. The public app and view interfaces, plus their tests, define current behavior.

## Purpose

An app receives shared mechanisms for UI composition, typed actions and
resources, collaboration, recovery, accessibility, and automation without
requiring every app to reimplement them. Each mechanism has an extension point
for the cases that need one; extending one layer must not opt an app out of the
others.

## Two kinds of state

An app separates its live information into two explicit forms:

| Form | Ownership and lifetime | Rules |
|---|---|---|
| State document | Shared, durable application truth | CRDT-backed; mutations are typed actions; synchronized, undoable, and serializable. |
| View | Local participant context | Serializable for recovery and optionally projected as presence; never CRDT-merged or written into shared state. |

A View contains things such as viewport, selection, caret, open resource, mode,
and pane layout. A participant may explicitly follow another View, but that
moves only the follower's local viewport. Devices are participants, not
automatic mirrors of one another.

## Contract surface

Applications compose shared widgets and expose a resource tree. Typed actions
are the single mutation path for state that needs authorization, undo,
replication, CLI access, or agent access. A custom widget, action projection,
serialization hook, or transport policy may refine an application without
creating an alternate authority path.

Freeze and thaw serialize the state document and local View together. Apps with
nonstandard persistence may provide the serialization hook, while preserving the
same recovery contract. Accessibility nodes and automation projections describe
the same resources and actions that visual interaction uses.

## Invariants

- One action produces one meaningful undo and replication boundary.
- Views and presence never mutate shared document truth.
- Resource identifiers are stable and permission-checked before invocation.
- Freeze/thaw restores a usable local session without publishing private view
  state to another participant.
- Tests exercise action replay, state convergence, view recovery, presence and
  follow behavior through the production path.
