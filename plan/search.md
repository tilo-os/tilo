# Search & resource model

Search is an OS-level surface **above** apps. Apps do not own search UI; they
register **resources** and **actions** into a shared registry. One search
primitive renders results, ranks them by context, and either navigates to a
result or runs an action. Two entry points:

- **Ctrl+K** — per-app: the overlay pre-scoped to the active app's resources +
  actions.
- **Super+Space** (or Super+K) — global: shell-level, all apps + full-text FS.

## Resources

A **resource** is an addressable, CRDT-backed thing an app owns and exposes.

- **Resource type** — declared by an app: `book`, `annotation`, `city`,
  `totp-code`, `yt-video`, `cell`, `note`, `contact`, … Each type declares:
  - **identity** — a stable id, unique within its context;
  - **provider** — `query(&str, ctx) -> [Hit]`;
  - **inline renderer** — how a hit displays in results (the live TOTP code, a
    cover + title, a city's current temp) — not just a label;
  - **activate** — default navigation (open the resource).
- **Resource instance** — a concrete resource, addressed `(type, id)` within a
  context. Lives in the CRDT (app doc / shared workspace doc), so it is
  collaboratively editable and syncable.

## Context / scope

Every resource has a **context** bounding visibility + ranking:

- **Global** — always searchable (installed apps, settings).
- **Workspace** — scoped to a compositor workspace (e.g. books opened in this
  workspace; a workspace's files). Search from a workspace prefers/limits to it.
- **App / open-instance** — scoped to a running app or an open resource instance
  (e.g. the annotations of the book currently open).

Context drives **ranking**: current workspace → open apps → manually
added/pinned apps/resources → everything else. Global search aggregates across
contexts but ranks by proximity to the user's current focus.

## Actions

Apps export their **CRDT actions** — mutations, searchable + runnable from the
palette.

- **Type-level (constructor)** — create an instance ("New book", "Add city").
  In scope whenever the type is.
- **Instance-level (CRUD)** — bound to a resource instance ("Add annotation" /
  "List annotations" / "Delete bookmark" on **this** book). In scope only when
  that resource is **in context** (a book is open, or the book instance is the
  selected result).
- Actions are CRDT mutations — running one (locally or from search) applies
  through the same collaborative path. They may take arguments via the palette's
  argument step ("Go to cell: B12", "Add annotation: <text>").
- **Contextual availability is the core rule**: an action surfaces iff its
  required resource(s) are in the current context. Open a book → its annotation
  / bookmark / highlight actions become searchable; close it → they leave scope.

## Nested resources

Resources form a **tree**: a `book` contains `annotation`, `bookmark`,
`highlight` sub-resources; each is itself a resource with its own CRUD actions.

- A parent declares its child resource types. Searching within a parent's
  context exposes the children + their actions ("annotations of this book").
- **Drill-down**: selecting a resource can scope the next query to its children,
  breadcrumbed.
- **UX is make-or-break.** Nesting only works if drill-down + scoping is
  exceptional: a clear breadcrumb of the current scope, obvious up/down, inline
  rendering of nested items, no ambiguity about which context an action applies
  to. If that bar isn't met, cap nesting at one level (a resource + its direct
  actions) rather than ship a confusing tree. This is an explicit design risk.

## Search surfaces

- **Per-app (Ctrl+K)** — the shared overlay pre-scoped to the active app's
  registered resources + actions + the in-context resource's nested
  resources/actions. In-process; fast; no IPC.
- **Global (Super+Space)** — shell-level: aggregates every running (+
  pinned/"added") app's providers, plus **FS full-text** over the P2P FUSE fs,
  ranked by context. Select = navigate (open the app at that content) or run the
  action.

Both surfaces share one overlay component + the per-type result renderers; only
scope + aggregation differ. Fuzzy match over label + description + resource
content.

## Bundle model

- **As a qapp** (on qOS): the search UI lives in the **shell**, not the bundle.
  The app ships only its resource/action **registration** (a manifest + provider
  entry points). Small bundles; the shell renders the overlay.
- **As a legacy-linux build**: compile the in-app overlay behind a `legacy`
  feature (per-app scope only, no global mode), same/similar style. Legacy forces
  the compromise.

## Architecture

- **Registry (in-process)** — a `ui`-level resource/action registry. Providers
  `query(&str, ctx) -> [Hit]`; renderers `render(&Hit) -> Scene`; activators
  `activate(Hit | Action, args)`.
- **Shell aggregator (global)** — the compositor collects providers from all
  running apps over the existing collab/IPC channel (a `Search` request/response,
  like the `CollabRequest` socket), plus an FS full-text index over
  `/var/workspace`. Merges + ranks. The larger, later phase.
- CRDT actions apply through the app's normal mutation path (remote/collab-safe;
  subject to the per-workspace sandbox boundary when invoked cross-app).

## Registration API (illustrative)

```rust
app.register_resource_type(ResourceType {
    name: "book",
    context: Context::Workspace,
    provider: |q, ctx| /* search books */,
    render:   |hit| /* cover + title */,
    activate: |hit| Open(hit),
    children: &["annotation", "bookmark", "highlight"],
    actions:  &[Action::constructor("New book", ...)],
});
app.register_instance_actions("book", |book| &[
    Action::instance("Add annotation", book, arg = "text", |b, a| crdt.add_annotation(b, a)),
    Action::instance("List annotations", book, |b| Scope(children(b, "annotation"))),
    // bookmarks, highlights, delete, …
]);
```

The real API lives in `ui` plus a manifest for the qapp/shell split.

## Concrete resources

| App | Resource(s) | Inline render | Notable actions |
|---|---|---|---|
| ebook | `book` → `annotation`/`bookmark`/`highlight` | cover + title; live note text | New book; Add/List/Delete annotation, bookmark, highlight (in-book) |
| totp | `totp-code` | the **live rotating code** | New account; copy code (activate) |
| weather | `Weather` (city) | city + current temp | Add city; open city (activate) |
| sheets | `cell` | address + value | Go to cell; per-cell edits |
| tube | `yt-video` / `yt-creator` / `yt-playlist` | thumbnail + title | Open; subscribe; add to playlist |
| contacts | `contact` | name + avatar | New; edit; import |

## Open questions / risks

- **Nested-resource UX** — the make-or-break; prototype book→annotations
  drill-down before committing to arbitrary depth.
- **FS full-text** — index cost/incrementality over the P2P fs; freshness.
- **Cross-app action safety** — permissions + the workspace sandbox when running
  another app's action from global search.
- **Ranking** — the exact context-proximity function.

## Phasing

1. Per-app registry + Ctrl+K overlay + resource providers + inline render +
   navigate. Adopters: totp, weather, sheets, ebook (book + its CRUD actions).
2. Instance-level + contextual actions (ebook annotations/bookmarks/highlights
   when a book is open).
3. Global Super+Space + shell aggregation + FS full-text + context ranking.
4. Nested-resource drill-down — gated on the UX prototype.
