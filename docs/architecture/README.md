# What Tilo is

Collaborating with people and with AI agents is one problem: both need permissions you
can reason about, visibility into who did what, and the ability to revert it.

That cannot live inside an application — it has to work in the ebook reader, the file
manager and the terminal, not just the document editor. So it lives underneath them, on
CRDT state the system owns.

## Resources, views and actions

**Applications own neither their pixels nor their persistent storage.**

An app exports **resources** — a video, an ebook, a note, a file. Resources nest: an ebook
contains annotations and bookmarks. For each it exports **actions**: CRUD on an
annotation, turn-page on a book, seek on a video. Files are resources on their own; where
the format-aware filesystem already understands a type, the app does not handle it.

A resource separately exports **views**: editor, reader, map, grid, calendar, timeline.
A view has its own stable identity and human label, and names the actions that open or
mount it. This is an explicit catalogue rather than a convention around an action called
`open`: ordinary presentation actions such as toggling a sidebar never become composable
views by accident. The same resource can therefore appear in several honest forms without
being copied into several app-owned documents.

UI is a suggestion for presenting resources and actions, not the interface itself. An app
can open a resource it never defined and it opens in the right application: URLs or
intents for everything, above and below file granularity.

Displaying a resource is not destructive. Taking an action needs a permission, confirmed
by the user by default.

Code: `crates/collab/actions` — `resource.rs`, `action.rs`, `registry.rs`, `executor.rs`.
Apps register; `shell` serves the result into search, the palette and the control socket.

## What follows

- **Accessibility and automation are the same door.** Screen readers, scripts and agents
  browse resources and invoke actions; none see or click. A capability reachable only
  through a click handler does not exist for them.
- **Search is a system service.** Apps register a search provider and a renderer per
  resource type, so a hit opens the app at that position. See `search.md`.
- **Undo, history, device sync and theming are system services.** One implementation, not
  forty. Undo works from the document's Automerge history: each action records the heads
  before and after it and who made it, and undo writes the inverse as a new change, so one
  person's undo reverts their own action and keeps everyone else's concurrent edits.
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
