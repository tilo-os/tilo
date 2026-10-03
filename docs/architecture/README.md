# What Tilo is

Collaborating with people and with AI agents is one problem: both need permissions you
can reason about, visibility into who did what, and the ability to revert it.

That cannot live inside an application — it has to work in the ebook reader, the file
manager and the terminal, not just the document editor. So it lives underneath them, on
CRDT state the system owns.

## Resources, views and actions

**Applications own neither their pixels nor their persistent storage.**

There is one resource / action / view model for every app. An app is a `ResourceApp`
(`crates/sdk/tilo-app`): its state is a `tilo_resource::Resource`, its mutations are that
resource's `Action` applied through the framework dispatcher, and its pixels are a view
(`ViewCx` / `ResourceSurface`) that only reads resource state and submits actions. There
is no second path — no hand-built `tilo_actions::Resource` tree for the same state, no
private `AutoCommit` the widget mutates, no app-local action vocabulary that is not
`Resource::Action`.

### Three layers

- **`tilo-resource`** — a `Resource` type: `read` state, one replayable `Action` type,
  optional named `declarations()`, nested `children()`, optional named `views()`.
  `publish()` is the only bridge from a resource type to the `tilo_actions::Resource` tree
  the palette, agents, scripts and the shell door see.
- **`tilo-actions`** — the protocol. `OperationKind::Document` replays into the CRDT.
  `Effect` and `View` do not. The door refuses non-Document operations ("must be handled
  by the owning application").
- **`tilo-app`** — `ResourceApp` + `ViewCx`. Sync (Automerge, history, per-author undo,
  collaboration) is on for every `ResourceApp`; an app does not implement its own sync
  loop.

### Nested resources

A resource can contain another resource. A rich-text body inside Notes, Chat or Mail is a
child resource (start with rich text), not a hand-rolled map that calls document mutations
directly. `children()` and `publish()` expose the child so it has the same actions,
permissions and history as a root resource.

### Three action surfaces (kept separate)

1. The `Action` enum is the replay format (`RichTextAction::SpliceText` stays an enum
   variant, not a `#[action]` method).
2. `declarations()` is the small named catalogue a person or agent invokes (set mark, set
   style, insert block, new event). Do not declare every positional variant.
3. `apply` / `group` stay the programmatic path for keystrokes and for grouping edits.

`Effect` is only for work with no document delta: starting a sync job, showing a week,
revealing a caret, opening a system file picker. Open/save of a file that lives in the
shared filesystem is a document action on that file resource, not an effect. `View` stays
local presentation (which week, where the caret is).

An app's effects are a typed vocabulary (`#[derive(Effect)]` on an enum) answered by the
view or surface that holds the state they need (`ViewCx::effects`,
`ResourceSurface::effect`). The framework publishes them on the resource with their live
availability, and routes every caller to the handler as a typed value. Long work is a job
(`EffectCx::thread` / `spawn` / `reply`) with progress and cancellation; it completes on the
UI thread and changes the document only through the resource's actions.

### Sync and undo of outside consequences

Collaborators sync the document actions (the CRDT). An external system (CalDAV, the shared
filesystem) is a **projection** drained by an `Effect` that reads queues the document
actions wrote. Peers replay those actions and converge; the Effect on each machine drains
the queue — it is not itself the history, and Effects are not undoable (they have no
heads). The framework runs the drain (`ViewCx::drain`): in the live app after every
change, on every participant's device, never in History; the drain acknowledges each
entry with an action.

If an action's `apply` records an outside consequence in the document (outbox,
`del_outbox`, a filesystem op queue), undo of that action is the **inverse document
action run through `apply`**, so the compensating queue entry is recorded the same way.
Inverse Automerge patches remain correct for pure CRDT actions with no outside
consequence (a text splice). A shared file write and a calendar create are document
actions, so they are undoable — including the compensating remote queue entry.

### Who did it, and what is an undo step

Every action knows its author: the participant that invoked it, pinned with the action
like its time, so every replica's `apply` reads the same one (`ActionCx::author`). A
view knows its reader (`ViewCx::me`). Resource state is the same for everyone; "your
message" is a view comparing the recorded author with `me`, never a reader baked into
the state or an author carried in the payload.

A record's genesis (`Resource::initialize`) is the same change on every device, so two
people creating the same record before they sync converge on one. What a new record
starts with that depends on who creates it or where — the creator as owner, an import
of settings that already exist — is the app's seed: actions applied once after the
genesis, by the creator, and nobody's undo step.

An action declares what it is to the people sharing the document (`change_kind`):
content, housekeeping about it (a read marker), or content from outside (fetched rows,
a process's output). All three sync and stay in history; only content is an undo step.
Edits that continue one another (`Resource::coalesces`: a word typed, a slider dragged)
are one step until the gesture ends.

### Views

A resource can name more than one view (editor, reader) with a stable id. That catalogue
is not an action called `open`. Ordinary presentation toggles never become composable
views by accident.

UI is a suggestion for presenting resources and actions, not the interface itself. An app
can open a resource it never defined and it opens in the right application: URLs or
intents for everything, above and below file granularity.

Displaying a resource is not destructive. Taking an action needs a permission, confirmed
by the user by default.

A view is built for a live window, a past version (History builds the same view again for
every version it previews) or a screenshot. Only a live view starts work that reaches
outside the process — a mail engine, a socket, a journal reader, a hardware probe. A
screenshot scenario is the same view, never a second widget tree: what the resource does
not hold is the scenario's fixture (typed stand-ins for live sources, files, blobs, other
people's versions and presence), prepared before the view is built; the resource is staged
with its actions; view state is staged by the views and surfaces that own it; and the
capture waits until they report nothing left to draw.

Code: `crates/sdk/tilo-resource` (`Resource`, `publish`), `crates/collab/actions`
(protocol), `crates/sdk/tilo-app` (`ResourceApp`, `ViewCx`). Apps register; `shell` serves
the published tree into search, the palette and the control socket.

## What follows

- **Accessibility and automation are the same door.** Screen readers, scripts and agents
  browse resources and invoke actions; none see or click. A capability reachable only
  through a click handler does not exist for them.
- **Search is a system service.** Apps register a search provider and a renderer per
  resource type, so a hit opens the app at that position. See `search.md`.
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
| `compositor.md` | strips, window model, input |
| `window-management.md` | one model for windows, strips and workspaces: chords, gestures, pointer, overlays, actions, motion |
| `collaboration.md` | CRDT sync, presence, peers |
| `rooms.md` | workspaces, hosts, membership |
| `storage.md` | unified storage, sync, records, history and caching (design for review) |
| `security.md` | sandbox, permissions, capability grants |
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
