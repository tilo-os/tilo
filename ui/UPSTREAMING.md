# Upstreaming plan — folding recurring app patterns into `ui`

Governed by `ui/HIG.md`: *"if an app can express a bad-UX layout without effort,
that is a toolkit bug."* When a shape recurs across apps, the fix is to promote a
`ui` primitive that bakes the rule in — not to reimplement (and re-violate) it
per app. This file is the ranked backlog of those promotions, newest analysis
first. Each item records **what is duplicated**, **in which apps**, the
**proposed `ui` API**, and **the simplification each app gains**.

Two surveys (2026-07-28) grounded this list: a sidebar/nav audit and a
card/tile/grid audit across all ~47 apps.

---

## Executed this pass

### 1. Card + CardGrid — the canonical grouped-content primitives  ✅ BUILT
**Problem.** `ui` had **no `Card` and no responsive `CardGrid`**. Two shapes were
hand-rolled everywhere, badly and inconsistently:
- a grouped "card" surface — apps drew their own `Container::with_background()` +
  title-row scaffolding, with per-app (often *literal*, not theme) radii;
- a grid of equal cards that reflows by width — every gallery/store/dashboard
  re-copied `cols = floor((w + gap) / (card + gap))` with different magic
  numbers and hand-placed each card, so cards **didn't line up**.

**Where.** Card surface hand-rolled in: settings, backups, browser, calculator,
disks, ebook, store, tube, git_client, maps, weather (~15 apps). Reflow grids
hand-rolled in: camera (two divergent grids, 30 literal radii / 0 theme),
chat (3 grids), ebook, software_store, weather. `StatTile`/`IconGrid` exist but
**no app imports them**.

**API (`ui::widget::card`, re-exported at crate root):**
- `Card::new(body)` / `Card::titled(title, body)` /
  `Card::titled_with_action(title, action, body)` — a themed grouped surface
  (fill, radius, border, padding all from `Theme`) that stretches to its
  column's width so a stack of cards shares edges and lines up.
- `CardGrid::new(cards).with_min_card_width(px).with_card_height(px).with_gap(px)`
  — reflows arbitrary card children into equal, aligned columns.
- `card_columns(avail_w, min_card_w, gap) -> usize` and
  `card_column_width(avail_w, cols, gap) -> f64` — the pure reflow math, exposed
  so **immediate-mode** apps (camera, store, chat) share one aligned formula
  instead of re-deriving it.

**Proof migrations (landed).**
- **settings** `widgets.rs`: `card` / `card_with_action` / `hero` now delegate to
  `ui::widget::Card`; the bespoke `Container::with_background()` + title
  scaffolding and the "known ui gaps" workaround note are deleted. Verified: the
  hero, `card_with_action` (Email + Add + Connected badge) and plain cards render
  identically via the shared surface.
- **camera**: the two divergent inline reflow formulas in the gallery /
  offline-scanner grids collapse onto `card_columns` / `card_column_width`.

**Remaining sweep:** more bespoke-card offenders (software_store featured/app
cards, backups, store, weather) can adopt `Card` / `CardGrid` — see the offender
list above.

### 2. `ui::portal` — host-native file open/save  ✅ BUILT (secondary)
**Problem.** 9 apps each carried a near-identical ~160-line `portal.rs`
(outcome enum + worker thread + private Tokio runtime + `mpsc` channel drained
per frame + `map_uris` + `inject_for_test`) — ~1771 LOC of copy-paste. The
ashpd call itself is already wrapped by `runtime-portal-xdg`, but that crate has
no filters / multi-select and none of the UI-thread integration, which is the
part every app re-wrote.

**Where.** feeds, totp, diffmerge, pdf_viewer, contacts, slides,
word-processor, music, video-player.

**API (`ui::portal`, behind the `portal` cargo feature):**
`FileDialog::open(title)` / `::save(title)` with `.accept_label`, `.filter`
(`Filter::new(name).mimetype(..).glob(..)`), `.multiple`, `.suggested_name`,
`.thread_name`, `.spawn() -> PortalChannel`; the pure `PortalOutcome`,
`map_uris`, and `inject_for_test` compile without the feature so headless tests
drive the exact per-frame drain path.

**Proof migration.** **feeds** deleted its 158-line `portal.rs` and its `ashpd`
+ `url` deps; its portal-drain tests pass against `ui::portal::inject_for_test`.
**Remaining sweep:** migrate the other 8 apps onto `ui::portal` and delete their
copies (~1600 LOC). (video-player still carries its own `portal.rs` — a candidate
for this sweep.)

### 3. Chrome consistency — the OS owns window chrome (HIG §"OS owns window chrome" + §1e)  ✅ DONE
**Problem.** The compositor draws the window title / app-name / logo, yet many
apps *also* painted their own app-name/logo strip at the top of their content —
a duplicated, inconsistent title bar the HIG forbids. Separately, `video-player`
painted a **persistent** 48px top chrome bar (media title + Open) that stayed on
screen even in fullscreen, breaking immersive playback (§1e).

**Executed.**
- Stripped the app-drawn name/logo header from **10 apps** (calculator, monitor,
  software_store, themes, video_editor, tube, disks, firewall, logs, totp),
  keeping every functional toolbar (tabs, search, filters, actions, contextual
  status) and reflowing the remaining controls into the reclaimed space. Where a
  header carried real status (drive summary, firewall state, account count) that
  line was promoted to primary rather than dropped.
- **video-player** is now edge-to-edge: the persistent bar is replaced by a
  floating top overlay (title + Open) that rides the player's controls and
  auto-hides during playback, so immersive video is truly full-bleed. Added
  `VideoPlayer::controls_visible()` so an app can float its own affordance in
  lockstep with the controls.

**Verified** by offscreen shot per app (header gone, controls intact) + each app's
test suite green.

**Not app-name headers (left as-is, correctly):** music/chat (contextual
view/channel titles), camera (viewfinder control pills), and the PARTIAL cases —
containers/remote/ebook draw a per-view *section* title (e.g. "Library"), which
is legitimate content chrome, not an app wordmark. A follow-up could still route
each app's title into OS chrome via `AppWidget::app_title` — blocked today because
the standalone `ui::App` runner doesn't consume `app_title` (only the compositor
does) and none of these apps implement `AppWidget` yet.

---

## Ranked backlog (not yet built)

### 4. Sidebar / NavSplit adoption sweep  — HIGH leverage, LOW risk
`ui` already has the right primitives — `NavShell` (§1a sidebar + §1b
master/detail), `AppShell` (flush sidebar + content + search; narrow = two
screens with a back arrow), `NavSplit` (single responsive split),
`SidebarDrawer`, `PageSidebar`. The gap is **adoption**: ~15 apps re-implement a
`NavSplit`-equivalent flap (hamburger toggle + overlay-when-narrow + flush
dock), several even citing the shared primitive in comments while rolling their
own. **Worst bespoke offenders (fix first):**
- **camera** — a *floating* rounded "glass" nav rail inset 14px, no collapse
  affordance, never becomes a separate screen when narrow. Should adopt
  `NavShell`.
- **backups** ✅ DONE — the Timeline source rail now uses `SidebarDrawer`
  (docked wide / overlay drawer + scrim when narrow), and the tile/card grids
  reflow (summary tiles 4→2→1 via `card_columns`, destination cards regrid +
  compact, schedules stack, top bar compacts). Both immediate-mode painters, so
  the *headless* `SidebarDrawer` (not the `Widget` `NavSplit`) is the right fit.
- **file-explorer** — 1122 LOC of bespoke places-sidebar, fixed 180px, no narrow
  behaviour. Adopt `NavSplit` for the frame.
- **firewall** ✅ DONE — the zone rail now uses `SidebarDrawer` (docked/overlay),
  plus progressive breakpoints (services panel collapses, rules table → two-line
  rows, add-rule form stacks). Also an immediate-mode painter → `SidebarDrawer`.
- Duplication tail (behave OK, just re-implement the flap): api_tester, chat,
  calendar, db_client, code_editor, snippets, containers, disks, feeds, remote,
  contacts, git_client, monitor, software_store, themes.

**Gain per app:** delete a 100–200 LOC hand-rolled flap kernel; get correct
narrow behaviour (separate screens / overlay drawer, never an `X`) for free.
*Note:* `NavShell` (overlay drawer when narrow) and `AppShell` (two separate
screens when narrow) currently encode two different narrow-sidebar rules; a
follow-up should reconcile them so "genuine sidebar" apps get one consistent
behaviour.

### 5. StatTile / IconGrid adoption (dashboards)  — MEDIUM leverage
`StatTile` (metric card) and `IconGrid` (reflow icon grid) exist but **no app
imports them**. monitor, firewall, containers, disks, vpn each hand-draw
caption/value tiles. Migrate them onto `StatTile` (or a `CardGrid` of
`StatTile`s for the reflow case).

### 6. CardColumn / Section (vertical card stack with correct gaps)  — MEDIUM
An intrinsic-height `Column` lays out as a CSS **block** stack, which **ignores
the taffy `gap`** — so every card-stacking app injects manual fixed-height
spacer boxes (settings `vgap`/`cards`/`rows`, etc.) to get uniform spacing. A
`CardColumn` / `Section` primitive that stacks cards with a correct, uniform gap
(via the same internal-spacer technique, once) removes those hacks fleet-wide
and guarantees one consistent vertical rhythm. Complements item 1.

### 7. Shared caret/selection core adoption (HIG §3)  — MEDIUM, per-app risk
`text_input`, `block_editor`, `rich_text_line`, `editable_label`, and
`text_selection` already provide CRDT-backed caret/selection. The gap is
**adoption** by fields that still hand-roll editing — the calculator math field
and the sheets formula bar (both 2-D/structured fields, so migration is
app-specific and higher-risk). Not a new build.

### 8. Draggable scrollbars (cross-app)  — MEDIUM
Scrollbar thumb/track don't respond to click/drag (seen in monitor's DataTable).
Make the shared `ui` scrollbar fully interactive (thumb drag + track paging) so
every app that uses `Scrollable`/`DataTable` gets it.

---

## CRDT / doc-layer cleanup

### What `ui` already gives apps
- `ui::State` (`AutoCommit` doc + undo/redo via `commit_action`, ephemeral state
  restore via `get_state`/`set_state`).
- `ui::doc` — diff-splice CRDT write helpers (`splice_text_diffed`,
  `put_text_diffed`, `put_if_changed`) that avoid the O(n²) `store()` churn.
- `ui::collaboration::CollaborationManager` + `ui::presence` for co-editing.

### The remaining boilerplate every app repeats
Each app still hand-wires the same lifecycle around `State`:
1. build/seed the initial document (schema, sample-vs-empty gating),
2. drain collaboration events and apply remote deltas each frame,
3. decide undo commit points (`needs_commit`/`on_commit` plumbing) and snapshot,
4. save/restore ephemeral selection across undo (`get_state`/`set_state`),
5. persist to / load from the workspace file.

### Proposed shared helper — `ui::AppDocument`
A thin scaffold over `State` so a new app gets **sync + undo/redo + presence +
selection-restore + persistence** for free:

```rust
pub struct AppDocument<M: AppModel> { /* State + collab + persistence */ }

pub trait AppModel {
    /// Seed an empty document's schema (called once on a fresh doc).
    fn seed(doc: &mut AutoCommit);
    /// Pull typed model state out of the CRDT for this frame.
    fn sync(&mut self, doc: &ReadDoc);
    /// Ephemeral (non-CRDT) selection to preserve across undo.
    fn ephemeral(&self) -> Vec<u8> { Vec::new() }
    fn restore_ephemeral(&mut self, _bytes: &[u8]) {}
}

impl<M: AppModel> AppDocument<M> {
    pub fn open(path: &Path) -> Self;      // load-or-seed + wire collab
    pub fn frame(&mut self) -> &mut M;     // drain collab, sync, hand back model
    pub fn commit(&mut self, label: &str); // undo point + persist
    pub fn undo(&mut self) / redo(&mut self);
}
```

Every app collapses its bespoke "load doc → seed-if-empty → drain collab →
sync → decide-commit → save" loop onto `AppDocument::open` + `frame` + `commit`.
This is the same "recurring pattern → framework" move as `ui::doc` and
`ui::portal`, one level up. It also gives a single place to enforce the
diff-splice write discipline (route model writes through `ui::doc`) so the
O(n²) `store()` churn can't regress per app. Build it once the write-side sweep
(item in `apps/MISSING.md`) has a stable set of doc helpers to build on.
