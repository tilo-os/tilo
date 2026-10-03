# Tilo Human Interface Guidelines

## Purpose & philosophy

The toolkit should make balanced, adaptive layouts easy to author. Shared
patterns encode useful defaults for spacing, hierarchy, readable width, and
interaction. When the same correction recurs across apps, improve the shared
pattern so applications inherit the better default.

Automatic, guided, and fully manual composition are all supported. An application
may position standard controls with authored dimensions and responsive rules,
including on ordinary forms and app chrome, without exception approval. Shared
control behavior, accessible names, reachable actions, and actual clipping still
matter. A custom gap or rectangle alone does not establish poor quality.

Use [Form composition](FORM_COMPOSITION.md) for the automatic/guided path and
[Manual layout](MANUAL_LAYOUT.md) for shared child measurement, exact placement,
mixed regions, and existing inspection tools.

---

## 1. Navigation & layout: sidebars vs. content panels

There are **two distinct kinds of panel**. Do not conflate them. A single
generic "overlay drawer with a scrim, everywhere" is wrong — it turns *content*
into a menu and clutters small screens.

### 1a. Genuine sidebar / menu  (pure navigation)

Chrome that lists destinations. Examples: a calendar's list of *calendars*; an
email client's accounts + folders.

- **Desktop / wide:** solid, **docked** beside the content. Collapsible via a
  proper **collapse / hamburger button** — **never an `X`**. It reads as part of
  the window frame, not a floating overlay. Smooth collapse/expand animation.
- **Mobile / narrow:** it **stays a genuine sidebar** — a web-style **overlay
  drawer** over the content (scrim behind), summoned and dismissed by the *same*
  proper collapse/hamburger button (**not** an `X`). It remains a menu; it does
  **not** turn into a full-screen page.

### 1b. Content panels  (overview + detail)

Panels that are themselves content the user reads and acts on. Examples: a
calendar's month/week grid (overview) + the event (detail); an email message
list (overview) + the message (detail).

- **Desktop / wide:** shown **side by side** (alongside the genuine sidebar, if
  any). Only the sidebar collapses; the content panels stay abreast.
- **Mobile / narrow:** show **only one of {overview, detail} at a time**. The
  overview is the default **full-screen** view; selecting an entry **slides the
  detail in from the right**, over the overview, with a **back button top-left**.
  Back returns to the overview. This is **push navigation** — never a
  scrim-overlay of content over content.

### 1c. How to decide which a panel is

- Is it *pure navigation* — a list of destinations / accounts / calendars? →
  **genuine sidebar** (§1a).
- Is it *content the user reads or acts on* — a list of messages, events, or
  cities-with-info? → **content panel** (§1b), **even if it sits on the left and
  looks sidebar-ish**.

### 1d. Concrete app mappings

- **Weather** — two **content panels**: a *contentful* city list (each row shows
  quick per-city info) as the **overview**, and the forecast as the **detail**.
  There is **no** genuine sidebar here — the city list is content, so it follows
  §1b. Mobile: full-screen city list → tap a city → full-screen forecast + back.
  Desktop: the two side by side. (Units belong in **Settings**, not in the app.)
- **Calendar / Email** — a **genuine sidebar** (calendars / accounts+folders,
  §1a) **plus two content panels** (overview grid/list + detail event/message,
  §1b).
  - Desktop: `[ sidebar | overview | detail ]` three abreast; **only the sidebar
    collapses**.
  - Mobile: the sidebar is an overlay drawer summoned by the collapse button; the
    overview is full-screen and the detail slides over from the right with a back
    button.

### 1e. Immersive / full-bleed apps  (no sidebar at all)

Some apps are **not** content-and-navigation apps — their primary surface *is*
the window. A **camera** viewfinder, a **video/media player**, **maps**, a
**game**: the live surface must fill the window edge-to-edge, uninterrupted.
These apps are a distinct class and **do not get the content-app sidebar** (§1a)
or the desktop overview/detail split (§1b).

- **Full-bleed surface.** The viewfinder / video / map fills the window to every
  edge. Do **not** dock a rail, sidebar, or app-title header beside it, and do
  **not** draw the app's own logo/brand mark — the OS owns window chrome
  (see "OS owns window chrome").
- **Controls float over the surface.** Capture/playback controls are translucent
  glass overlays anchored to the edges (top strip for capture settings,
  bottom-centre for the primary action + mode switch, corners for
  secondary actions), kept legible with a soft scrim rather than an opaque panel.
  When idle they may fade; they never box the surface in.
- **Secondary screens are pushes or sheets, not rail destinations.** A library /
  gallery is **pushed** over the surface with a **back button top-left**
  (§1b push navigation); settings and pickers are **overlay sheets** with a
  scrim and a close affordance. They are summoned *from* the surface (a
  thumbnail, a gear) and dismissed back *to* it — they are never persistent
  navigation chrome.
- **A mode switch is not a sidebar.** If the app has peer modes (photo / video /
  scan; play / browse), express them as a compact bottom-centre segmented
  switcher over the surface — not as a vertical list of destinations.

> How to tell the classes apart: if the window's job is to *show a live/edge-to-
> edge surface the user acts through*, it is immersive (§1e). If the window's job
> is to *navigate between and read destinations/content*, it is a content app
> (§1a/§1b). A camera with a Camera/Gallery/Settings rail down the left is the
> canonical mistake — it turns an immersive app into a content app and crowds the
> viewfinder.

---

## 2. Expandable rich-field cards

A per-item card hosting a rich field (e.g. a graph formula in the calculator)
**grows in height to fit its content, up to a maximum height**. Beyond that
maximum it becomes **vertically scrollable within the card**.

- Never **clip** content behind the card's edges to "hide" overflow — that is
  not a solution.
- Overflow detection must be **reliable**: any content taller than the card's max
  (e.g. a fraction, a 3D surface's multi-line default) must be recognised as
  overflowing and get scrolling.

---

## 3. Text editing (everywhere)

Editable text is a **CRDT-backed editor**, via the shared `ui` caret /
selection core:

- Undo / redo (Ctrl+Z / Ctrl+Y).
- Selection: shift+arrows, drag-select, word-wise (ctrl+arrows), select-all.
- Clipboard: Ctrl+C / X / V.
- Collaborative multi-cursor.

If an app hand-rolls text editing, it will get these wrong — use the shared core.

---

## 4. Chrome & copy

- No marketing slop in placeholders or badges ("Search privacy-respecting
  videos" → "Search"; no "no ads" pill).
- No internal / implementation labels in user-facing chrome (e.g. "CRDT-synced",
  "Stored in the document CRDT", a leaked `/var/workspace` path).
- Settings toggles live in **Settings**, not duplicated as always-on top-bar
  chips.
- Never present fabricated content as live data (no synthesized "video feed"
  gradients, no invented data on an empty state).

---

## 5. System integration (file access, host services)

- **File open/save goes through the XDG Desktop Portal**
  (`org.freedesktop.portal.FileChooser`, via `ashpd`) — an app **never draws its
  own file picker**. The *host desktop* provides the picker: on GNOME the user
  sees GNOME's file chooser; on Tilo the **compositor implements the portal
  backend** and renders Tilo's picker (which is also where the per-cluster
  sandbox / file-access prompt lives — the portal is the security boundary). A
  bespoke in-app "OpenDialog", or an app-drawn `ui::FilePicker` used for OS file
  open/save, is a bug. (`ui::FilePicker` exists only for the compositor's own
  portal-backend rendering, or non-filesystem in-cluster pickers.)
- Prefer host/portal services generally (screenshot, sharing, notifications)
  over reinventing them in-app, so a Tilo app behaves like a native app on
  whatever desktop it runs.

## 6. Performance (felt responsiveness)

- **No idle redraw spin.** An app at rest requests **no** frames; animate only
  while something is animating. (A caret blink or a 1 Hz clock is not a
  license to re-render the whole scene every vsync.)
- **No unbounded per-frame work.** CRDT writes are **diffed** — never rewrite the
  whole document (or delete+reinsert a whole text) per keystroke; that is
  O(n²) tombstone churn and it freezes the app. Failed network work
  (e.g. a map tile) is **negative-cached with backoff**, never retried every
  frame.
- The compute path (event → laid-out frame) should stay well under one frame
  (~16 ms); if an app feels slow, measure event/layout/draw/present separately
  before blaming app code.

---

## 7. Visual craft & polish (the bar for *every* app)

Every screen must meet the spacing, hierarchy, responsive layout, state
visibility, motion, interaction, and theming requirements below. Functional
completeness does not exempt a screen from these requirements.

- **Layout breathes.** Deliberate spacing on the `Theme` scale; content is not
  jammed edge-to-edge, controls are not scattered. A clear reading order and a
  single primary action per screen. Groups are separated by space and hierarchy,
  not by hairlines and boxes-within-boxes.
- **Nothing touches.** Every neighbouring pair keeps a token-step gap (`s1`+). A
  fixed offset that produces a touch, an overlap or a clipped glyph at 1× is a
  defect regardless of how it was authored — the screenshot decides, not the
  byte-identical frame. Content is centred in its container: icon + label
  blocks in tiles, leading icons and trailing controls on the row's centre
  line, text on its measured line box.
- **An edge you draw is derived.** Where an app paints its own card, row or
  pill edge, it comes from `ui::surface::{card, edge_on, separator}` or
  `ui::chrome::panel_border`, measured against the surface it sits on;
  `border_standard` is a rule on `surfaces.base`, never a stroke on `raised`
  or `overlay` (on the dark preset the two are three shades apart and the
  edge vanishes).
- **Real typographic hierarchy.** Size / weight / color establish
  title → section → body → caption; digits that line up use tabular figures. No
  wall of same-size text; no illegible low-contrast label on a colored fill
  (the permissions-pane and camera regressions were exactly this).
- **Content reflows by size, not just shrinks.** Honor the responsive rule (§1):
  card grids re-column, participant/media grids re-tile by count, panels become
  separate screens when they no longer fit — measured against window *area*, and
  **recomputed on resize**, never laid out once.
- **State reads at a glance.** Selection, focus, active/live, speaking, syncing,
  error — each has a *visible form* (an emphasis, a ring, a pill, a severity
  stripe), not only a text label. Empty states are composed and inviting (a
  zero-state, per §4 — never fabricated content), loading and error states are
  designed, not raw.
- **Motion is purposeful and quiet.** Transitions clarify a change (a panel
  sliding in, a value settling); they never gate interaction or spin at idle
  (§6). Reduced-motion is honored.
- **Interactive things look interactive** and give feedback: hover, press, focus,
  and disabled are all distinct; a control that does nothing yet is a defect
  (§ functionality), and a control that *looks* dead is a craft defect.
- **Theme-driven, pixel-clean.** Every color and metric comes from the `Theme`
  (§ toolkit) so light/dark and OS accent both look intentional; no hard-coded
  colors, no 1px seams from mismatched fills, no clipped glyphs or overlapping
  text.

Validate these requirements with `--shots` for the primary screens (populated,
empty, and a mid-interaction state), then inspect the output. When a pattern
recurs (a media/participant grid, a stat tile, a huddle overlay, a segmented
control), promote it to a `ui` primitive so the shared version is the default.

### 7a. Let content determine ordinary layout

Choose a shared composition by what the region contains:

| Region | Starting point | Decisions supplied by the framework |
|---|---|---|
| Account or settings form | `FormPage`, `Form`, `Section`, `Field` | One page gutter, readable width, related-field spacing, measured label placement, wrapping and scrolling |
| Card with a title, content and actions | `Card::composed` with a header and footer | Content height, consistent card gutter, footer containment and alignment |
| Small primary/secondary action group | `Actions` | Measured unspecified button sizes, wrapping and trailing alignment |
| Long command strip | `Toolbar` | Measured command sizes, bounded overflow menu, keyboard navigation and accessible command bounds |
| Main writing surface | `Scrollable::with_content_min_viewport_height()` | Short auto-height content fills the viewport; long content retains its intrinsic scroll extent |
| Empty list or first-use pane | `EmptyState` | Wrapped title/message, measured action height, centering and decorative shedding |
| Authored canvas or custom arrangement | `Manual` or existing layout primitives | Shared child measurement and ordinary control behavior; the author chooses geometry |

Give primary content the remaining workspace. Empty optional docks should not
reserve a large area before they have useful content. A short query or request
can start compact and retain a visible resize route. Bound prose and forms to a
readable width; keep document, image, map and editing canvases useful at full
workspace width. These are different content needs, so a single global maximum
width or larger global gap cannot produce good balance.

Keep related title, explanation and action rows close enough to read as a group,
with a larger separation before the next group. Avoid repeating a page title
inside its first section or wrapping each grouping in another padded surface.
When commands or panes no longer fit, use wrapping, a labelled disclosure, or
the existing overview/detail navigation. Preserve selected state and the return
route through the transition.

For custom text painting, measure and draw with the same style and width.
Theme typography tokens are already resolved logical sizes; pass those styles
through `TextRenderer::renderer_input_style` once before calling the legacy
renderer-relative APIs. Base font sizes supplied to editor controls remain base
sizes. Do not compensate for a scale mismatch with smaller fonts or offsets.

An empty state's title and action can exceed a physically small pane even after
decoration is removed. Retained callers can use `EmptyState::into_scrollable()`;
immediate callers use `minimum_essential_height` and provide their own scroll
owner. Check the action after scrolling, including pointer and keyboard use.

For a primary editor, give the Scrollable a viewport and opt into a minimum
content height:

```rust
Scrollable::new_vertical()
    .fill_parent_height()
    .with_content_min_viewport_height()
    .with_child(Box::new(editor))
```

`fill_parent_height()` sizes the viewport. The additional opt-in lets short
auto-height content fill that viewport while long content scrolls. Ordinary
multiline form fields remain content-sized by default, and a child's explicit
fixed height stays authoritative. Scratchpad uses this composition with its
existing editor and Save action.

For inline rich text, use `RichTextLine` marks so measurement, glyphs, caret,
selection and links share one shaped layout. Do not paint styled words over a
plain text field: their different advances create doubled glyphs and misplaced
interaction targets. Code chips and rules are decorations around those shaped
ranges, not separately positioned text.

For a long command strip, let `Toolbar` choose the visible commands from
renderer measurements and put the remainder in its owned overflow menu. The
same original callbacks remain available through pointer, keyboard and semantic
activation. Use the ordinary Widget measurement and overlay lifecycle;
`recompute_visible` is a compatibility estimate for callers without a renderer,
not the final measured layout. A small form footer should still use `Actions`
when wrapping its few buttons gives a clearer result.

Explicit dimensions remain authored choices. Use guided overrides or fully
[manual composition](MANUAL_LAYOUT.md) wherever the default does not express the
intended layout. Evaluate the resulting reading order, useful content area,
containment and action reachability rather than counting custom pixel values.

### 7b. Spacing tokens, insets and density

Every gap and inset is a step on `theme.tokens.spacing` (`s1`–`s16` =
4 / 8 / 12 / 16 / 24 / 32 / 48 / 64); every control height, hit target, panel
width and icon size is a field of `theme.tokens.metrics` (§9.6). Both scale
with `ui_scale`; a custom theme's `spacing_px` rescales the whole spacing scale
proportionally and leaves metrics alone. That is also why a hit target must
read `metrics.min_pointer_target`, never a spacing step: a control that sizes
its minimum from `spacing.s8` shrinks with a dense gutter preference.
`Spacing::BASE` and `Metrics::BASE` are the unscaled reference values for the
few chrome constants that must be compile-time.

Rhythm inside a group versus between groups:

| Relationship | Step | Typical use |
|---|---|---|
| Label ↔ its control, icon ↔ its label | `s1` / `metrics.icon_label_gap` | stacked field label, button icon |
| Related rows that read as one group | `s2` | title + explanation, help copy under a field |
| Siblings of equal weight | `s3` (`gap_standard`, the `Row`/`Column` default) | toolbar buttons, list rows |
| Section padding, gap between Panel sections | `s4` | panel body, card gutter |
| Before the next group; page gutter | `s6` (the `Column` default inset) | form sections, page edge |
| Between major regions | `s8`+ | header block ↔ content |

One rhythm per panel or screen: the same relationship always gets the same
step. A section gap of 30 px beside one of 18 px for the same relationship is
a defect even when both are "authored".

The framework supplies the values so callers state the relationship, not the
number:

- `Row::with_gap(px)` / `Column::with_gap(px)` change the gap between children;
  `Row::with_justify(..)` and `Row::with_align_items(..)` place them along and
  across the row. Defaults (gap `s3`, children centred, start-packed) are
  unchanged.
- `Spacer::token(SpacingStep::S4)` is a fixed gap that resolves from the Theme
  at layout time and therefore follows `ui_scale`; `Spacer::fixed(px)` is the
  authored-pixel form. Neither grows. `Spacer::new()` still grows to push
  siblings apart.
- `insets_all(px)`, `insets_xy(horizontal, vertical)` and
  `insets(top, right, bottom, left)` build a taffy padding `Rect` from token
  values in one expression instead of four `length(..)` fields.

`tests/spacing_token_audit.rs` ratchets module-level pixel constants,
literal `length(N)` values, literal container gaps and padding, and
`Spacer::fixed(N)` in widget, chrome, shell and app production code: a file
may never carry more than its shard under `tests/baselines/spacing/` records.
Every ratchet runs on one engine (`tests/support/ratchet.rs`), so all audits
see exactly the same production lines; §9.13 lists them.

A custom theme's `spacing_px` is built inside `CustomThemeSpec::SPACING_PX_MIN
..= SPACING_PX_MAX` (6–22, the Themes app's range) even though the schema
admits 2–64: the whole scale is proportional to it, and 2 px would put `s1`
under a pixel.

---

## 8. Never hand-roll an interactive primitive

Manual positioning of standard widgets is supported. The guidance below concerns
reimplementing their interaction and editing behavior, not choosing their geometry.


**Use the shared `ui` widget for every interactive primitive — text field,
button, dialog, dropdown, checkbox, toggle, slider, tabs, menu, tooltip,
popover. Never paint your own.** A "clickable rect + a manual hit-test" or a
"`String` field you `push`/`pop` on raw key events" is not a lightweight
shortcut — it is a broken control that silently drops the behaviours the shared
widget already gets right:

- **Text selection** — shift+arrows, drag-select, word-wise, select-all
  (a hand-rolled `String` field has no selection at all).
- **Clipboard** — Ctrl+C / X / V, and paste from other apps.
- **Keyboard** — a painted button cannot be focused or activated with
  Space/Enter; a hand-rolled field has no caret navigation or IME.
- **Focus** — the 2D directional focus ring (`App::focusable_order`) skips
  anything that is not a widget, so hand-rolled controls are unreachable by
  keyboard and invisible to focus-follow.
- **Accessibility** — the AT-SPI / a11y tree is built from the widget tree; a
  painted rect has no role, name, or state, so it does not exist to a screen
  reader.
- **Theming** — shared widgets read every color and metric from the `Theme`
  (hover/press/focus/disabled included), so light/dark, high-contrast, and OS
  accent all track automatically. A hand-rolled control with `Color::rgb(...)`
  literals breaks all three (see §7's "Theme-driven" clause).

This generalises §3 (text editing) to **all** interactive primitives. Concretely:

- Editable field → `TextInput` (or `SearchField` / `NumberInput` /
  `EditableLabel`). Never keep a per-app cursor index and splice characters into
  a `String` on `Key::Character` / `Backspace`.
- Clickable → `Button`. Never `draw_*_button` + a `contains()` hit-test.
- Confirm / prompt / sheet → `ModalDialog` (scrim, focus capture, Esc, a11y are
  built in). Never paint your own overlay + scrim rect.
- Select / choice → `Dropdown`; on/off → `Toggle` / `Checkbox`; range →
  `Slider` / `NumberInput`; peer views → `TabBar` / `ToggleGroup`; right-click →
  `ContextMenu`; hover help → `tooltip`; anchored transient → `Popover`.

**If the shared widget is missing a capability you need, extend the widget — do
not fork it.** Add the feature to the one `ui` primitive so every app gets it and
the accessible / themed / clipboard-correct version stays the default. Forking a
private copy re-earns every bug above and guarantees drift. The only sanctioned
"hand-rolled" text surfaces are genuine document engines whose buffer *is* the
app (the terminal grid, a code/rich-text/cell editor) — and even those must route
their **side** fields (find bar, rename box, cell entry, command line) through the
shared widgets, and are themselves candidates for a shared editor primitive
(§ toolkit gaps) rather than N private forks.

> Enforcement: a new painted control is a toolkit gap, not an app feature. Grep
> your diff for the patterns in §9.13 plus `draw_*_button` and
> `Key::Character => *.push` — each hit is either a shared-widget call you
> skipped or a widget you should be extending.

**Grep for the spelling the code actually uses.** This rule used to say "grep
for `Color::rgb`", and that pattern is why hard-coded colour accumulated under
it for months: `Color::rgb8(…)` is the *old* peniko constructor and this tree
does not call it. The one automated check that implemented the rule
(`crates/sdk/ui-core/tests/design_token_audit.rs`) was green over 125 literal
`Color::from_rgb8(…)` colours in `crates/sdk/ui-core/src` alone, because it was looking for a
string that appears nowhere. Two more spellings were invisible for the same
reason:

- **`Color::WHITE` / `Color::BLACK`** — most are a defensible ink on an accent
  fill or a scrim; the defect is that nothing was looking, so nobody ever had to
  say which.
- **`surface_white_*` / `border_white_*`** — the values are no longer literally
  white: `Colors::themed` washes them toward the contrasting extreme, so they
  flip to black on a light palette (the fix landed after every unselected filter
  chip lost its pill at 1.01:1 on light). What survives is the reason §9.5
  deprecates them anyway — the *name* still says white, which is how a reader
  picks one while reasoning about the wrong colour, and the tier is a
  **declared** alpha that never measures itself against the ground it lands on.
  `Colors::lift()` and `Colors::hairline_on()` do measure.

`design_token_audit.rs` now covers all three in the toolkit, the shell and
every app, ratcheted against the shards under
`crates/sdk/ui-core/tests/baselines/theme/`: a file may never gain one. The
count only goes down. Those shards, not this document, are the authority on
how many are left (§9.13).

---

## 9. The shell chrome design language

Sections 1–8 govern **apps**. This section governs the **shell** — the surfaces
the OS itself paints around and between apps: the top bar, window decorations
and tab pills, the workspace disc, the strip indicator, quick settings, the
command palette, and the collaborator invite/roster surfaces.

It exists because those surfaces drifted into separate design systems. Each was
transliterated independently from its own mockup component, carrying that
mockup's literal Tailwind values inward — `bg-black/80` for the bar,
`bg-slate-900/80` for quick settings, `rounded-3xl` here and `rounded` there,
`text-green-400` for a CPU label. The toolkit's token vocabulary was already
good, and the chrome simply did not read from it. The result was four surfaces
that share a screen without sharing a language.

The rule that prevents the drift returning is the same one §8 applies to apps:

> **Shell chrome does not invent appearance. It selects a role from this
> section, and the role resolves to tokens.** A literal colour, a literal
> radius, or a bespoke duration in a chrome draw path is a defect, the same way
> a hand-rolled text field is.

### 9.1 Roles, not pixels

Every chrome surface is exactly one of six **roles**. Pick the role first; the
role fixes radius, surface, border, elevation and motion together, so two
surfaces of the same role are automatically consistent.

| Role | What it is | Radius | Surface | Border | Elevation |
|---|---|---|---|---|---|
| **Ground** | the desktop behind everything | — | `surfaces.base` | — | none |
| **Bar** | docked chrome fixed to a screen edge (top bar) | — (edge to edge) | `chrome::shell::panel_ground` (opaque) | bottom hairline only | `e1`, downward only |
| **Panel** | a floating surface that opens over content (quick settings, palette, popovers, sheets) | `radii.xl` (24) | `chrome::shell::panel_ground` (opaque) | full hairline | `e4` |
| **Control** | something inside a Bar or Panel that you press (action tile, list row, slider, field) | `radii.lg` (12) | selection tiers (§9.3) | per tier | none |
| **Selector** | one of a set you choose between (tab pill, tray pill, workspace chip, strip segment, scope chip) | `radii.full` | selection tiers (§9.3) | per tier | none |
| **Object** | a thing that holds app content (window card, tile) | `radii.xl` (24) | `surfaces.window_card` | `Colors::window_edge()` | `e3`; `e4` while dragged |

Two consequences worth stating outright, because both were being violated:

- **`radii.sm` (4) and `radii.md` (8) are not for interactive bodies.** They are
  for details drawn *inside* a control — a progress track, a badge, a sparkline
  clip. A pressable thing is `radii.lg` if it is a Control and `radii.full` if
  it is a Selector, never a 4 px rectangle.
- **16 px is not on the scale.** The radius scale is 4 / 8 / 12 / 24 / full and
  deliberately omits 16 (see `tokens::radii`). A literal `16.0` in a chrome
  draw path is off-scale by construction.

### 9.2 Surfaces come from the semantic tier, never from a ramp slot

Chrome reads `colors.surfaces.*`, which every theme defines for itself. It does
**not** reach into `colors.neutral[n]`. A ramp slot is a colour; a surface token
is a *promise about contrast* that each theme keeps in its own way, and only the
latter survives a theme the author never saw.

- Bar and Panel: `surfaces.overlay`.
- Ground: `surfaces.base`. Object: `surfaces.window_card`.
- Scrim behind a modal Panel: `surfaces.scrim`, faded in with the panel.

One shared constant, `CHROME_SURFACE_ALPHA`, sets the opacity of every Bar and
Panel, so the whole chrome is one material. Per-surface alpha literals are what
made quick settings read as a different substance from the bar it hangs off.

**That material is opaque.** Translucency is
only worth its cost when the backdrop is blurred, and this renderer cannot blur
a backdrop — vello can blur a *shape*, which is what a drop shadow is made of,
but there is no backdrop filter short of rendering the scene to a texture and
convolving it. Unblurred, a translucent panel simply adds a fraction of whatever
is behind it to itself: not glass, but a loss of contrast that changes with what
the user happens to have open. Measured at the old 0.80, one quick-settings
panel's own body ran `#ffffff` over a document and `#f7f7f7` over the desktop —
one flat surface reading as two, with a seam across the middle of it.

**A Panel separates itself by value and by edge.** `panel_ground` walks the
declared overlay tier away from *both* the desktop and the window card until it
clears each of them, because a Panel floats over app content and a Bar does not,
and the two are one material. `panel_border` supplies the edge. Get those right
and elevation is a finishing touch; get them wrong and the shadow is asked to do
their job, which is how it ends up too big to look like a shadow.

### 9.3 Selection is a lift, and only a lift

Every "which one is chosen" state in the shell — active tab, active workspace,
active strip, selected row, chosen scope chip, current power profile — uses the
**same four tiers**:

| State | Fill | Border | Inner glow | Scale |
|---|---|---|---|---|
| idle | transparent | none | none | 1.0 |
| hover | `colors.lift(0.5)` | none | none | 1.0 |
| **active** | `colors.lift(1.0)` | `hairline_on(ground, 26)` | `hairline_on(ground, 51)` | 1.0 |
| pressed | as active | as active | as active | 0.97 |

This is `ui::chrome::selection_tier` (promoted out of `widget::tabs`, where it
was `pub(crate)` and therefore unreachable from the shell — which is precisely
why the shell grew its own incompatible answers). Call it; do not restate it.
This includes toolkit list selection: `TreeView`, `Table`, `DataTable` and
`Sidebar` rows are a lift, never an accent wash.

**One caveat the tier cannot absorb for you.** `Colors::lift` interpolates from
`surfaces.base` toward `surfaces.raised`, and where those two are close it
falls back to a derived step against the ground. That fallback is by design and
is what the default dark palette already uses. But it is *weak*, and on the
high-contrast palette — where `base` and `raised` are both pure black — it is
weaker than a fixed floor. A control that gives its unselected state a floor of
its own (because it has no row around it to supply context, as the strip ribbon
does) can therefore end up painting the **selected** state less prominently
than the unselected one. If a control needs its own floor, derive all of its
states from one ordered ramp against the ground and assert the ordering per
theme; do not mix a floor with the tier.

Why lift rather than an accent fill:

- `Colors::lift` measures its move against the chrome's *own ground* by relative
  luminance, so it lifts correctly on dark, light and high-contrast without a
  per-theme branch. A fixed white wash only lifts over a dark ground, and a
  fixed accent fill collides with whatever the accent is being used for.
- It leaves the **accent free to mean one thing**: see §9.4.
- Hover and active are the same *kind* of change at two strengths, so the
  control reads as one continuous affordance rather than two unrelated looks.

An active thing never gets *more* prominent on hover. The contrast gain belongs
to the inactive things brightening toward the active tier.

**Elevation and selection are different vocabularies and must not be mixed:**

> **A shadow means "off the surface". A lift means "chosen on the surface".**

A selected pill casts no shadow — it is not floating, it is picked. A dragged
object casts `e4` — it is lifted and will land somewhere. Rows of
idle pills each laying down a soft shadow is what made the tab strip read as
muddy rather than as chrome.

**A shadow is the shape of the thing casting it, and comes from a token.** One
painter, `surface::shadow(scene, transform, rect, radius, token)`: it blurs the
surface's *own rounded outline*, so a shadow is equally dense the whole way
round and its corners follow the surface's corners. Pass the radius. A surface
that hands its shadow a square field gets a box around a rounded panel, with
corners several times lighter than the flat edges — "boxy and uneven" is what
that looks like from the outside.

The token is the whole description: `offset_y`, `blur`, and a `color` whose
**alpha is the strength and is honoured**. Do not hand a shadow a bare `Color` —
a caller given a colour and no offset or blur has to invent the other two, which
is how four surfaces in this toolkit each grew a different shadow, three of them
a solid rounded rect nudged down a few pixels and called soft. `offset_y` stays
a quarter of `blur` across the ramp; a larger ratio slides the falloff onto the
bottom edge and the surface stops looking lifted and starts looking like it has
a dark bar under it.

`soft_shadow` is the older sprite painter and is **not** for drop shadows any
more. It survives for effects with no outline to follow — the wheel disc's
bloom, a glow around a dragged card.

### 9.4 Accent means live, not selected

`colors.accent` is reserved for **state the user did not choose and should
notice**: recording, screen-sharing, a peer joining, a live collaborative
session, an unread count, a sync in flight. Selection is a lift (§9.3); focus is
`border_focus`; danger/warning/success are `colors.semantic.*`.

This is the rule that kills per-surface palettes. A CPU label is not "green" —
if a metric strip needs to distinguish its columns, it distinguishes them with
`semantic.success` / `semantic.info` / `semantic.warning`, which every theme
defines and high-contrast redefines. Literal `0x4a,0xde,0x80` is Tailwind
green-400 wearing a costume, and it ignores the user's theme completely.

Per-peer identity colours are the single sanctioned exception: they come from
`ui::presence::user_color`, are derived from the peer id, and are *identity*,
not decoration.

### 9.5 Borders are derived, never declared

Chrome borders call `Colors::hairline_on(ground, alpha)`, which picks white or
black by whichever actually moves against that ground. A declared
`border_standard` stroked onto a translucent Panel does not track the surface
under it and disappears on one theme or glares on another.

- hairline on a control: alpha 26 (`/10`)
- inner glow on an active control: alpha 51 (`/20`)
- separator inside a Panel: alpha 26, inset from the panel edge by
  `spacing.s3`, never running full-bleed into the corner radius

`surface_white_5` / `surface_white_10` / `border_white_10` were literal white at
low alpha in **every** theme, including light ones where a white veneer over a
white panel is invisible — the faint tier measured 1.01:1 on light, which is why
every unselected filter chip there read as bare text. `Colors::themed` now
washes them toward the contrasting extreme instead, so they flip to black on a
light palette and a tier means the same thing on both sides of the line.

They stay **deprecated in chrome** regardless, for the half that derivation
cannot fix: the name says white while the value is a veneer, and the strength is
*declared* rather than measured against the ground the surface actually lands
on. `lift` and `hairline_on` take that measurement, so a hairline stays a
hairline on a palette their author never saw. Reach for those.

#### Rest edges (toolkit and shell)

At rest (idle, enabled) every control, card and popup has a **visible derived
edge or a clear contrast step** against the surface it sits on, at 1× in dark
and light alike. "Edge" is `ui::surface::edge_on(fill)` painted as a crisp
inset 1 px stroke (`ui::surface::card`, `pill`, or the same geometry inline);
chosen states use `selection_tier` (Controls) or `glass_selector` (Selectors);
focus stays `border_focus`; disabled is the rest face under
`widget::DISABLED_ALPHA`, so rest and disabled never look the same.

| Control | Rest fill | Rest edge |
|---|---|---|
| Button Neutral / Disabled, Dropdown field | `surfaces.raised` | `edge_on(raised)` |
| Card / Container / ContentCard | as today | `edge_on(fill)`; a section inside a card gets space, not a second edge |
| TextInput / NumberInput / SearchField | field fill | `edge_on(fill)`; focus `border_focus` |
| Dropdown list, ContextMenu, Popover, SlashMenu, Autocomplete, LinkPopover, CommandPalette, FilePicker, PermissionPrompt, Toast, ModalDialog, ModalFrame | `panel_ground` (or `surfaces.overlay` where already used) | `panel_border`; separators `separator(.., s3, ..)`, never full-bleed |
| Tooltip | as today | `edge_on(fill)` |
| Segmented / ToggleGroup track | `surfaces.raised` | `edge_on(track)`; active segment `glass_selector` |
| Chip quiet | `surfaces.raised` | `edge_on(raised)`; selected accent, no edge |
| Toggle | off `lift(1.0)`, on `accent_primary` | `edge_on(track)`; one knob colour for both states |
| Slider | unfilled `lift(1.0)`, filled `accent_primary` | `edge_on(unfilled)`; thumb contrasts against both halves |
| Checkbox | unchecked `surfaces.raised` | `edge_on(raised)` |
| Table / DataTable rules | — | `edge_on(table_fill)`, never into a rounded corner |
| TreeView / Sidebar / VirtualList rows | transparent | none; selection is `selection_tier` |
| Divider | — | `border_standard` only on `surfaces.base`; `Divider::on(surface)` elsewhere |
| App chrome Toolbar / StatusBar | as today | `chrome_hairline` |
| Selectors (tab pills, tray pill, scope chip) | idle none | none (`glass_selector`) |

A declared border or ramp token (`border_standard`, `border_strong`,
`overlay_border_strong`, `colors.neutral[..]`) on a chrome surface is a defect;
`tests/design_token_audit.rs::declared_borders_in_chrome_do_not_grow` ratchets
their count against the shards under `tests/baselines/chrome-border/`.

**One painter per row of that table.** `ui::surface::card(fill)` is the edged
opaque surface; `ui::surface::panel(theme)` is the Panel row (`panel_ground`
fill + `panel_border`) so no app spells the pair; `ui::surface::card_on(fill,
ground)` is for a translucent wash (an accent tint, a hover film) — the edge
derives from what the eye sees, the wash composited over its ground, because a
hairline judged by a 16 % wash's own RGB flips polarity on light and vanishes.
`edge_on` assumes an opaque fill. All three snap their rect to whole logical
pixels and `separator` lands on one device-pixel row (`separator_rect`): a 1 px
edge on a fractional coordinate is two half-covered rows that fall under
`HAIRLINE_MIN_DELTA` even when the colour registers.

**Selection lifts from the ground it sits on.** `chrome::selection_tier` measures
its lift against `surfaces.base`; a row on `panel_ground`, a raised sidebar or a
card is within rounding distance of that lift on dark and high-contrast (Δ
0.008 / 0.002). Use `chrome::selection_tier_on(theme, state, ground)` there — it
keeps the base lift where it registers and otherwise walks `LIFT_ALPHA_STEPS`
until the active fill clears the ground by `HAIRLINE_MIN_DELTA`, with hover
halfway between. The same rule for one control never yields a different fill
depending on which panel hosts it, because the ground is the input.

### 9.6 Type and density

Chrome type is a **compressed** version of the app scale — chrome labels a thing,
it does not host prose.

| Use | Size | Weight | Colour |
|---|---|---|---|
| Panel title | `scale.base` (14) | `semibold` | `text_primary` |
| Section header inside a Panel | `scale.xs` (10) | `medium` | `text_tertiary` |
| Control label | `scale.sm` (12) | `medium` | `text_primary` |
| Control sub-line / value | `scale.sm` (12) | `regular` | `text_dim` |
| Bar label | `scale.sm` (12) | `medium` | `text_primary` |
| Numeric readout | `scale.sm` (12) | `semibold`, tabular | `text_primary` |

Density, so two panels built by two people come out the same size. Each value
is a field of `theme.tokens.metrics` (`ui::tokens::Metrics`), scaled by
`ui_scale` like spacing; read it from there rather than restating the number:

- Bar height 40 (`bar_height`; also the shared `Button`/`Dropdown` default);
  Bar control height 28 (`bar_control_height`).
- Panel padding `spacing.s4` (16); gap between Panel sections `spacing.s4`.
- Panel row / Control height 44 (`panel_row_height`); Selector height 28
  (`selector_height`).
- Minimum pointer target 32×32 (`min_pointer_target`) — a 20 px glyph
  (`icon_md`) gets a 32 px hit rect. Glyphs: `icon_sm` 14, `icon_md` 20,
  `icon_lg` 24; icon-to-label gap `icon_label_gap` 6.
- Panel width: 320 for a utility Panel (quick settings,
  `panel_width_utility`; an overlay drawer is the same width,
  `drawer_width`), 360 for a roster or list Panel (`panel_width_list`), 560
  for a sheet with a form in it (`panel_width_sheet`).
- Responsive shells: a genuine sidebar docks at `sidebar_width` 280, an
  overview column at `overview_width` 340, and a sidebar docks beside its
  content at or above `compact_breakpoint` 720. `NavShell` and `AppShell`
  read those defaults from `theme.tokens.metrics` in `build_layout`, so they
  follow `ui_scale`. `NavSplit` (300 / 900), `NavShell`'s 700 breakpoint and
  `AppShell`'s 264 sidebar predate the tokens; the proposal on record is one
  set for all four shells — sidebar `sidebar_width` 280, breakpoint
  `compact_breakpoint` 720 (drawer: 720 + `drawer_width − sidebar_width` =
  760), and one chip for `AppShell`'s Back button (34) and `NavShell`'s
  collapse / Back chip (36): `min_pointer_target` 32 inside an `s3` inset,
  which also makes the chip band (56) equal to a 56 px header bar.
- **One text-scale rule for shell inputs (`Theme::responsive_scale`).** A
  breakpoint or pane width handed to `NavSplit`, `NavShell`, `AppShell` or
  `SidebarDrawer` is **authored at 1×** and the shell resolves it against the
  theme when the tree is built: `× ui_scale × text_scale.max(1.0)`
  (`Metrics::text_growth`). Panes and breakpoints are sized by the text they
  hold, so they grow with it and never shrink below the authored geometry;
  an app passes `with_breakpoint(760.0)` and deletes its own
  `× text_scale` arithmetic (`widget::AuthoredExtent` is the rule as a
  value, for a host that needs it directly). A width sized by icons rather
  than text — a rail — takes the `with_icon_*_width` builder
  (`SidebarDrawer::icon_docked_width`) and follows `ui_scale` alone.
  `SidebarDrawer` has no `build_layout`, so a host adopting the rule calls
  `drawer.scale_for(theme)` once from its own; until it does, the drawer lays
  out the authored geometry, which is what every drawer host does today.
  Read the resolved values back (`shell.breakpoint()`, `sidebar_width()`)
  rather than comparing a window width against the authored constant. A
  shell control that carries one line of text (the Back chip, the search
  slot) grows by `widget::text_control_height(authored, theme)` — the
  authored height until the body line outgrows it, then line + the authored
  padding — never by the whole scale.
- **Shell panes are measured with the host's theme and renderer.** A
  `NavSplit` / `NavShell` pane is built against the theme its outer tree was
  built with and solved in `adjust_layout_with_measure` with the renderer
  the host hands the layout pass; a `Text` under a split measures at the
  same text scale as everything beside it. Never lay a child out against
  `Theme::dark()` or a fresh `TextRenderer::new()` on a draw or layout path.
- **A docked sidebar keeps its full height.** The collapse chip sits in the
  docked sidebar's own header row, which the sidebar clears horizontally
  (mail: `CHIP_CLEARANCE`), so the three panes' headers share one top edge.
  Only an overlay drawer, and a narrow content pane the chip floats over,
  start below the chip band.

### 9.7 Motion

All chrome animation reads `tokens.motion.durations` and nothing else:

- `fast` (130 ms) — hover, press, chip toggle, drop-zone hint, pip elongation.
- `base` (210 ms) — a Panel opening or closing, a tab in/out, a strip change,
  a reflow.
- `slow` (400 ms) — whole-view changes: overview, workspace switch, window
  spawn.

Panels open with the **one** entrance: opacity 0→1, y −10→0, scale 0.95→1,
`Curve::EaseOut` over `base`, and reverse to close. Bespoke per-surface
constants (the shell previously carried eight near-but-unequal ones) are a
defect. Reduced-motion collapses every duration to 0 and keeps the end state.

### 9.8 Chrome may not block the frame

Chrome is the OS's own UI, so a stall in it looks like the machine is broken
rather than like an app being slow. §6 applies with one extra, non-negotiable
rule:

> **No chrome draw path performs I/O.** No process spawn, no D-Bus round trip,
> no filesystem scan, no network call — not on open, not on hover, not per
> frame.

State that comes from the OS is read on a worker and delivered to the frame
asynchronously. The surface opens *immediately* with the last known values and
fills in when the read lands; a value that has never been read shows its own
"reading" state, never an empty panel and never a delayed one.

Reads are also **tiered by what the surface actually shows**. A panel fetches
what is on screen now, and defers what is only visible after a drill-in. Quick
settings was the worked example: opening it ran seven blocking shell-outs, one
of which (`nmcli device wifi list`) forces a Wi-Fi scan — 4.5 s of frozen
compositor to populate a list the user had not yet asked to see.

Every shell-out gets a timeout. An OS tool that hangs must degrade to a stale
value, never to a hung desktop.

### 9.9 The shell obeys §8

Everything in §8 applies to chrome, and chrome has been the worst offender: the
quick-settings sliders, its action tiles and its list rows are painted rects
with manual hit-tests, which is why they have no keyboard path, no focus ring
and no a11y node. New chrome uses `ui::widget::*`; a chrome control that the
toolkit does not yet have is a toolkit gap to fill, not a rect to paint.

### 9.10 Each level of the model owns one part of the screen

The model is `Workspace → Strip → WindowGroup → Tab`: Niri-like scrollable
strips inside collaborative workspaces. Three of those four levels are things
the user moves between, and each gets a fixed, non-overlapping part of the
screen so the control for a level is always in the same place:

- **Left edge of the content — which workspace.** The workspace disc. The
  outermost level, and the one changed least often.
- **Right edge of the content — which strip.** The strip ribbon (§9.11), within
  the workspace already in view.
- **Horizontal, inside the content — which group, which tab.** Groups scroll
  horizontally within a strip; tabs tile within a group. Nothing else may claim
  horizontal scrolling of the content area.

Keep these separate. Folding strip switching into the disc would mean one
control moving between two different levels of the model, and nothing on the
control would tell the user which of the two a given drag was about to do.
Workspace switching already owns Ctrl/Super + Up/Down, so a strip binding must
be distinct from it rather than a variant of it.

### 9.11 A switcher must answer "which one am I on" without being asked

Any control that moves between N things shows, at rest and without interaction:

1. **how many** there are,
2. **which one** is current,
3. **which direction** the others lie in.

A switcher that only appears once you already know the gesture is not a
switcher; it is a shortcut for people who read the source. Strips failed all
three — a workspace could hold four strips with nothing on screen indicating
that a second one existed.

The **strip ribbon** is the answer here: a slim vertical ribbon on the
content's right edge, one segment per strip, ordered to match the order the
strips are stacked in. The focused segment takes the active tier from §9.3 and
elongates; the others sit at the idle tier and thicken on hover. It is
`radii.full` (Selector role), it is click- and keyboard-addressable, and it
animates at `base`.

Note what it indicates. Every strip in a workspace is rendered at once, sharing
the content height between them, so the ribbon does not say "this is the strip
you can see" — it says **which strip has focus**, which is the thing that
decides where a new window lands and what the keyboard is aimed at, and which
had no representation on screen at all. It idles quiet enough to ignore and is
never absent, so a second strip is discoverable by looking rather than by being
told.

### 9.12 Collaboration chrome is chrome

Inviting a peer, showing who is here, and joining someone else's workspace are
**shell** surfaces and take shell roles — a Panel with Control rows and a
Selector for scope, not a bespoke dialog per entry point. Specifically:

- Presence is not a toast that vanishes. Who is in this workspace is legible
  from a persistent roster in the Bar; a toast may *announce* a change but never
  be the only place the fact is available.
- Every asynchronous collaboration action has a **designed pending, failure and
  empty state**. "Waiting for peer", "peer refused", "no peers found" are
  states, not log lines.
- A peer address the user has to type is a `TextInput` with validation and a
  paste path, never a painted field, and never the only way in — discovery and
  a share code are the primary routes.
- Ordinary prose `TextInput`s spell-check by default. Fields whose contents are
  structured opt out with semantic intent: `TextInputKind::Search`, `Name`,
  `Username`, `Email`, `Url`, `Path`, `Code`, `Number`, or `Password`. Shared
  search, number, code, secret, and editable-name widgets set that intent for
  their callers automatically. `with_spellcheck(true)` explicitly opts a
  structured field back in. Set `with_spell_language` when a field's content
  language differs from the session locale. High-confidence autocorrection
  happens only at a word boundary, respects Settings and personal vocabulary,
  and must be immediately reversible with Backspace.
- Per-peer colour comes from `presence::user_color` and is used consistently in
  every surface that mentions that peer (roster, cursor, tab badge, invite row).

### 9.13 Enforcement

Every look rule that can be seen in source is a **ratchet** in
`crates/sdk/ui-core/tests/`. A ratchet counts each pattern per production
file (comments and `#[cfg(test)]` items are skipped) in the toolkit, the shell
and every app, and the count may only fall. A change that adds a hit fails. A
change that removes one passes, and the run notes the lower count so the
baseline can be lowered in that change or a later one. Apps are scanned
(`apps/*/src`, so a new app is covered the day it lands), and every root must
yield files or the ratchet fails rather than passing empty.

| Audit (baseline dir) | Scans | Keys | Test |
|---|---|---|---|
| `theme` | ui-core `src`, shell `src`, `apps/*/src`; not `theme.rs`, `tokens/`, `presence/render.rs` | `literal_color`, `white_or_black`, `theme_blind_white` | `design_token_audit.rs::theme_escape_hatches_do_not_grow` |
| `app-paint` | shell `src`, `apps/*/src`; same exemptions | `literal_color_new`, `ramp_slot`, `legacy_alias`, `desktop_ground`, `literal_radius`, `literal_alpha`, `raw_paint`, `glyph_icon`, `transparent_shot` | `design_token_audit.rs::app_and_shell_paint_knobs_do_not_grow` |
| `chrome-border` | ui-core `src/widget`, `src/chrome`, `surface.rs`, `status_bubble.rs`, shell `src` | `declared_border` | `design_token_audit.rs::declared_borders_in_chrome_do_not_grow` |
| `spacing` | ui-core `src/widget`, `src/chrome`, shell `src`, `apps/*/src` | `pixel_const`, `taffy_length`, `literal_gap`, `literal_spacer` | `spacing_token_audit.rs::spacing_audit_sees_what_it_polices_and_literals_do_not_grow` |
| `typography` | ui-core `src/widget`, `src/chrome`, shell `src`, `apps/*/src`; not `tokens/typography.rs`, `text_render*`, `text_metrics.rs` | `literal_font_size`, `legacy_size_api`, `font_size_const`, `type_step_arithmetic`, `raw_type_step`, `renderer_scale_helper` | `typography_token_audit.rs::typography_literals_do_not_grow` |
| `copy` | `apps/*/src` | `all_caps_label`, `middle_dot_join`, `to_uppercase` | `copy_audit.rs::copy_rules_do_not_grow` |

What each key counts (the test file pins every key against lines that must
and must not match):

- `literal_color`: `Color::from_rgb8(` / `from_rgba8(` with a literal first
  argument. Match both constructor spellings (see §8: grepping only for
  `Color::rgb` is how these accumulated under a green check).
- `white_or_black`: `Color::WHITE` / `Color::BLACK`. A colour is a colour even
  when it has a name; the theme has ink and scrim tokens.
- `theme_blind_white`: `surface_white_*` / `border_white_*`, a declared
  strength never measured against its ground (§9.5).
- `literal_color_new`: `Color::new([<literal>, …])`.
- `ramp_slot`: `.neutral[…]` or `accent[<digit>]`; use the semantic tier.
- `legacy_alias`: `.bg_app`, `.bg_primary`, `.bg_hover`, `.bg_active`.
- `desktop_ground`: `surfaces.base` / `surfaces.window_card` outside the
  desktop. The framework paints the window ground; apps declare regions.
- `literal_radius`: a numeric literal as a corner radius
  (`to_rounded_rect(8.0)`, `RoundedRect::from_rect(r, 6.0)`,
  `fill_round(s, r, 4.0, …)`, `.radius(12.0)`).
- `literal_alpha`: `with_alpha(0.5)`, `with_alpha_factor(x * 0.4)`,
  `multiply_alpha(0.6)`: a fractional literal opacity.
- `raw_paint`: `scene.fill(` / `scene.stroke(`. Chrome is composed from
  widgets; only genuine content drawing paints, from theme values.
- `glyph_icon`: a string or char literal containing `✕ × ⟳ ↻ ⌫ ⇅ « » ‹ › ▾ ▸ ▴
  ◍ ◎ ◷ ⚓ ➤ ⏰` or an emoji (U+1F300–U+1FAFF); use `ui::icons`.
- `transparent_shot`: `render_png(…TRANSPARENT…)`; shots render on the
  framework ground, like the live window.
- `declared_border`: `.border_standard`, `.border_strong`,
  `overlay_border_strong` or `colors.neutral[` on a chrome surface (§9.5).
- `pixel_const`: a module-level `const NAME: f32|f64 = <whole number ≥ 1>;`.
- `taffy_length`: a non-zero literal in taffy's `length(…)`.
- `literal_gap`: `.with_gap(` / `.with_padding(` / `.with_row_gap(` /
  `.with_column_gap(` with a literal.
- `literal_spacer`: `Spacer::fixed(<literal>)`.
- `literal_font_size`: a literal size passed to `TextStyle::new`,
  `.with_size`, `.with_scaled_size`, `.with_fixed_size`, `.text_size`,
  `.with_font_size` or `.set_font_size`.
- `legacy_size_api`: the pixel-size text calls `.draw_text(`,
  `.draw_text_weighted(`, `.measure(`, `.measure_weighted(`,
  `.measure_styled(`, `.elide_to_width(`, `.elide_to_width_weighted(`.
- `font_size_const`: an `f32` constant whose name contains `FONT`, `TEXT`,
  `SIZE`, `TITLE`, `LABEL`, `CAPTION` or `HEADING` (`*CONTENT_PX` document
  sizes excepted).
- `type_step_arithmetic`: arithmetic on `typography.scale.<step>`.
- `raw_type_step`: any `typography.scale.` read or `TypeScale::default()`.
- `renderer_scale_helper`: `renderer_input_style` / `renderer_type_size`.
- `all_caps_label`: a string literal opening with an upper-case word of three
  or more letters (not an acronym such as `PDF`, `HTTP`, `GET`) on a line
  that hands text to a label, title, caption, heading, header, placeholder,
  tooltip, button, `Text` or a draw/measure/elide call.
- `middle_dot_join`: a string literal containing ` · ` (an `A · B · C`
  metadata chain).
- `to_uppercase`: `.to_uppercase()`.

Hard failures, with no inherited debt, in `crates/sdk/ui-core/src`:
`cosmic_text::Color::rgb(<literal>…)`, `.to_rounded_rect(<literal>)` and a
literal shadow `blur:` (`design_token_audit.rs::no_hardcoded_style_literals_in_tree`).

**Baselines are sharded per unit** so app changes never conflict:
`crates/sdk/ui-core/tests/baselines/<audit>/<unit>.txt`, where the unit is
`apps-<name>`, `ui-core` or `shell`. Each line is `<path relative to the
repository root> <key> <count>`. A missing shard allows zero, so a unit that
reaches zero deletes its shard and is locked at zero from then on.

Updating a shard:

- With cargo (CI, or a checkout with a local toolchain):
  `UPDATE_RATCHET_BASELINES=1 RATCHET_SCOPE=apps/mail cargo test -p tilo-ui --test it do_not_grow`
  rewrites only that unit's shards (`RATCHET_SCOPE` also takes
  `crates/session/shell`, `crates/sdk/ui-core` or a unit name). Without
  `RATCHET_SCOPE` every shard is rewritten. An update refuses to raise any
  count unless `RATCHET_ALLOW_GROWTH=1` is also set; set it only after
  reviewing why each count grew.
- With rcargo, which neither forwards the environment nor brings a test's
  writes back from the builder:
  `rcargo test -p tilo-ui --test it do_not_grow --success-output immediate`.
  A ratchet with progress prints the stale entries and the full text each
  affected shard should now hold (or `delete <path>` when the unit reached
  zero). Write exactly that.

**Inlining to dodge a ratchet is a defect.** Moving a literal into a local,
renaming a constant so its name no longer matches, splitting a call over
lines, or inlining "whole-number sizes at their call sites because this file
carries no `pixel_const` to spare" (the justification once written above two
font constants in `apps/git-client/src/app.rs`) hides the debt from the
ratchet without paying any of it. The fix is the semantic API that removes
the knob: a `TextRole`, a spacing token, a region, a widget. Never write a
comment that explains why a value was shaped to avoid a ratchet; a hit that
is genuinely correct (a document colour, a chart's data ink) stays in the
shard with its count.

Not automated yet; grep a diff for these:

- `Duration::from_millis(` with a literal in an animation (use
  `tokens::motion`)
- a `Command::new` / blocking read reachable from `draw`

`docs/ui-migration.md` maps each key to the API that replaces it.

---

*This document is versioned with the toolkit. When a guideline here becomes a
`ui` primitive, link the primitive from the relevant section so the rule and its
enforcement stay together.*

## Form composition

For content-first account/settings forms, use `FormPage`, `Form`, `FormSection` and `Field`; see [automatic and guided forms](FORM_COMPOSITION.md) for the API, independent override/reset rules and runnable primitive comparison. Manual composition remains supported with the same standard controls.
