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

### 0. One Ctrl+K palette over the resource/action model  ✅ BUILT
**Problem.** `ui` carried *two* parallel Ctrl+K stacks. `ui::App`'s Ctrl+K opened
an `ActionPalette` that bridged the resource/action model (`qos_actions`
`Resource`/`ActionSpec`, published via `Widget::resources`) into the older
single-provider `CommandPalette`. Separately, the richer "find things" layer —
`ui::search` (`SearchProvider`/`SearchRegistry`) + the `SearchResultsView`
overlay + `SearchField` — existed but was **never wired to Ctrl+K**; contacts
even declared a `ContactSearchProvider` that reached no surface. Two registries,
two result widgets, two type→filter→activate flows.

**Unified.** `ActionPalette` is now the single Ctrl+K surface: a shared
`SearchField` over a shared `SearchResultsView`, driven by the resource/action
model as the foundation.
- The focused app's exported `ActionSpec`s form the **Actions** group; activating
  one dispatches through `qos_actions` (locus-honouring, undo-recorded — the same
  path a peer replays). A `Prompted` action collects its argument in the field.
- Apps fold in searchable content via `Widget::search_providers()` — each
  `SearchProvider`'s hits render as their own group (e.g. **Contacts**), and
  activating one calls `Widget::activate_search_result()` so the app navigates
  (presentation, not a CRDT action). The two outcomes are drained separately.

`CommandPalette` stays as a general widget for the surfaces that still use it
directly (shell global search, containers, sheets); it is simply **out of the app
Ctrl+K path** — no second registry, one renderer, one flow. `ui::search` is
untouched (the shell still drives its own compositor-level search through it).
Proof: the `ctrl_k_palette` scenario in `cargo run -p ui --example ui-shots -- --list`
(`ui/examples/ctrl_k_palette.rs`) renders the unified palette (Actions +
Contacts groups) offscreen; app-level tests in `ui/src/app.rs` prove both an
action-dispatch and a content-navigate through the real path; contacts/ebook/tube
tests exercise the unified `ActionPalette` API.

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
chat (3 grids), ebook, store, weather. `StatTile`/`IconGrid` exist but
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

**Remaining sweep:** more bespoke-card offenders (store featured/app
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

**Completed migration.** Every app-local `portal.rs` has been removed. Dialog
configuration now lives beside the action that consumes it while worker
threading, URI validation, single/multi channels, save-name seeding, and test
injection all come from `ui::portal`. The sweep includes the original nine
apps plus the later adopters (ebook, hex_editor, sheets, typst_editor,
video_editor, and vpn); vpn's older direct `ashpd` client and its private
`ashpd`/`tokio`/`url` dependencies were removed as part of the convergence.

### 3. Chrome consistency — the OS owns window chrome (HIG §"OS owns window chrome" + §1e)  ✅ DONE
**Problem.** The compositor draws the window title / app-name / logo, yet many
apps *also* painted their own app-name/logo strip at the top of their content —
a duplicated, inconsistent title bar the HIG forbids. Separately, `video-player`
painted a **persistent** 48px top chrome bar (media title + Open) that stayed on
screen even in fullscreen, breaking immersive playback (§1e).

**Executed.**
- Stripped the app-drawn name/logo header from **10 apps** (calculator, monitor,
  store, themes, video_editor, tube, disks, firewall, logs, totp),
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
  contacts, git_client, monitor, store, themes.

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

---

## 2026-07-29 — comprehensive cross-app redundancy audit

A grep-level sweep of all 47 app crates (plus `crates/`) for the *same thing
implemented N times*, ranked below. Each row cites `file:symbol` evidence, not
impressions. The last column flags whether the concurrent **`dedupe-io`** session
is likely already handling it: that session's landed signature is
**file/persistence/settings/uri** consolidation — it shipped
`crates/system/atomic-file` (22 hand-rolled crash-safe writes → one),
`settings-store` (the shared `settings.json`), `accounts` (Mail/Calendar/Settings
account model), `app-state` (the canonical Automerge-doc-on-disk home), `power`,
and a single `uri` percent-decoder. So **value-formatting, avatars, dialogs, icons
and widget-adoption are NOT in its lane** and are the genuinely-open, highest-
leverage items here.

### Ranked table (lead with genuinely-open, high-leverage)

| # | Pattern (duplicated code) | Apps duplicating (count + names) | Suggested shared home | Effort | In dedupe-io sweep? |
|---|---|---|---|---|---|
| A | **Value formatting** — byte sizes, byte-rate, media timecode, "time ago", compact counts, month/weekday names. No shared home exists (`crates/format/*` is WIT *format-providers*, unrelated). | **byte-size: 13** (`api_tester model.rs:human_size`, `imager model.rs:fmt_size`, `maps app.rs:fmt_bytes`, `backups model.rs:human_bytes`, `camera app.rs:human_size`, `chat render_chat.rs:human_size`, `containers model.rs:human_bytes`, `disks model.rs:fmt_size`, `file-explorer model.rs:human_size`, `hex_editor view.rs:human_size`, `mail model.rs:human_bytes`, `monitor model.rs:fmt_bytes`, `vpn model.rs:human_bytes`+`human_rate`). **timecode: 6** (`music model.rs:fmt_duration`, `tube util.rs:clock`, `video_editor timecode.rs:clock`+`timecode`, `camera app.rs:fmt_dur`, `slides app.rs:present_clock`, `maps geo.rs:fmt_duration`). **relative-time: 8** (`camera app.rs:ago`, `feeds model.rs:relative_time`, `git_client repo.rs:relative_time`, `vpn model.rs:human_ago`, `mail util.rs:format_relative`, `chat model.rs:day_label`, `calendar date.rs:relative_label`, `tube ytdlp.rs`). **compact-count: 5** (`music` ×3: `backend/live.rs:human_count`, `app/render.rs:human_count`, `search_type.rs:compact_count`; `tube util.rs:human_count`; inline in `monitor`/`store`). **month/weekday tables: 5** (`calendar`, `logs`, `mail`, `sheets` ×2 `month_abbrev`+`month_abbrev_local`, `tube`). | **New `ui::format`** module: `bytes()`, `rate()`, `duration()`/`timecode()`, `relative()`, `compact_count()`, `month_abbrev()`/`weekday()`. | **M** | **no** — pure formatting, outside dedupe-io's file/persist lane. **Highest leverage + a correctness win:** the copies *disagree* — `mail`/`backups` label base-1024 as `KB/MB/GB`, `containers` as `KiB/MiB/GiB` for the identical math, so the fleet renders the same size two ways today. A shared formatter makes it consistent, not just shorter. |
| B | **Avatar circle + initials + deterministic color** — hand-drawn, even though `ui::widget::Avatar` already ships `from_initials`, `color_for_initials`, `paint_initials`, `display_initials`. **[RESOLVED 2026-07-29 — see note below.]** | **8** hand-roll it (only `tube` uses `ui::Avatar`): `chat render_chat.rs:draw_avatar`+`identity.rs:{initials_of,color_from_bytes}`, `contacts app.rs:draw_avatar`+`model.rs:initials`, `git_client draw.rs:avatar`, `mail draw.rs:{draw_avatar,avatar_color}`+`model.rs:initials`, `music app/render.rs:draw_avatar`+`art.rs:initials`, `pdf_viewer app_draw.rs:draw_avatar`, `totp list.rs:draw_avatar`+`model.rs:initials`, `calendar ics.rs:color_from_name`. | **Adopt existing `ui::widget::Avatar`** (extend it with an image-backed variant where needed — `mail`/`music`/`tube` want a photo fallback). | **M** (adoption) | **no** — widget adoption, not persistence. Same "primitive exists, apps re-roll it" shape as the Card sweep. |
| C | **Confirmation / modal dialogs** hand-painted, though `ui::widget::ModalDialog` exists (6 adopters: db_client, mail, notes, pdf_viewer, terminal, totp). | **~16** hand-draw a confirm/dialog/overlay sheet: browser, code_editor, contacts, disks, file-explorer, git_client, hex_editor, imager, logs, monitor, slides, store, themes, vpn, weather, word-processor. | **Adopt `ui::widget::ModalDialog`** (add a `confirm(title, body, destructive)` convenience for the delete/format/discard case that recurs most). | **M** | **no** — not mentioned in this file yet; widget adoption. |
| D | **Per-app config/state dir + JSON read/write** — each app re-resolves its own `dirs::config_dir()/qos/<app>/…` and hand-writes serde JSON. | **~12**: `file-explorer config.rs`/`favorites.rs`, `db_client session.rs`, `store config.rs`, `settings prefs.rs`/`region.rs`, `totp storage.rs`, `browser settings.rs`/`history.rs`, `vpn config.rs`, `themes app.rs`, `music storage.rs`, `mail account.rs`, `contacts storage.rs`, `chat storage.rs`. | **Extend `crates/system/app-state`** (already the canonical *Automerge-doc* home) with a sibling `app_config::<T: Serialize>` path-resolver + atomic JSON store (built on the new `atomic-file`). | **M** | **maybe** — directly adjacent to dedupe-io's `app-state`/`settings-store`/`atomic-file` work; they may fold this in. Confirm with that session before building. |
| E | **Icon set fragmentation** — `ui::Icon` (66 named icons) + `IconWidget` exist and 14 apps use them, but two apps declare a *private* `Icon` enum and ~8 hand-draw vector glyphs into the scene. | Private enums: `containers draw.rs:enum Icon`, `music icon.rs:enum Icon`. Hand-drawn vector icons: `camera paint.rs:icon`, `containers draw.rs:icon`, `backups app.rs:draw_{source,dest}_icon`, `code_editor file_tree.rs:draw_{folder,file}_icon`, `contacts app.rs:draw_calendar_glyph`, `maps canvas.rs:draw_category_icon`, `pdf_viewer app_draw.rs`. Emoji/`&'static str` glyph maps: `chat model.rs:glyph`, `db_client model.rs:glyph`+`icon`, `maps model.rs:glyph` (×4), `mail model.rs:icon`, `file-explorer icons.rs:icon_for`. | **Grow `ui::icons`** to cover the missing glyphs (folder/file kinds, category pins, source/dest kinds) so apps drop private enums + bespoke painters. | **M–L** (nuanced; some are genuinely app-specific pictograms) | **no** — but note an *active* thread elsewhere is already replacing emoji glyphs with vector icons (`osk` ⇧/⌫/⏎ landed), so coordinate. Partially overlaps item 5 (StatTile/IconGrid). |
| F | **Clipboard** — `ui::{clipboard_copy,clipboard_paste,clipboard_copy_image,clipboard_copy_primary}` route text and images through the installed compositor/Wayland backend, with X11 and headless fallbacks. Word Processor's four text sites now use these helpers while retaining its rich-fragment sidecar. | No production app instantiates `arboard::Clipboard` directly; `terminal` only does so in tests. | Complete. Keep the root re-exports as the concise widget-facing API; `ui::clipboard` remains the backend boundary. | **S** | **no** |
| G | **Remote-image fetch + decode** — `ui::Fetcher` + `ui::ImageStore` exist (adopted by chat, mail, music, slides, tube); three network apps fetch+decode by hand. | **3**: `weather`, `feeds`, `maps` fetch images with `reqwest`/`ureq` and decode without `ImageStore`. | Adopt `ui::ImageStore`/`Fetcher`. | **S–M** | **no** |

### Item B resolved (2026-07-29) — avatars adopt `ui::widget::Avatar`

The hand-rolled coloured-circle + centred-initials rendering is gone from the
apps that hand-rolled a real person/roster avatar; they now paint through the
shared `Avatar::draw_at` immediate-mode helper (the same one `tube` already
used), so hue, circle geometry, and glyph centring are one implementation.

Shared-widget additions (additive to `Avatar`, no other `ui` surface touched):
- `Avatar::initials_for_name(name)` — the name→monogram step (first char of the
  first two words, uppercased). Apps did this word-split inline; `draw_at` /
  `from_initials` only *truncate*, so a full name would have yielded the first
  two letters (`"Ad"`) instead of the per-word monogram (`"AL"`). Tested.
- `Avatar` initials now render **Semibold** (both `draw_at` and the widget's
  `paint_initials`) — the weight the apps used before adopting the helper, and
  crisper on a small chip.

**Converted (6):**
- **contacts** — `app.rs:draw_avatar` now delegates to `Avatar::color_for_initials`
  + `draw_at`; the local `DefaultHasher`-over-initials colour is deleted (it was
  byte-identical to `color_for_initials`). `model.rs:initials` **kept** — it's
  domain logic (first-name + last-name fields, not a word split).
- **git_client** — `draw.rs:avatar` body replaced by `initials_for_name` +
  `draw_at`; threads `theme` through (added a param; both call sites had it).
  `author_color` **kept** — a genuinely-needed per-email hue so two authors with
  the same initials stay distinct; passed straight into `draw_at`.
- **mail** — the initials-disc branch of `draw.rs:draw_avatar` now uses `draw_at`.
  The Gravatar **image branch is kept** (genuine photo fallback), as is
  `avatar_color` (per-email hue) and `model.rs:initials` (email/name-aware).
- **pdf_viewer** — `app_draw.rs:draw_avatar` now uses `color_for_initials` +
  `draw_at`; the local FNV-over-name colour (only 4 hues) is deleted, so
  commenter discs now span the full accent ramp.
- **totp** — `list.rs:draw_avatar` now uses `draw_at`. `account_color` **kept**
  (the colour is **user-assignable** per account) and `model.rs:initials` **kept**
  (issuer/label domain logic); the colour is passed into `draw_at`.
- **chat** — `render_chat.rs:draw_avatar` now paints the disc via `draw_at` and
  `identity.rs:initials_of` delegates to `initials_for_name`. Kept, deliberately:
  the **per-peer palette colour** (`palette_color`/`color_from_bytes`, which gives
  each peer a distinct hue *and* is shared with the workspace-tint colouring — not
  an initials hash) and the **presence dot** (chat-specific extra, drawn by the
  same helper after the disc).

**Left local, with reason (2):**
- **music** `app/render.rs:draw_avatar` — **not** a flat initials chip. It's a
  procedural **two-tone HSL** disc (`art::art_colors`) with a decorative accent
  blob and themed (non-white) initials, deliberately matching the procedural
  album-art tiles beside it; its photo path already uses the shared
  `ui::image_store::paint_image_cover`. Forcing the flat accent-ramp `Avatar`
  would regress that visual and break consistency with the album grid.
  `art::initials` stays as the art-system's monogram helper.
- **calendar** `ics.rs:color_from_name` — **false positive**: it maps an ICS
  `X-QOS-COLOR` name to a `Palette` enum for event colours. Calendar has no
  person/roster avatar at all.

Proof: `--shots` of **contacts** (initials-hash colour path), **chat**
(per-peer palette + presence dots), and **totp** (user-assignable colour) —
inspected; discs are circular, monograms centred + Semibold, colours
deterministic/assigned, presence dots intact. contacts' two "AL" discs (list +
detail) sampled to the identical fill `(199,210,254)`, confirming determinism.
All six converted crates' suites pass (`cargo test -p <app> --locked`), plus the
new `Avatar::initials_for_name` unit test in `-p ui --lib`.

### Refresh of existing backlog items (from this sweep)

- **Item 4 (sidebar adoption)** — updated tally: **15 apps now adopt** a shared nav
  primitive (api_tester, backups, calculator, containers, disks, feeds,
  file-explorer, firewall, mail, music, pdf_viewer, settings, slides, tube,
  weather), and **13 still hand-roll** a rail/flap: **calendar, camera, chat,
  code_editor, db_client, ebook, git_client, monitor, notes, snippets,
  store, themes, video_editor**. `file-explorer` and `firewall`/`backups`
  called out earlier have since adopted; `camera` remains a bespoke floating rail.
- **Item 7 (caret core adoption)** — no new *clean* offenders beyond the noted
  calculator/sheets structured fields; `word-processor`/`typst_editor` carry
  genuinely bespoke editors (rich-text / code) that are higher-risk migrations,
  not quick wins.

### Not an extraction — a theme-read fix (HIG §theming), noted for completeness

Hard-coded `Color::rgb*` literals (widgets must read from `Theme`) are heaviest in
`ebook` (62), `word-processor` (44), `tube` (32), `pdf_viewer` (18), `feeds` (16),
`terminal` (14), `sheets` (13), `camera` (12), `chat` (11). There is nothing to
*extract* here — the fix is per-app (route through `Theme`) — but it's the same
consistency failure the formatting item (A) exposes, and worth a dedicated pass.

## 2026-07-29 — finding A landed: `ui::format`

The shared formatter module now exists at `ui/src/format.rs` (`ui::format`) with
unit tests covering edge cases (0, the 1000/1024 boundaries, singular/plural,
rounding). API:

- `bytes(u64)` — base-1024 with **correct IEC binary units** (`KiB/MiB/GiB/TiB/…`).
- `bytes_si(u64)` — base-1000 SI (`kB/MB/GB/…`) for genuinely-decimal callers.
- `rate(f64)` — bytes/sec → base-1024 IEC + `/s`.
- `timecode(u64)` / `duration(u64)` — `m:ss` / `h:mm:ss`.
- `relative(i64)` — coarse "time ago" (`just now`, `3 min ago`, `yesterday`, …).
- `compact_count(u64)` — `1.2K` / `3.4M` / `1.1B`.
- `month_abbrev`/`month_name`/`weekday_abbrev`/`weekday_name` — 0-based index,
  English tables, index-based so a `fluent` caller can translate the same index.

**The KB/KiB correctness bug is fixed.** `mail`, `backups`, `containers`, and
`vpn` now all label base-1024 sizes with IEC binary units via `ui::format::bytes`
(previously `mail`/`backups`/`vpn` printed `KB/MB/GB` for the identical `/1024`
math). A `ui::format` test asserts the unit label matches its base
(`byte_unit_label_matches_its_base`).

**Converted (local copies deleted):**

- [x] `mail` — `human_bytes` → `ui::format::bytes`
- [x] `backups` — `human_bytes` → `ui::format::bytes`
- [x] `containers` — `human_bytes` → `ui::format::bytes`
- [x] `vpn` — `human_bytes` + `human_rate` → `ui::format::bytes` + `ui::format::rate`
- [x] `music` — `fmt_duration` → `timecode`; **three** count copies
  (`search_type.rs:compact_count`, `backend/live.rs:human_count`,
  `app/render.rs:human_count`) → `ui::format::compact_count`

**Remaining adopters — checklist for follow-up** (each true equivalent is a
straight swap to the matching `ui::format` fn + deletion of the local copy):

- [x] byte-size, base-1024 (5): `api_tester model.rs:human_size`, `maps
  app.rs:fmt_bytes`, `chat render_chat.rs:human_size`, `hex_editor
  view.rs:human_size`, `monitor model.rs:fmt_bytes` → `ui::format::bytes`. The
  four copies that mislabeled binary values as `KB/MB/GB` now use canonical IEC
  units; Monitor already used the correct labels.
- [ ] byte-size, base-1000 (4): `imager model.rs:fmt_size`, `camera
  app.rs:human_size`, `disks model.rs:fmt_size`, `file-explorer
  model.rs:human_size`. These were incorrectly classified as base-1024 in the
  original survey. Imager and Disks deliberately show vendor-style decimal
  storage sizes, File Explorer explicitly selects `humansize::DECIMAL`, and
  Camera also divides by 1000. Their precision and zero-value conventions do
  not exactly match `ui::format::bytes_si`, so they remain until that API can
  preserve the caller-visible formatting rather than changing it incidentally.
- timecode (5): `tube util.rs:clock`, `video_editor timecode.rs:clock`+`timecode`,
  `camera app.rs:fmt_dur`, `slides app.rs:present_clock`, `maps geo.rs:fmt_duration`.
- compact-count (3): `tube util.rs:human_count`, inline in `monitor`,
  inline in `store`.
- relative-time (8): `camera app.rs:ago`, `feeds model.rs:relative_time`,
  `git_client repo.rs:relative_time`, `chat model.rs:day_label`,
  `calendar date.rs:relative_label`, `tube ytdlp.rs`, plus `vpn model.rs:human_ago`
  and `mail util.rs:format_relative` — the last two were **intentionally left
  app-local** here: `vpn` wants the compact `34s ago` style and `mail` a
  calendar-style label (`09:42` / `Yesterday` / `Jun 24`), both distinct from
  `ui::format::relative`'s `3 min ago` wording. Adopting them needs either a
  matching `ui::format` variant or an agreed wording change, not a straight swap.
- month/weekday tables (5): `calendar`, `logs`, `mail util.rs` (its `MONTHS`
  table + `weekday`, still used by `format_relative`/`format_absolute`),
  `sheets` ×2 (`month_abbrev`+`month_abbrev_local`), `tube`.
