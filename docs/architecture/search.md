# Search & resource model

Search is an OS-level surface **above** apps. Apps do not own search UI; they
register **resources** and **actions** into a shared registry. One search
primitive renders results, ranks them by context, and either navigates to a
result or runs an action. Two entry points:

- **Ctrl+K** — per-app: the overlay pre-scoped to the active app's resources +
  actions.
- **Super+Space** — global: shell-level, all apps + full-text FS.

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

What the palette shows for an empty query, how rows rank and group, and what a
row may say are in [search-and-history-views.md](../search-and-history-views.md).

## Bundle model

- **As a qapp** (on Tilo): the search UI lives in the **shell**, not the bundle.
  The app ships only its resource/action **registration** (a manifest + provider
  entry points). Small bundles; the shell renders the overlay.
- **As a legacy-linux build**: compile the in-app overlay behind a `legacy`
  feature (per-app scope only, no global mode), same/similar style. Legacy forces
  the compromise.

## Architecture

- **An app's search source** is its resource's: the framework indexes the
  resource's `SearchEntry` values (`Resource::search`) together with what its
  surfaces show beside the document (`ResourceSurface::search_entries`: a
  page's text, live processes, history), and answers bounded, paged queries
  with stable action addresses. An entry carries how its hit reads (result
  group, subtitle, icon, boost) and may carry long prose as a body read in
  place, whose segments can be hits of their own (a reading location). A
  content widget publishes no search provider of its own.
- **Shared desktop search** lives in `crates/session/desktop-search`: the
  result kinds, the per-provider field scores on the shared ranker, the
  cross-provider merge and kind grouping, portable providers (applications
  from the manifest registry, open windows, application actions and search
  providers, recent choices, host commands) and the palette session over the
  toolkit's `CommandPalette`. The browser desktop (`tilo-desktop-surface`)
  presents it; the native shell ranks and groups with the same code.
- **Shell aggregation** lives in `crates/session/shell/src/shell/search/`. Built-in providers
  cover applications, files, editor state, notes, actions, and records; the
  live-app provider obtains registered resources and actions through the
  collaboration broker.
- **Actions** use the descriptors and invocation path in
  `crates/collab/actions`, so palette activation and automation share the same
  validation and permission boundary.
- CRDT actions apply through the app's normal mutation path (remote/collab-safe;
  subject to the per-workspace sandbox boundary when invoked cross-app).

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
