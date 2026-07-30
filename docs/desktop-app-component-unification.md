# Desktop ↔ app component unification

**What this is.** The companion to `docs/hand-rolled-ui-audit.md`. That audit
swept the **apps** under `apps/`; this one sweeps the **desktop chrome** — the
`shell` compositor's own UI (wheel disc, top bar, overlays, notifications,
palette, prompts, window pills). The OS owns chrome (titlebars, roster, perms,
search, notifications, theme), so the desktop and an app's in-content controls
must render through the **same** `ui` widget, not two look-alikes. Every place
the compositor hand-paints an interactive primitive it should be reusing is a
`ui/HIG.md` §8 violation and a source of drift.

**Method.** For each chrome area, graded hand-rolled only where it reimplements
a widget that already exists in `ui/src/widget/` rather than legitimately
composing it. Evidence is `file:line`/`fn`. Risk key matches the app audit:
`HIGH` = breaks clipboard/selection/IME/a11y (text), `MED` = misses
hover/press/focus/keyboard/a11y-role/theme (buttons, controls, modals, rows),
`LOW` = cosmetic.

---

## Already unified (the references to copy)

These desktop surfaces already route through the shared widget — the target
shape for everything below:

| Chrome | Renders via | Evidence |
|---|---|---|
| Window-group floating pill / titlebar / tab strip | `ui::widget::WindowGroup` + `ui::widget::tabs::TabBar` | shell transforms state → widget; no bespoke tab/close/label paint |
| Ctrl+K global search palette | `ui::widget::command_palette::CommandPalette` | `shell/src/shell/global_search.rs` is a thin seed/filter wrapper |
| Transient file-access / permission prompt | `ui::widget::permission_prompt::PermissionPrompt` | mounted by `overlay_host.rs`; `set_rect` + `draw` only |
| **Notification / toast stack** | **`ui::widget::toast` (`paint_card`)** | **done this pass — see below** |

---

## Duplication inventory (hand-rolled desktop chrome)

| # | Chrome piece | file:symbol | Hand-drawn primitive | `ui` widget it should use | ~LOC | Risk | Duplication note |
|---|---|---|---|---|---|---|---|
| 1 | Top-bar collaborator avatar stack | `shell/topbar_chrome.rs:1147-1220` (`Circle::new(center,9.0)` fill + `Circle(7.5)` + initials + `"+N"` at `:1196`) | initials avatar chip + presence ring + overflow chip; interactive (`avatar_stack_rect` click toggles collaborators at `:1372`) with a manual hit-test — no focus/keyboard/a11y | `ui::Avatar::draw_at` + `color_for_initials`/`initials_for_name`; overflow `ui::Badge` | ~80 | MED | Near-verbatim of #2/#3 |
| 2 | Wheel-card presence dots | `shell/sidebar_wheel_chrome.rs:573-608` (`Circle::new(Point,dot_r)` loop + `"+{overflow}"` at `:600`) | per-card presence dots + `"+N"` chip | `ui::Avatar::draw_at` (dot = empty-initials chip) + `ui::Badge` | ~60 | LOW | Verbatim of #1/#3 |
| 3 | Wheel-disc active-card collaborators | `shell/sidebar_wheel_disc.rs:~795-829` | presence dots + overflow | `ui::Avatar` + `ui::Badge` | ~40 | LOW | Verbatim of #1/#2 |
| 4 | Overview / ws-settings / collaborators-popover peers | `overlay_host.rs draw_overview:6345`, `draw_ws_settings:7027`, `draw_collaborators:8621` | more presence-avatar loops (initials circle + colour hash) | `ui::Avatar` | ~90 | MED | Same avatar, 3 more copies |
| 5 | Quick-settings Wi‑Fi list | `overlay_host.rs draw_qs_wifi_list:8402` | painted row + signal glyph + manual hit-test | list `GroupRow` / `ui::Toggle` per row | ~110 | MED | ≈ #6 (bt list) |
| 6 | Quick-settings Bluetooth list | `overlay_host.rs draw_qs_bt_list:8515` | painted row + manual hit-test | list `GroupRow` / `ui::Toggle` per row | ~100 | MED | Near-verbatim of #5 |
| 7 | Quick-settings tiles + brightness/volume | `overlay_host.rs draw_quick_settings:7983` (+ `draw_qs_drill_in_header:8312`) | tile toggles + sliders + drill-in rows painted | `ui::Toggle::paint` + `ui::Slider` + `ui::Button::paint` | ~220 | MED | Toggle/slider re-rolled |
| 8 | Workspace-settings sheet | `overlay_host.rs draw_ws_settings:7027` | one fn reimplements scrim+panel, tab bar, name field, Grant/Revoke buttons, member avatars, cap badges | `ui::ModalDialog` + `ui::ToggleGroup` + `ui::TextInput` + `ui::Button` + `ui::Avatar` + `ui::Badge` | ~457 | HIGH (name field) | Whole modal stack re-rolled |
| 9 | Workspace-settings grant composer | `shell/workspace_settings.rs` (Grant/Revoke rows, cyclers, subject/verb pickers) | painted rows + buttons + cyclers, manual hit-test | `ui::Button` + `ui::Dropdown`/`ui::segmented` + list rows | ~960 | MED | Largest single file of chrome debt |
| 10 | Modal scrim + panel pattern | `overlay_host.rs draw_scrim:6074` / `draw_scrim_at:6080` / `draw_modal_scrim_at:6086`, reused at `:3993 :4004 :4058 :4212 :4263` | bespoke scrim + centered panel around palette/overview/ws-settings/quick-settings/peer-sync | `ui::ModalDialog` (themed scrim + focus trap + Esc + a11y `Dialog` role) | ~120 | MED | 5 call sites of one fork |
| 11 | Peer-sync sheet | `overlay_host.rs draw_peer_sync:7689` (+ `draw_real_peers:7568`, `draw_demo_peer:7490`) | scrim+panel + peer rows + Copy/Sync buttons | `ui::ModalDialog` + `ui::Avatar` + `ui::Button` | ~180 | MED | Reuses #10 scrim + #4 avatar |
| 12 | Filter chips / scope switch / WS-id pill / DND pill | `overlay_host.rs draw_filter_chip:473`; DND pill `notification.rs draw` (`:505`); scope pills + WS-id pill in `topbar_chrome.rs`/`sidebar_wheel_*` | painted rounded rect + label, manual hit-test | `ui::Chip` / `ui::Badge` / `ui::ToggleGroup::paint` (Pill) | ~130 | LOW–MED | The pill shape recurs 4+ places |
| 13 | Scattered action buttons | add-workspace `+` (`overlay_host.rs draw_overview` add-rect), Copy/Sync (peer-sync), qs action tiles, search-banner button (`draw_search_banner:7852`) | painted rect + `.contains()` hit-test | `ui::Button::paint(&ButtonFace)` (retained `Button` where a tree exists) | ~150 | MED | Same anti-pattern copied |
| 14 | Notification toast card | `shell/notification.rs NotificationStack::draw` | ~~severity dot + title/body/caption + close-`X` lines + chevron + shadow, all hand-painted; self-declared "richer fork of `ToastStack`"~~ | **`ui::widget::toast::paint_card`** | ~150 | MED | **DONE — see below** |

### Cross-app duplication (why this is the same debt as the app audit)

Every widget in the right-hand column above is one an **app** also hand-rolls
(per `docs/hand-rolled-ui-audit.md`): `Avatar` (roster/mention chips in chat,
contacts, mail), `Toast` (in-app toasts), `Button`/`Toggle`/`Slider` (Category
2/4), `ModalDialog` (Category 3), `Chip`/`Badge`. So a desktop unification and
an app unification converge on the **same** `ui` primitive — fixing one raises
the other for free, and locks the desktop and its apps to one look.

---

## Ranked wave plan (biggest exact-duplication / worst UX first)

Each wave is independently shippable and maps cleanly to one follow-up agent.
Waves that need a `ui` widget extended say so up front (extend, never fork).

### Wave 0 — Notification toast → shared toast card  ✅ DONE (this pass)
Routed `shell/notification.rs` through a new shared `ui::widget::toast::paint_card`;
added a `Ghost` `ButtonStyle` for the dismiss `X`; deleted the bespoke card
painter; added an a11y live region. Details below.

### Wave 1 — Collaborator avatar / presence cluster → `ui::Avatar` + `ui::Badge`
**Biggest exact-duplication in the whole desktop.** The same initials-circle +
presence-dot + `"+N"` overflow is hand-painted in **6** places (#1–#4), three of
them near-verbatim. `ui::Avatar::draw_at` / `initials_for_name` /
`color_for_initials` are drop-in immediate-mode entries and *improve* the result
(per-identity hashed colour + initials instead of blank dots). No `ui` change
needed. Convert the interactive top-bar stack (#1) first — it's the one with a
manual hit-test and no a11y. **Apps affected:** none directly, but the same
`Avatar` backs chat/contacts/mail rosters.

### Wave 2 — Modal scrim + panel pattern → `ui::ModalDialog`
The scrim helper (#10) is forked once and reused at 5 call sites; the panels it
frames (#8 ws-settings, #7 quick-settings, #11 peer-sync) each re-roll the
centered card. `ui::ModalDialog` already gives themed scrim + centered box +
Esc + backdrop-dismiss + **focus trap** + a11y `Dialog` role — everything these
lack. Do the scrim + the simplest panel (peer-sync) first; ws-settings is Wave 4.

### Wave 3 — Quick-settings rows / toggles / sliders → `ui::Toggle` / `ui::Slider` / rows
#5–#7. The Wi‑Fi and Bluetooth lists are near-verbatim; the tiles re-roll
toggles and the brightness/volume sliders re-roll `Slider`. Use the immediate-
mode `ui::Toggle::paint` (already exists) + `ui::Slider`; extract one shared
list-row. **`ui` gap:** `Slider` binds to a CRDT path (audit gap 7); the QS
sliders bind to a non-CRDT value — extend `Slider` with a plain-value binding
first (same gap tube/music/video_editor hit), then adopt.

### Wave 4 — `draw_ws_settings` internals → the widget stack
#8. One ~457-LOC function reimplements a whole modal: scrim (Wave 2), tab bar
(`ui::ToggleGroup`), workspace-name field (`ui::TextInput` — the one HIGH-risk
text input in the chrome, splices raw keys today), Grant/Revoke (`ui::Button`),
member rows (`ui::Avatar` — Wave 1), cap badges (`ui::Badge`). Land after Waves
1–2 supply the pieces.

### Wave 5 — Workspace-settings grant composer → `ui::Button`/`ui::Dropdown`/rows
#9, `shell/workspace_settings.rs` (~960 LOC) — the single largest chrome file.
Painted grant rows, subject/verb cyclers, and action buttons → `ui::Button`,
`ui::Dropdown`/`ui::segmented`, shared list rows. Security-sensitive (this is
the sandbox-grant UI) so convert paint-only first, keep the authority path
untouched.

### Wave 6 — Chips / pills → `ui::Chip` / `ui::Badge` / `ui::ToggleGroup`
#12. Collapse the filter chip, scope-switch pills, workspace-id pill, and DND
pill onto one pill primitive. Low risk, high consistency; mostly a paint-swap.

### Wave 7 — Scattered action buttons → `ui::Button::paint(&ButtonFace)`
#13. The add-workspace `+`, Copy/Sync, qs action tiles, and search-banner button
are the same painted-rect + `.contains()` anti-pattern. The immediate-mode
`Button::paint` face (now including the `Ghost` style added this pass) covers the
canvas cases; mount the retained `Button` where a widget tree already exists.

---

## Done this pass — Notification toast → `ui::widget::toast`

**Problem.** `shell/notification.rs` opened with a comment declaring itself a
"richer fork" of `ui::widget::toast::ToastStack`, then hand-painted every card:
a severity **dot** (every kind drew the same shape), title/body/caption text, a
close **`X` drawn as two `Line`s** with a manual hit-test (no keyboard, no a11y),
an actionable chevron, and its own shadow + fill + border + fade. Meanwhile
`ui::ToastStack::draw` painted a *second*, thinner copy of the same card.

**Fix (extend, don't fork — HIG §8).**
- `ui/src/widget/toast.rs`: added one immediate-mode card renderer,
  `paint_card(scene, …, rect, &ToastCard)`, that both surfaces now call. It
  paints soft shadow → raised surface + border → **left severity accent stripe**
  (the HIG §7 "state reads at a glance" form, replacing the dot) → attribution
  caption / title / body → a real ghost **dismiss `Button`** → actionable
  chevron, all modulated by an `alpha` the caller drives for enter/exit. It
  returns the dismiss-button rect so the caller keeps one hit-test on
  widget-owned geometry. `ToastStack::draw` now routes through it too (its
  bespoke card painter deleted).
- `ui/src/widget/button.rs`: added a `Ghost` `ButtonStyle` (transparent +
  borderless at rest, tints to `bg_hover`/`bg_active` on interaction, dim
  label/icon) — the variant the audit flags as missing for git_client / notes /
  logs, now backing the notification close `X`.
- `shell/src/shell/notification.rs`: the per-notification paint loop (~150 LOC of
  dots/text/close-lines/chevron/shadow) is replaced by one `paint_card` call;
  `NotificationSeverity::toast_kind()` maps onto the shared severity ramp; the
  stack gained an `accessibility()` **live region** (assertive; redacts to a
  count while locked) matching `ToastStack`.

**Kept intact (OS policy stays in the shell):** the DND status pill, the
lock-screen redaction card, stacking, expiry/animation, D-Bus attribution, and
the click routing (`notify_click_at`, now hit-testing the widget-returned close
rect).
