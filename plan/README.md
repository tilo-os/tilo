# What qOS is

Collaborating with people and with AI agents is one problem: both need permissions you
can reason about, visibility into who did what, and the ability to revert it.

That cannot live inside an application — it has to work in the ebook reader, the file
manager and the terminal, not just the document editor. So it lives underneath them, on
CRDT state the system owns.

## Resources and actions

**Applications own neither their pixels nor their persistent storage.**

An app exports **resources** — a video, an ebook, a note, a file. Resources nest: an ebook
contains annotations and bookmarks. For each it exports **actions**: CRUD on an
annotation, turn-page on a book, seek on a video. Files are resources on their own; where
the format-aware filesystem already understands a type, the app does not handle it.

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
  forty. Undo works from an action log that is per-app and per-user, so you can see what
  changed and revert selectively.
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
| `architecture.md` | component overview and system stack |
| `compositor.md` | strips, window model, input |
| `collaboration.md` | CRDT sync, presence, peers |
| `rooms.md` | workspaces, hosts, membership |
| `security.md` | sandbox, permissions, capability grants |
| `search.md` | providers and per-resource-type rendering |
| `agents.md` | agent identity, access, tasks, providers, local models and MCP |
| `../docs/system-extensions-and-local-models.md` | optional runtime/driver packs and one-click local inference |
| `distribution.md` | how software, settings and content reach a machine |
| `accounts.md` | identity |
| `language-checking.md` | spelling, grammar, text actions |
| `os-layer.md` | image, boot, system services |
| `product.md` | who it is for |
| `roadmap.md` | order of work |
