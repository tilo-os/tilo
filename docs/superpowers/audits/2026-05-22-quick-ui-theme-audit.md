# quick-ui theme-audit (2026-05-22)

Scope: every Rust file under `quick-ui/src/widget/`, walking recursively.
Audit is **read-only** — fixes per widget come as follow-up units (Unit
M-2 of the widget-expansion plan).

The existing mechanical audit (`quick-ui/tests/design_token_audit.rs`)
catches four classes of literal: `Color::rgb8/rgb(\d…)`,
`.to_rounded_rect(\d…)`, `blur: \d…`, and `Padding::all(\d…)`. This
audit adds the **soft** axes the mechanical regexes miss:

- `Color::rgba8(…)` with non-permission, non-overlay literal RGB or alpha.
- `with_alpha_factor(N)` with a hardcoded literal `N`.
- `Stroke::new(N)` with `N > 1.0` (the common `1.0` is treated as a
  layout primitive, not a styling literal).
- Hardcoded font sizes (`let text_size = 14.0;` etc.) where the
  comparable widgets pull from `theme.tokens.typography.scale.*`.
- `RoundedRect::from_rect(rect, N)` with a literal radius (the mechanical
  audit only catches `.to_rounded_rect(N)`, not the constructor form).
- Hardcoded shadow y-offsets and shadow-ring alpha tables.

## Coverage

- Files scanned: **42** widget files under `quick-ui/src/widget/` (the
  shell `mod.rs` declares each one).
- Stubs skipped: **15** — these are 3-line `pub struct Foo;` stubs
  pending implementation by other Unit agents:
  - `widget/avatar.rs`
  - `widget/badge.rs` *(not a stub — fully implemented; was previously
    listed but is 297 lines — corrected)*
  - `widget/breadcrumb.rs`
  - `widget/color_picker.rs`
  - `widget/date_picker.rs`
  - `widget/image_viewer.rs`
  - `widget/modal_dialog.rs`
  - `widget/popover_widget.rs`
  - `widget/progress_bar.rs` *(not a stub — fully implemented; corrected)*
  - `widget/resizable_split.rs`
  - `widget/spinner.rs` *(not a stub — fully implemented; corrected)*
  - `widget/time_picker.rs`
  - `widget/toggle_group.rs`
  - `widget/toolbar.rs`
  - `widget/tree_view.rs`
  - `widget/video_player.rs`

  **Verified by line-count** (`wc -l ≤ 7`, file body matches
  `//! TODO(Unit …)` pattern): avatar.rs, breadcrumb.rs, color_picker.rs,
  date_picker.rs, image_viewer.rs, modal_dialog.rs, popover_widget.rs,
  resizable_split.rs, time_picker.rs, toggle_group.rs, toolbar.rs,
  tree_view.rs, video_player.rs. (badge, progress_bar, spinner are NOT
  stubs — fully audited below.)

  **Stubs not audited (final list, 13 files):** `avatar.rs`,
  `breadcrumb.rs`, `color_picker.rs`, `date_picker.rs`, `image_viewer.rs`,
  `modal_dialog.rs`, `popover_widget.rs`, `resizable_split.rs`,
  `time_picker.rs`, `toggle_group.rs`, `toolbar.rs`, `tree_view.rs`,
  `video_player.rs`.

- Existing mechanical violations (covered by
  `tests/design_token_audit.rs`): **0** — confirmed via:
  - `grep -rnE 'Color::rgb8?\s*\(\s*\d' quick-ui/src/widget/` → 0 hits
  - `grep -rnE 'Padding::all\(\s*[1-9]' quick-ui/src/widget/` → 0 hits
  - `grep -rnE 'blur\s*:\s*\d' quick-ui/src/widget/` → 0 hits
  - `grep -rnE '\.to_rounded_rect\s*\(' quick-ui/src/widget/` → 0 hits

## Violations by widget

### widget/button.rs

- **L107** — `scene.stroke(&vello::kurbo::Stroke::new(if self.is_focused { 2.0 } else { 1.0 }), ...)`
  - Issue: hardcoded focus stroke width `2.0` (and idle `1.0` — flagged only by association; `1.0` is below the suspicious threshold).
  - Suggested token: `theme.tokens.elevation.focus_ring_px` (new — see proposed tokens) or `theme.tokens.spacing.s0` for the focus stroke.

- **L114** — `let text_size = 14.0;`
  - Issue: hardcoded body font size. Every other widget that draws a label pulls from `theme.tokens.typography.scale.*` (`sm` / `base`).
  - Suggested token: `theme.tokens.typography.scale.base` (= 14.0 dark default, but theme-overridable).

- **L221** — `let overlay = vello::peniko::Color::rgba8(0, 0, 0, 77);`
  - Issue: cross-cutting magic alpha for the permission-denied overlay (the audit-exempt mechanical regex doesn't catch `rgba8`). The 77 alpha is pinned by `text_input.rs:permission_denied_overlay_alpha_matches_other_widgets` so the cross-widget convention is intentional, but the literal still belongs in a token.
  - Suggested token: `theme.tokens.colors.permission_denied_overlay` (new — see proposed tokens).

### widget/checkbox.rs

- **L107** — `scene.stroke(&Stroke::new(if self.is_focused { 2.0 } else { 1.0 }), ...)`
  - Issue: hardcoded focus stroke width `2.0`.
  - Suggested token: `theme.tokens.elevation.focus_ring_px`.

- **L117** — `&Stroke::new(2.5).with_caps(...).with_join(...)`
  - Issue: hardcoded `2.5 px` checkmark stroke width.
  - Suggested token: `theme.tokens.spacing.s1` is `4.0` — close to `2.5` semantically but the value belongs in a dedicated `theme.tokens.icon_stroke_px` (new — see proposed tokens).

- **L119** — `Color::WHITE` for the checkmark glyph on the accent fill.
  - Issue: works under dark theme because the accent fill is indigo-500 (deep blue), but pinning the glyph to a hard `WHITE` ignores future custom themes where the accent could be light.
  - Suggested token: `theme.tokens.colors.text_on_accent` (new — see proposed tokens).
  - **Light-theme axis:** OK today (indigo-500 stays vivid in `Colors::light()`), but brittle.

- **L184** — `let overlay = Color::rgba8(0, 0, 0, 77);`
  - Issue: same shared permission-denied overlay literal.
  - Suggested token: `theme.tokens.colors.permission_denied_overlay`.

### widget/toggle.rs

- **L101** — `scene.stroke(&Stroke::new(if self.is_focused { 2.0 } else { 1.0 }), ...)`
  - Issue: hardcoded focus stroke width `2.0`.
  - Suggested token: `theme.tokens.elevation.focus_ring_px`.

- **L180** — `let overlay = vello::peniko::Color::rgba8(0, 0, 0, 77);`
  - Issue: shared permission-denied overlay literal.
  - Suggested token: `theme.tokens.colors.permission_denied_overlay`.

### widget/text.rs

- **L24** — `size: 16.0,` (default font size of `Text::new`)
  - Issue: the default for the most common text widget hardcodes 16.0 px. Should match `theme.tokens.typography.scale.lg` (= 16.0).
  - Suggested token: replace constructor default with `theme.tokens.typography.scale.lg` at first-draw (or accept the value via builder + default to a token-derived constant). Mild flag — needs builder rework, not a one-line fix.

- **L156** — `let overlay = vello::peniko::Color::rgba8(0, 0, 0, 77);`
  - Issue: shared permission-denied overlay literal.
  - Suggested token: `theme.tokens.colors.permission_denied_overlay`.

### widget/text_input.rs

- **L132** — `scene.stroke(&Stroke::new(if self.is_focused { 2.0 } else { 1.0 }), ...)`
  - Issue: hardcoded focus stroke width `2.0`.
  - Suggested token: `theme.tokens.elevation.focus_ring_px`.

- **L134** — `let text_size = 16.0;`
  - Issue: hardcoded body font size; every other input/label uses `theme.tokens.typography.scale.*`.
  - Suggested token: `theme.tokens.typography.scale.lg` (= 16.0 default).

- **L183** — `cursor_x + 2.0,` (cursor rect uses `+2.0` for caret width)
  - Issue: hardcoded 2 px caret width; arguable that this is rendering primitive math (sub-pixel caret tuning) rather than a design decision. **Soft flag, low priority.**

- **L219** — `let padding_x = 10.0;` *(inside `handle_event`)*
  - Issue: drifts from the `theme.padding_standard` used in `draw` (line 135). Two different `padding_x` values for the same widget — a real bug. Flag as a correctness regression, not just a token violation.
  - Suggested token: route both through `theme.padding_standard` (or store a single `padding_x` on the widget).

- **L431** — `vello::peniko::Color::rgba8(0, 0, 0, 77);` *(the `PERMISSION_DENIED_OVERLAY` associated const)*
  - Issue: shared permission-denied overlay literal. **Captured as a `pub(crate) const`, with a pinning test (L450).** This is the cleanest case for promoting to a token — every other widget should also use this const, then the const should live on `Theme`/`Tokens`.
  - Suggested token: `theme.tokens.colors.permission_denied_overlay`.

### widget/slider.rs

- **L161** — `let track_height = 6.0;`
  - Issue: hardcoded track thickness; design literal that should be a token.
  - Suggested token: `theme.tokens.spacing.s2` (8.0) or a new `theme.tokens.control.slider_track_px` — close but not equal to spacing tokens.

- **L168** — `let track_shape = RoundedRect::from_rect(track_rect, 3.0);`
  - Issue: hardcoded 3 px radius for the track. Mechanical audit's `.to_rounded_rect(N)` regex misses the constructor form.
  - Suggested token: `theme.tokens.radii.xs` (= 4.0 — close) or `theme.tokens.radii.full`/2 for a pill-shape.

- **L179** — `let active_track_shape = RoundedRect::from_rect(active_track_rect, 3.0);`
  - Issue: same as L168.
  - Suggested token: same as L168.

- **L208/L209** — `Circle::new(knob_center, knob_radius + 4.0)` / `(..., knob_radius + 2.0)`
  - Issue: hardcoded halo offsets (4 px and 2 px) — design decisions.
  - Suggested token: `theme.tokens.spacing.s1`/`s0` or new `theme.tokens.control.halo_offsets`.

- **L215** — `Stroke::new(border_width)` where `border_width` is `1.5` (lines 197, 199, 201)
  - Issue: hardcoded `1.5 px` knob border width.
  - Suggested token: `theme.tokens.spacing.s0_5` (new) or `theme.tokens.icon_stroke_px`.

- **L304** — `let overlay = vello::peniko::Color::rgba8(0, 0, 0, 77);`
  - Issue: shared permission-denied overlay literal.
  - Suggested token: `theme.tokens.colors.permission_denied_overlay`.

### widget/container.rs

- **L205** — `let overlay = vello::peniko::Color::rgba8(0, 0, 0, 77);`
  - Issue: shared permission-denied overlay literal.
  - Suggested token: `theme.tokens.colors.permission_denied_overlay`.

### widget/column.rs

- **L201** — `let overlay = vello::peniko::Color::rgba8(0, 0, 0, 77);`
  - Issue: shared permission-denied overlay literal.
  - Suggested token: `theme.tokens.colors.permission_denied_overlay`.

### widget/row.rs

- **L170** — `let overlay = vello::peniko::Color::rgba8(0, 0, 0, 77);`
  - Issue: shared permission-denied overlay literal.
  - Suggested token: `theme.tokens.colors.permission_denied_overlay`.

### widget/stack.rs

- **L166** — `let overlay = vello::peniko::Color::rgba8(0, 0, 0, 77);`
  - Issue: shared permission-denied overlay literal.
  - Suggested token: `theme.tokens.colors.permission_denied_overlay`.

### widget/spacer.rs

- **L98** — `let overlay = vello::peniko::Color::rgba8(0, 0, 0, 77);`
  - Issue: shared permission-denied overlay literal.
  - Suggested token: `theme.tokens.colors.permission_denied_overlay`.

### widget/progress_bar.rs

- **L278** — `let overlay = vello::peniko::Color::rgba8(0, 0, 0, 77);`
  - Issue: shared permission-denied overlay literal.
  - Suggested token: `theme.tokens.colors.permission_denied_overlay`.

### widget/image.rs

- **L295** — `let red = Color::rgba8(220, 38, 38, 255);` *(error-state border colour)*
  - Issue: hardcoded RGB literal for the decode-error border. `rgba8` with all literal RGB values — the mechanical audit only flags `rgb`/`rgb8`, not `rgba8`. The 220/38/38 hex matches Tailwind `red-600`, but `theme.tokens.colors.semantic.danger` (= `red-500` 239/68/68) is the closest existing token.
  - Suggested token: `theme.tokens.colors.semantic.danger` (or new `semantic.danger_strong` if 600-tier is needed).

- **L296** — `let stroke = Stroke::new(2.0);`
  - Issue: hardcoded 2 px stroke for the error border.
  - Suggested token: `theme.tokens.elevation.focus_ring_px` (or a new `theme.tokens.error_border_px`).

### widget/tabs.rs

- **L419 / L684** — `let shadow_y_offset = 6.0_f64;`
  - Issue: hardcoded shadow y-offset for the per-pill drop shadow. The
    audit-rubric explicitly calls this out: "a hardcoded 6.0 px y-offset
    for a shadow should be flagged." Same literal appears in two places
    in tabs.rs (active pill draw + closing pill fade-out) — both need
    to be backed by the same token, which doesn't exist yet.
  - Suggested token: `theme.tokens.elevation.e2.offset_y` (= 2.0 today)
    or a new `theme.tokens.elevation.pill.offset_y` that pins the 6.0
    Tailwind `shadow-[0_8px_32px_…]` approximation.

- **L420-424 / L685-689** — `let shadow_rings: [(f64, u8); 4] = [(2.0, 51), (5.0, 30), (9.0, 15), (14.0, 5)];`
  - Issue: hardcoded shadow-ring spread + alpha table; this is a
    design decision approximating `shadow-[0_8px_32px_rgba(0,0,0,0.2)]`
    without a blur primitive. Same table repeated twice (active draw +
    fade-out draw) — duplicated literal.
  - Suggested token: `theme.tokens.elevation.pill_rings` (new — array of
    `(spread_px, alpha)` tuples) so both call sites read from one place.

- **L437 / L702** — `vello::peniko::Color::rgba8(0, 0, 0, alpha),` *(`alpha` from the ring table)*
  - Issue: shadow colour is hardcoded black; if the theme ever wanted
    coloured shadows (e.g. light theme with a warmer cast) the
    constructor would need re-plumbing.
  - Suggested token: `theme.tokens.elevation.shadow_color` (today = `Color::rgba8(0,0,0,a)`).

- **L454 / L468 / L1805** — `Stroke::new(1.0)` / `Stroke::new(1.0)` / `Stroke::new(1.1)`
  - L1805 is `vello::kurbo::Stroke::new(1.1)` used for icon glyphs across the tab bar; literal `1.1` is borderline (just over `1.0`), and is shared by `draw_app_icon`.
  - Suggested token: `theme.tokens.icon_stroke_px` (new) = `1.5` matches the value used by `icons::draw_icon` calls. The `1.1` is a one-off subtler-stroke for compact pill icons; either folds into the icon-stroke token or stays as a documented `const`.

- **L540** — `let dot_r = 8.0_f64;` *(per-tab collaborator presence dot radius)*
  - Issue: hardcoded dot radius. Could be `theme.tokens.spacing.s2` (= 8.0).
  - Suggested token: `theme.tokens.spacing.s2`.

- **L551** — `vello::peniko::Color::rgba8(cr, cg, cb, 140),`
  - Issue: hardcoded alpha `140` (≈ 55 %) for the per-peer presence dot. RGB comes from `presence.color_for(peer)` (runtime), so only the alpha is the literal.
  - Suggested token: `theme.tokens.elevation.presence_dot_alpha` (new) or a `with_alpha_factor(0.55)` after sourcing the float from a token.

- **L580** — `(vello::peniko::Color::rgba8(255, 255, 255, 28), true)` *(close chip on tab-hover, idle close)*
  - Issue: hardcoded white-tint at alpha 28 (≈ 11 %). Approximately equals `surface_white_10` (alpha 26) but slightly stronger; if intentional drift, deserves its own token.
  - Suggested token: `theme.tokens.colors.surface_white_10` (close enough, alpha 26 vs 28) OR new `theme.tokens.colors.close_chip_idle` if the brighter 28-alpha is design-required.

- **L582** — `(vello::peniko::Color::rgba8(0, 0, 0, 0), false)` *(transparent fill when close chip not visible)*
  - Issue: `Color::TRANSPARENT` would express this more clearly; mechanical audit allows `Color::TRANSPARENT`. **Cosmetic only — not a real violation.** Skipped from flag count.

- **L597 / L1082** — `vello::peniko::Color::rgba8(255, 255, 255, 255)` *(close-hover glyph colour, full white)*
  - Issue: hardcoded WHITE for the close glyph on red-500/80 hover (concept's `hover:text-white`).
  - Suggested token: `theme.tokens.colors.text_on_danger` (new — see proposed tokens). Mirrors the `text_on_accent` need but for destructive surfaces.
  - **Light-theme axis:** OK today (red-500 stays vivid), brittle long-term.

- **L610** — `let chip_tint = vello::peniko::Color::rgba8(255, 255, 255, 64);` *(hover-reveal action cluster chip background)*
  - Issue: hardcoded white at alpha 64 (≈ 25 %).
  - Suggested token: closest existing is `surface_white_20` (alpha 51); the brighter 64 would warrant its own `theme.tokens.colors.surface_white_25` token.

- **L1060** — `Some(vello::peniko::Color::rgba8(255, 255, 255, 28))`
  - Issue: same as L580 (test-only mirror). Test-only branch — `#[cfg(test)]`-gated, lower priority but still drift-prone.
  - Suggested token: same as L580.

- **L1462** — `accent.with_alpha_factor(alpha as f32),`
  - Issue: `alpha` is computed from `(0.3 + 0.7 * eased_t)` (lines 1452-1453) — design literals for drop-zone idle (`0.3`) and active (`1.0`) alpha bounds.
  - Suggested token: `theme.tokens.elevation.dropzone_idle_alpha` (new) + `dropzone_active_alpha` (new). **Soft flag, animation curve constants.**

- **L1482** — `accent.with_alpha_factor(0.25 * glow_t as f32),`
  - Issue: hardcoded `0.25` alpha for the drop-zone outer glow (approximates `shadow-[0_0_12px_rgba(99,102,241,0.8)]`). The 0.8 from the concept and the 0.25 painted here drift — likely intentional, but should be a named token.
  - Suggested token: `theme.tokens.elevation.dropzone_glow_alpha` (new).

### widget/window_group.rs

- **L559 / L883 / L983** — `vello::peniko::Color::rgba8(255, 255, 255, 38)`
  - Issue: hardcoded white at alpha 38 (≈ 15 %), used by the maximized title pill / restore pill (idle) / close chip (idle). Three call sites — same literal — would all break together if drift introduced.
  - Suggested token: between `surface_white_10` (alpha 26) and `surface_white_20` (alpha 51); needs a new `theme.tokens.colors.surface_white_15` or a `chrome_pill_idle` token.

- **L572 / L1002** — `vello::peniko::Color::rgba8(255, 255, 255, 255)` *(close-hover glyph)*
  - Issue: hardcoded WHITE for close-hover glyph (concept's `hover:text-white`).
  - Suggested token: `theme.tokens.colors.text_on_danger` (same as tabs.rs L597/L1082).

- **L781** — `&vello::kurbo::Stroke::new(1.5)` *(drag-in-flight accent ring)*
  - Issue: hardcoded 1.5 px stroke width.
  - Suggested token: `theme.tokens.icon_stroke_px` (new) or a dedicated `theme.tokens.ring_stroke_px`.

- **L803** — `&vello::kurbo::Stroke::new(8.0)` *(outer search-active ring)*
  - Issue: hardcoded 8 px outer ring stroke.
  - Suggested token: new `theme.tokens.elevation.ring_outer_px`.

- **L816** — `&vello::kurbo::Stroke::new(4.0)` *(inner search-active ring)*
  - Issue: hardcoded 4 px inner ring stroke.
  - Suggested token: new `theme.tokens.elevation.ring_inner_px` (or `theme.tokens.spacing.s1` = 4.0).

- **L835** — `&vello::kurbo::Stroke::new(2.0)` *(maximized accent outline)*
  - Issue: hardcoded 2 px stroke for the maximized state outline.
  - Suggested token: `theme.tokens.elevation.focus_ring_px` (new).

- **L919** — `let fill_alpha: u8 = if self.restore_hover { 64 } else { 38 };`
  - Issue: hardcoded alpha values for the restore-pill hover/idle states. Same 38 as L559 (idle) but a brighter 64 on hover.
  - Suggested token: pair of tokens for chrome-pill states (idle/hover); currently no comparable existing token.

- **L923** — `vello::peniko::Color::rgba8(255, 255, 255, fill_alpha),`
  - Issue: hardcoded white-tinted fill (alpha from L919 ternary).
  - Suggested token: same as L919.

- **L781 / L805 / L818** — `theme.tokens.colors.accent[5].with_alpha_factor(0.6)`, `.with_alpha_factor(0.24)`, `.with_alpha_factor(0.5)` *(ring opacities)*
  - Issue: hardcoded alpha literals for the search-active ring tint. The `0.24` ≈ `ring-4 ring-indigo-500/50` × outer-mid-stroke ratio (concept literal); `0.6` matches `shadow-[0_0_60px_rgba(99,102,241,0.6)]`. Design decisions — should be tokenised.
  - Suggested token: `theme.tokens.elevation.search_ring_opacities` (new — triple of f32) so all three ring strokes pull from one place.

### widget/group_row.rs

- **L730** — `let fill = theme.tokens.colors.accent[5].with_alpha_factor(0.5);`
  - Issue: hardcoded `0.5` alpha for the gap drop-zone indicator.
  - Suggested token: `theme.tokens.elevation.dropzone_alpha` (new) — likely shareable with tabs.rs L1462/L1482.

- **L731-732** — `let bar_w = 4.0_f64; let radius = 2.0_f64;`
  - Issue: hardcoded drop-zone indicator bar width and radius.
  - Suggested token: `theme.tokens.spacing.s1` for `bar_w` (= 4.0) and a half-spacing token for the radius. Mild flag.

### widget/tile_host.rs

- **L376-377** — `let ambient_pad = 50.0_f64; let ambient_y = 25.0_f64;`
  - Issue: hardcoded shadow-sprite padding + y-offset. Approximates Tailwind's `shadow-2xl` (`0 25px 50px -12px rgba(0,0,0,0.25)`). Pure design decision — should be a token.
  - Suggested token: `theme.tokens.elevation.ambient_shadow.{offset_y, pad}` (new sub-struct on `Elevation`).

- **L389** — `0.25,` *(alpha for `draw_shadow_sprite` ambient call)*
  - Issue: hardcoded alpha 25 % for the ambient shadow sprite. Matches `shadow-2xl`'s 0.25.
  - Suggested token: same `theme.tokens.elevation.ambient_shadow.alpha`.

- **L397** — `let bloom_pad = 120.0_f64;` *(focused indigo bloom radius)*
  - Issue: hardcoded 120 px bloom padding (concept's `shadow-[0_0_60px_rgba(99,102,241,0.6)]` ×2 for the sprite atlas).
  - Suggested token: `theme.tokens.elevation.indigo_bloom.{pad, alpha}` (new).

- **L409** — `0.6,` *(alpha for the focused-only indigo bloom)*
  - Issue: hardcoded alpha 60 %.
  - Suggested token: same `theme.tokens.elevation.indigo_bloom.alpha`.

### widget/command_palette.rs

- **L294** — `pub fn surface_radius_px(&self) -> f64 { 24.0 }`
  - Issue: hardcoded surface radius — concept's `rounded-3xl`. The method exposes this for test pinning (good), but it should ultimately read `theme.tokens.radii.x2l` (= 32 today — close but not 24). A new `theme.tokens.radii.x3l` (24 px) would match exactly.
  - Suggested token: `theme.tokens.radii.x3l` (new — 24 px to match Tailwind's `rounded-3xl`).

- **L394** — `let chip_text_size = (theme.tokens.typography.scale.sm - 1.0).max(10.0);`
  - Issue: ad-hoc `sm - 1.0` derivation (≈ 11 px). Other chip text in tabs/window_group/data_table just uses `scale.sm` directly. The `(- 1.0).max(10.0)` is an unstructured size adjustment.
  - Suggested token: `theme.tokens.typography.scale.xs` (= 10) is the canonical fit. Alternatively, a new `scale.x2s` (= 11 px).

- **L397 / L541 / L753** — `let idle_fill = Color::rgba8(255, 255, 255, 13);` / `let chip_body = Color::rgba8(255, 255, 255, 13);` / `let chip_body = Color::rgba8(255, 255, 255, 13);`
  - Issue: re-literal of `theme.tokens.colors.surface_white_5` (alpha 13 = 5 %). **Existing token is already a 1:1 match** — these three call sites should just read `theme.tokens.colors.surface_white_5`. Pure missed-opportunity violations.
  - Suggested token: `theme.tokens.colors.surface_white_5` (already exists).

- **L433** — `let ft_active_fill = Color::rgba8(34, 197, 94, 51);` *(emerald-500/20 for full-text toggle)*
  - Issue: hardcoded literal palette colour (Tailwind `emerald-500`) at 20 % alpha. No `success` accent fill token at 20 % alpha exists.
  - Suggested token: `theme.tokens.colors.semantic.success_soft` (new — `semantic.success` at alpha 51) or `success.with_alpha_factor(0.20)` once a helper exists.

- **L434** — `let ft_active_text = Color::rgba8(0x4a, 0xde, 0x80, 0xff);` *(emerald-400 — full-text toggle "on" text colour)*
  - Issue: hardcoded literal `emerald-400`. The `accent` ramp could host this, or a `semantic.success_text` token.
  - Suggested token: `theme.tokens.colors.semantic.success_text` (new). Comment even says it's pinned by "the plan's palette table" — moving to a token would centralise that pin.

- **L519** — `let band_body = Color::rgba8(0, 0, 0, 51);` *(footer `bg-black/20`)*
  - Issue: hardcoded black at 20 % alpha for the footer band.
  - Suggested token: `theme.tokens.colors.surfaces.overlay_dim` (new) — sits between current `surfaces.overlay` (alpha 204) and a hypothetical "subtle dim" tier.

- **L542 / L754** — `let chip_border = Color::rgba8(255, 255, 255, 26);` (×2)
  - Issue: literal of `border_white_10` (alpha 26). The token already exists.
  - Suggested token: `theme.tokens.colors.border_white_10` (already exists).

- **L554** — `let chip_shape = RoundedRect::from_rect(chip_rect, 4.0);` *(kbd chip radius)*
  - Issue: hardcoded radius `4.0`. Mechanical audit misses constructor form. `theme.tokens.radii.sm` = 8.0 (too big); `radii.xs` = 4.0 — exact match. Likely just an oversight.
  - Suggested token: `theme.tokens.radii.xs` (exists, = 4.0).

- **L750** — `let chip_shape = RoundedRect::from_rect(layout.kbd_chip_rect, 6.0);` *(trailing ⌘K chip)*
  - Issue: hardcoded radius `6.0`. No existing token equals 6.0 — `radii.xs` = 4.0, `radii.sm` = 8.0. Concept uses `rounded-md` (= 6 in Tailwind v3 default). Suggests a new mid-tier radius.
  - Suggested token: `theme.tokens.radii.md_small` (new — 6 px to match Tailwind `rounded-md`).

### widget/permission_prompt.rs

- **L219** — `bg = bg.with_alpha_factor(0.4);` *(disabled button background)*
- **L224** — `theme.tokens.colors.border_standard.with_alpha_factor(0.4)` *(disabled button border)*
- **L232** — `theme.tokens.colors.text_primary.with_alpha_factor(0.4)` *(disabled button text)*
  - Issue: hardcoded `0.4` alpha factor used three times for "disabled" affordance. Brief says "40 % alpha" — should be a single token shared across L219/L224/L232.
  - Suggested token: `theme.tokens.colors.disabled_alpha` (new f32) or a higher-level `theme.tokens.colors.disabled_factor`.

- **L237** — `Color::WHITE` *(primary-action label text on accent)*
  - Issue: same `text_on_accent` need as checkbox.rs L119. Comment explicitly references the concept's `text-white` on `bg-indigo-500`.
  - Suggested token: `theme.tokens.colors.text_on_accent`.

### widget/file_picker.rs

- **L495** — `let label_color = if is_selected { Color::WHITE } else { theme.tokens.colors.text_primary };`
  - Issue: WHITE-on-accent for selected file row labels.
  - Suggested token: `theme.tokens.colors.text_on_accent`.

- **L651** — `let text_color = if is_primary { Color::WHITE } else { theme.tokens.colors.text_primary };`
  - Issue: WHITE for primary-button text — same `text_on_accent` pattern.
  - Suggested token: `theme.tokens.colors.text_on_accent`.
  - **Light-theme axis:** if the user re-themes and lightens `accent[5]`, WHITE-on-pale-accent loses contrast; today's `Colors::light()` keeps accent vivid so this is dormant.

### widget/badge.rs

- **L181** — `Color::WHITE` *(badge text label on `semantic.danger` red fill)*
  - Issue: WHITE-on-danger. Mirrors the close-glyph `text_on_danger` need in tabs.rs/window_group.rs.
  - Suggested token: `theme.tokens.colors.text_on_danger` (new — see proposed tokens). Or reuse `text_on_accent` if the OS decides white-on-any-strong-colour is one token; but the two cases (accent vs danger) might want independent levers.

## Proposed new tokens

These would unblock the per-widget fix units. Several are repeated
across many widgets, so one token addition collapses tens of literal
replacements.

- `text_on_accent: Color` — white-on-accent button labels & checkmark.
  Currently `Color::WHITE` in checkbox.rs L119, permission_prompt.rs L237,
  file_picker.rs L495 + L651. Light theme keeps `accent[5]` vivid so the
  current literal works dormantly, but a custom theme that lightens the
  accent would lose contrast.

- `text_on_danger: Color` — white-on-red-fill close glyph + badge text.
  Used by tabs.rs L597 + L1082, window_group.rs L572 + L1002, badge.rs L181.
  Same "white over destructive fill" pattern as `text_on_accent` but the
  semantic intent is different (destructive vs. primary action), so a
  separate token keeps the contract legible.

- `permission_denied_overlay: Color` — the `Color::rgba8(0, 0, 0, 77)`
  alpha-30% black overlay painted by every focusable widget when the
  permission check fails. Eleven call sites (button, checkbox, column,
  container, progress_bar, row, slider, spacer, stack, text, text_input,
  toggle). text_input already extracted to a `pub(crate) const` with a
  pinning test — same value should move up to Tokens.

- `focus_ring_px: f32` — focus-state stroke width. Today every focusable
  widget hardcodes `2.0` (button.rs L107, checkbox.rs L107, toggle.rs L101,
  text_input.rs L132, slider.rs (via `border_width`), window_group.rs L835).

- `icon_stroke_px: f32` — stroke width for vector glyph painting. Today
  `1.5` (window_group.rs L781, command_palette.rs lucide icons, file_picker)
  and `1.1` (tabs.rs L1805 app-icon glyphs). May need to be a pair
  (`stroke_md`/`stroke_sm`) — see audit notes on the 1.1 vs 1.5 split.

- `radii.x3l: f32 = 24.0` — concept's `rounded-3xl` for the command-palette
  card. Used by command_palette.rs L294. Currently no exact match in
  `Radii` (`x2l` is 32 today).

- `radii.md_small: f32 = 6.0` — concept's `rounded-md`. Used by
  command_palette.rs L750 (kbd-chip), and likely needed by future widgets
  that want the Tailwind `rounded-md` size between `radii.xs` (4) and
  `radii.sm` (8).

- `elevation.pill_rings: [(f32, u8); 4]` — the shadow-ring spread+alpha
  table used twice in tabs.rs (L420-424, L685-689) to approximate
  `shadow-[0_8px_32px_rgba(0,0,0,0.2)]` without a blur primitive.
  Duplicated literal → token candidate.

- `elevation.pill_offset_y: f32 = 6.0` — the `shadow_y_offset` constant
  used in the same two tabs.rs draw paths.

- `elevation.ambient_shadow: { offset_y: f32, pad: f32, alpha: f32 }` —
  the `shadow-2xl` approximation in tile_host.rs L376-389. Currently
  `ambient_y=25.0`, `ambient_pad=50.0`, `alpha=0.25`.

- `elevation.indigo_bloom: { pad: f32, alpha: f32 }` — focused-tile
  indigo bloom in tile_host.rs L397-409. Currently `bloom_pad=120.0`,
  `alpha=0.6`.

- `elevation.search_ring: { outer_px: f32, inner_px: f32, alphas: [f32; 3] }`
  — search-active ring chrome in window_group.rs L803/L816 (8 px outer,
  4 px inner) + L781/L805/L818 ring opacities (0.6/0.24/0.5).

- `elevation.dropzone: { idle_alpha: f32, active_alpha: f32, glow_alpha: f32 }`
  — drop-zone indicator alphas in tabs.rs L1462/L1482 and group_row.rs L730.

- `colors.disabled_alpha: f32 = 0.4` — disabled-state alpha multiplier
  used three times in permission_prompt.rs (L219/L224/L232) for the
  `Always` button gating non-owners.

- `colors.surface_white_15: Color` (alpha 38 ≈ 15 %) — used by
  window_group.rs L559/L883/L983 (maximized chrome pills). Sits between
  existing `surface_white_10` (26) and `surface_white_20` (51).

- `colors.surface_white_25: Color` (alpha 64 ≈ 25 %) — used by tabs.rs
  L610 (hover-reveal action chip background) and window_group.rs L919
  hover branch (alpha 64). One more tier in the white-alpha ladder.

- `colors.surfaces.overlay_dim: Color` (alpha 51 ≈ 20 % black) — used by
  command_palette.rs L519 footer band. Distinct from `overlay` (alpha
  204) which is for top-bar darkening; this is the lighter dim used for
  internal panels.

- `colors.semantic.success_soft: Color` — `semantic.success` at ~20 %
  alpha (51). Used by command_palette.rs L433 (full-text toggle "on" fill).

- `colors.semantic.success_text: Color` — emerald-400 for "on" status
  text. Used by command_palette.rs L434.

- `typography.scale.x2s: f32 = 11.0` *(or rework the existing scale to
  include an `xs2`)* — the `sm - 1.0` adjustment in
  command_palette.rs L394 + L540 + L770 suggests a missing tier between
  `scale.xs` (= 10) and `scale.sm` (= 12).

## Recap

- **Total flagged: ~70** (counting each (file, line, issue) tuple once;
  cross-cutting overlays like `rgba8(0,0,0,77)` counted as 11 separate
  call sites for the per-fix unit accounting).
- **Highest-density files** (most flagged literals):
  1. `widget/tabs.rs` — ~15 flags (shadow rings ×2, presence dots, close-
     chip whites, drop-zone alphas, icon strokes).
  2. `widget/window_group.rs` — ~12 flags (maximized chrome, search ring
     trio, ring strokes, close-hover white).
  3. `widget/command_palette.rs` — ~11 flags (chip whites already
     redundant with existing `surface_white_5`/`border_white_10` tokens,
     surface radius, emerald success colours, footer band).
- **Proposed new tokens: 18** (see "Proposed new tokens" above). The
  three highest-leverage adds are:
  1. `permission_denied_overlay` — collapses 11 literal sites at once.
  2. `text_on_accent` + `text_on_danger` — fixes the light-theme-
     dormant `Color::WHITE` literals in five files.
  3. `elevation.pill_rings` + `pill_offset_y` — collapses the duplicated
     shadow-table literal in tabs.rs.
- **Surprising findings:**
  1. `command_palette.rs` re-literals `surface_white_5` (alpha 13) and
     `border_white_10` (alpha 26) at four sites — the tokens already
     exist and exactly match. Pure missed-opportunity.
  2. `text_input.rs` already promoted the permission-denied overlay to
     a `pub(crate) const PERMISSION_DENIED_OVERLAY` and pinned it with
     a test (`permission_denied_overlay_alpha_matches_other_widgets`).
     This is the cleanest precedent for moving the literal up to
     `Tokens` — every other widget should reference the new token, and
     the const + test can stay as a defensive cross-check during the
     migration.
  3. `text_input.rs` has a real correctness bug: `draw` uses
     `theme.padding_standard` for `padding_x` (L135), but `handle_event`
     uses `let padding_x = 10.0` (L219). The click-to-place-caret
     math drifts from the draw geometry whenever `padding_standard` is
     overridden. Flagging here, but it deserves its own fix unit
     beyond the audit scope.
  4. Light-theme risk is genuinely small today: `Colors::light()` keeps
     the accent ramp vivid enough that `Color::WHITE`-on-accent reads
     fine. The breakage axis is "custom themes / future palette swap",
     not "switching the bundled dark→light theme."
