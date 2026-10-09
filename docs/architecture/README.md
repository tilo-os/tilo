# What Tilo is

Collaborating with people and with AI agents is one problem: both need permissions you
can reason about, visibility into who did what, and the ability to revert it.

That cannot live inside an application — it has to work in the ebook reader, the file
manager and the terminal, not just the document editor. So it lives underneath them, on
CRDT state the system owns.

## Resources, views and actions

**Applications own neither their pixels nor their persistent storage.**

There is one resource / action / view model for every app, written one way
([resources.md](resources.md)):

- **Model.** State is a `#[derive(Model)]` struct in a CRDT document. Storage
  types declare how concurrent edits merge; apps never call Automerge.
- **Actions.** Every field has a typed path and editing it is a built-in
  action; `#[action]` methods are the real verbs. An action's kind (Read,
  Undoable, Outside) is derived from what its code can do, never declared.
  Outside consequences go through an `Outbox` drained by the owning device.
- **Views.** Views bind controls by path and submit typed actions; forms are
  local; only controls whose paths a change touched re-run.
- **Visible.** Invoking an action makes its view live and runs it there, on the
  strip where the task runs, with the actor present and attributed. Nothing is
  performed out of sight.
- **History.** Entries are structured data (actor, on whose behalf, what, which
  item); sentences are rendered from Fluent in the reader's language.
- **Permissions.** Grants over the published tree, filtered by kind, answering
  Allow, Ask or Deny; enforced at every door and on receipt by relays and
  devices ([permissions.md](permissions.md)).

### Settings, files and secrets

What follows a person or a device but is not shared with a room is not the
document. A preference is a typed `Settings<T>` whose type declares its scope:
`Personal` roams across the person's own devices, `DeviceLocal` never leaves
this one, `Shared` reads a per-user section every app follows (locale,
spelling). Views and surfaces get settings, the app's files and its secrets
from their context; a secret of one document item is keyed by the item's id,
so the document names it and the secret stays on the device. See
`storage.md` §13.6.

### Views, versions and captures

A resource can name more than one view (editor, reader) with a stable id.
UI is a suggestion for presenting resources and actions, not the interface
itself. An app can open a resource it never defined and it opens in the right
application: URLs or intents for everything, above and below file granularity.
Opening something is a record and actions, not a second launcher.

A view is built for a live window, a past version (History builds the same view
again for every version it previews) or a screenshot. Only a live view starts
work that reaches outside the process. A screenshot scenario is the same view,
never a second widget tree: what the resource does not hold is the scenario's
fixture, prepared before the view is built; the resource is staged with its
actions; and the capture waits until views report nothing left to draw.

Code: `crates/sdk/tilo-resource` (`Model`, `Resource`, `publish`),
`crates/sdk/tilo-resource-macros`, `crates/collab/actions` (protocol),
`crates/sdk/tilo-app` (`ResourceApp`, `ViewCx`), `crates/collab/action-policy`
(grants). Apps register; `shell` serves the published tree into search, the
palette and the control socket.

## What follows

- **Accessibility and automation are the same door.** Screen readers, scripts and agents
  browse resources and invoke actions; none see or click. A capability reachable only
  through a click handler does not exist for them.
- **Search is a system service.** An app's resource and its surfaces publish search
  entries, each invoking the action that opens the app at that position. See
  `search.md`.
- **Undo, history, device sync and theming are system services.** One implementation, not
  forty. Per-author undo reverts through the inverse document action when the action
  recorded an outside consequence, otherwise through Automerge inverse patches, so one
  person's undo keeps everyone else's concurrent edits.
- **Collaboration reduces to transport.** See `collaboration.md` and `rooms.md`.
- **Anything reusable belongs in `ui`.** Tabs, presence, text handling. The toolkit sets
  presentation defaults and the user overrides them; the app does not.

## Shape of the desktop

Horizontally scrolling strips of non-overlapping windows, inside workspaces that carry
their own collaborators, permissions and host. `Workspace → Strip → WindowGroup → Tab`.

Strips are borrowed from Niri, not the identity. Do not reason from "spatial OS" — an
early mockup's phrase that leads to Exposé-style surfaces. See `compositor.md`.

A workspace is a unit of collaboration, not screen space. Rendering and state are
decoupled, so one can be hosted elsewhere and render locally. See `rooms.md`.

## Files here

| | |
|---|---|
| `overview.md` | component overview and system stack |
| `resources.md` | the one app model: models, field edits, verbs, kinds, views, visible actions, history, evolution |
| `permissions.md` | grants over the published tree: kinds, answers, delegation, enforcement at doors and on receipt |
| `compositor.md` | strips, window model, input |
| `window-management.md` | one model for windows, strips and workspaces: chords, gestures, pointer, overlays, actions, motion |
| `collaboration.md` | CRDT sync, presence, peers |
| `rooms.md` | workspaces, hosts, membership |
| `storage.md` | unified storage, sync, records, history and caching (design for review) |
| `security.md` | sandbox and capability grants for app code |
| `theming.md` | the theme token contract, validation, layers and precedence, app boundaries |
| `hosts.md` | host interfaces: time, background work, platform services, storage, transport, actions, desktop model |
| `network.md` | `tilo-net`: HTTP, WebSocket, WebTransport, sockets, status, and the fetch-proxy contract |
| `search.md` | providers and per-resource-type rendering |
| `agents.md` | agent identity, access, tasks, providers, local models and MCP |
| `agent-host.md` | agent MCP beside a local enrolled relay on a foreign Linux host (no desktop/OS) |
| `../system-extensions-and-local-models.md` | optional runtime/driver packs and one-click local inference |
| `distribution.md` | how software, settings and content reach a machine |
| `language-checking.md` | spelling, grammar, text actions |
| `os-layer.md` | image, boot, system services |
| `product.md` | who it is for |
