# X1 — text scaled once, centred on the measured line (round two)

Unit X1 of the 2026-09-17 UI alignment review. Branch `ui-review/r2-x1-text-scale-centring`,
cut from `381eff6a8` (merged tip of round one). Scope: the `ui` crate only —
`ui/src/widget/**`, `ui/src/chrome/**`, `ui/src/presence/**`, `ui/src/status_bubble.rs`,
`ui/src/lib.rs`, one new module and one new integration test. `apps/`, `shell/`,
`ui/src/text_render.rs` and `ui/src/history_mode/*` were not touched.

## The two defects, fixed once

Round one's systemic findings 1 and 2 (see `../2026-09-16-ui-alignment-review/README.md`):

1. **Double text scaling.** `Theme::with_text_scale` multiplies every typography token, and
   the legacy renderer-relative `TextRenderer` APIs (`draw_text`, `draw_text_weighted`,
   `measure`, `measure_weighted`, `elide_to_width*`, `measure_styled`, …) multiply by the
   renderer's text scale again. Any widget that fed `theme.tokens.typography.scale.*` into
   them painted 4× glyphs at 200 %.
2. **Font-size centring.** `y0 + (band − font_size) / 2` sits a line
   `(line_height − 1) × size / 2` too low (1.5 px at 12 px, 3 px at 200 %).

### `ui/src/text_metrics.rs` (new; `pub mod text_metrics;` in `ui/src/lib.rs`)

| helper | contract |
|---|---|
| `renderer_type_size(text_renderer, resolved) -> f32` | Size-only twin of `TextRenderer::renderer_input_style`: divides once by the **renderer's** sanitised text scale. Renderer, not theme: in a matched host the two agree; in an unmatched host (theme scaled, shared renderer left at 1× — the apps' `--shots` loop, F2-05/F3-18) the renderer is what multiplies back, so glyphs land at the theme's resolved size either way. **No floor** — the type scale's floor (`scale.xs`) is the policy; `modal_frame.rs`'s `.max(12.0)`/`.max(14.0)` were no-ops at every text scale ≥ 1 and silently overrode the token below it, so they are gone (identical results at 0.5×, confirmed in review). |
| `centered_text_top(band_y0, band_height, measured_line_height) -> f64` | F2's rule, moved here verbatim: unclamped, so a line taller than its band overhangs equally. `widget::data_table::centered_text_top` (F3's `f32` variant) is now a thin wrapper that clamps to `band_y0` — the row-stacking policy a table needs — so both behaviours share one home for the arithmetic. |
| `line_height_px` | Deliberately **not** added: every caller in `ui` has a renderer in hand and the `.1` of `measure*` is the line box actually painted. The one renderer-less copy in the tree is `ui/src/math/layout.rs::line_h` (size × 1.25), owned with `text_render`. |

Old paths keep working: `widget::text::centered_text_top` is a `pub use`, and
`widget::data_table::renderer_type_size` a `pub(crate) use`. The two remaining local
"divide by `theme.text_scale`" copies (`chrome/status_bar.rs::label_size`,
`widget/page_sidebar.rs::label_size`) and `identity_flow/sheet.rs::Role::size` now take the
renderer and call the helper; `tabs.rs`'s three `/ tr.text_scale()` copies do too.

### Method

`rg -n 'typography\.scale\.' ui/src` minus the style-path bridges (`renderer_input_style`,
`renderer_type_size`, `set_text_scale(1.0)` brackets, `QueryComposition::draw` /
`NameField::draw`, which shape via `font_system` and never see the renderer scale), traced
per binding into the API it feeds; a parallel read-only audit produced the per-line table for
the six largest files. Every migrated line is either a `let x = renderer_type_size(<tr>,
<theme size>)` binding or an in-place wrap of an inline argument, applied by a line-anchored
script (`target/ui-review/wrap.pl`) that refused any line it could not classify. Sites where
the theme size also drives **geometry** (tile_host's name offset, tooltip's chip box,
toast's card, permission_prompt's block, avatar/chip label ratios, status pill box) were done
by hand so the geometry stays in theme space and only the renderer argument is divided.

## Migrated sites

186 call sites in 41 `ui/src` files now go through the two helpers. Pattern (a) =
renderer-relative size, (b) = measured-line centring. Files whose rows were already on F3/F5
helpers (`data_table.rs`; `capture_studio.rs`'s local `centered_text_top(tr, size, weight,
cy)`) are listed because their **inputs** changed: the sizes they receive are now divided once.

Hand-fixed (b) sites: `status_bubble.rs:161` (pill label), `modal_dialog.rs:543-544` (title in
the 48 px bar), `command_palette.rs:1241-1246` (argument heading, inside the palette's 1×
renderer bracket), `permission_prompt.rs:415-421` (two-line block centred on measured heights,
second line at `+ app_id_h + 4`), `toast.rs:732-750` (caption/title/body block stacked and
centred on measured line boxes — 1× layout inside the fixed 48 px card moves by ≤ 1.5 px),
`tile_host.rs:1799-1802,1822` (card title on its measured line, detail advanced by the
measured title height), `activity_timeline.rs:2074-2075` (group-row line two at
`6 + measured title line + 6`, the 27 px literal it replaces at 1×).

| file | line | expression |
|---|---:|---|
| `ui/src/widget/tooltip.rs` | 70 | `let (tw, _th) = text_renderer.measure(label, renderer_type_size(text_renderer, text_size), None);` |
| `ui/src/widget/tooltip.rs` | 152 | `let text_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/video_player.rs` | 1082 | `let time_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/video_player.rs` | 1591 | `let title_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/video_player.rs` | 1678 | `let label_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/video_player.rs` | 1780 | `let label_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/popover.rs` | 258 | `let fs = renderer_type_size(tr, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/popover.rs` | 391 | `let fs = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/screenshot_markup.rs` | 701 | `let title_size = renderer_type_size(tr, theme.tokens.typography.scale.base);` |
| `ui/src/widget/screenshot_markup.rs` | 702 | `let caption_size = renderer_type_size(tr, theme.tokens.typography.scale.xs);` |
| `ui/src/widget/screenshot_markup.rs` | 866 | `renderer_type_size(tr, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/screenshot_markup.rs` | 873 | `renderer_type_size(tr, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/screenshot_markup.rs` | 880 | `renderer_type_size(tr, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/screenshot_markup.rs` | 1001 | `renderer_type_size(tr, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/screenshot_markup.rs` | 1008 | `renderer_type_size(tr, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/screenshot_markup.rs` | 1052 | `let tip_size = renderer_type_size(tr, theme.tokens.typography.scale.xs);` |
| `ui/src/widget/window_group.rs` | 1255 | `let title_size = renderer_type_size(tr, theme.tokens.typography.scale.xs);` |
| `ui/src/widget/window_group.rs` | 1309 | `let title_size = renderer_type_size(tr, theme.tokens.typography.scale.xs);` |
| `ui/src/widget/native_view.rs` | 457 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.sm),` |
| `ui/src/widget/page_sidebar.rs` | 123 | `renderer_type_size(tr, theme.tokens.typography.scale.sm)` |
| `ui/src/widget/segmented.rs` | 240 | `let text_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/floating_window.rs` | 879 | `let title_size = renderer_type_size(` |
| `ui/src/widget/file_picker.rs` | 1215 | `let title_size = renderer_type_size(text_renderer, tokens.typography.scale.base);` |
| `ui/src/widget/file_picker.rs` | 1252 | `let crumb_size = renderer_type_size(text_renderer, tokens.typography.scale.sm);` |
| `ui/src/widget/file_picker.rs` | 1294 | `let row_size = renderer_type_size(text_renderer, tokens.typography.scale.sm);` |
| `ui/src/widget/file_picker.rs` | 1376 | `let row_size = renderer_type_size(text_renderer, tokens.typography.scale.sm);` |
| `ui/src/widget/file_picker.rs` | 1776 | `.text_size(renderer_type_size(text_renderer, theme.tokens.typography.scale.sm))` |
| `ui/src/widget/command_palette.rs` | 1242 | `let text_y = centered_text_top(` |
| `ui/src/widget/data_table.rs` | 124 | `let size = renderer_type_size(text, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/data_table.rs` | 137 | `centered_text_top(rect.y0, rect.height(), text_h),` |
| `ui/src/widget/data_table.rs` | 1222 | `let text_size = renderer_type_size(text_renderer, tokens.typography.scale.sm);` |
| `ui/src/widget/data_table.rs` | 1230 | `let label_y = centered_text_top(header_rect.y0, HEADER_HEIGHT, header_h);` |
| `ui/src/widget/data_table.rs` | 1254 | `centered_text_top(header_rect.y0, HEADER_HEIGHT, arrow_h),` |
| `ui/src/widget/data_table.rs` | 1335 | `let size = renderer_type_size(text_renderer, tokens.typography.scale.sm);` |
| `ui/src/widget/data_table.rs` | 1345 | `centered_text_top(y0, row_h, text_h),` |
| `ui/src/widget/data_table.rs` | 1715 | `crate::text_metrics::centered_text_top(band_y0, band_h, f64::from(text_h)).max(band_y0)` |
| `ui/src/widget/autocomplete.rs` | 486 | `let text_size = renderer_type_size(text_renderer, tokens.typography.scale.base);` |
| `ui/src/widget/autocomplete.rs` | 487 | `let detail_size = renderer_type_size(text_renderer, tokens.typography.scale.sm);` |
| `ui/src/widget/autocomplete.rs` | 688 | `let text_size = renderer_type_size(text_renderer, tokens.typography.scale.base);` |
| `ui/src/widget/gauge.rs` | 197 | `let size = renderer_type_size(text_renderer, tokens.typography.scale.xl);` |
| `ui/src/widget/gauge.rs` | 211 | `let ss = renderer_type_size(text_renderer, tokens.typography.scale.xs);` |
| `ui/src/widget/gauge.rs` | 425 | `renderer_type_size(text_renderer, tokens.typography.scale.sm),` |
| `ui/src/widget/gauge.rs` | 433 | `let (vw, _) = text_renderer.measure(value, renderer_type_size(text_renderer, tokens.typography.scale.sm), None);` |
| `ui/src/widget/gauge.rs` | 437 | `renderer_type_size(text_renderer, tokens.typography.scale.sm),` |
| `ui/src/widget/activity_timeline.rs` | 1614 | `let size = renderer_type_size(tr, (theme.tokens.typography.scale.sm - 1.0).max(10.0));` |
| `ui/src/widget/activity_timeline.rs` | 1670 | `let text_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/activity_timeline.rs` | 2063 | `let title_size = renderer_type_size(tr, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/activity_timeline.rs` | 2368 | `let chip_text_size = renderer_type_size(text_renderer, (theme.tokens.typography.scale.sm - 1.0).max(10.0));` |
| `ui/src/widget/activity_timeline.rs` | 2581 | `let size = renderer_type_size(tr, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/activity_timeline.rs` | 3020 | `let size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/presence/interaction.rs` | 310 | `let size = renderer_type_size(tr, theme.tokens.typography.scale.sm);` |
| `ui/src/presence/render.rs` | 423 | `let size = renderer_type_size(text_renderer, pill_label_size(theme));` |
| `ui/src/presence/render.rs` | 716 | `let label_size = renderer_type_size(text_renderer, pill_label_size(theme));` |
| `ui/src/presence/render.rs` | 850 | `let label_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.xs);` |
| `ui/src/presence/render.rs` | 881 | `let label_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.xs);` |
| `ui/src/presence/render.rs` | 1017 | `let label_size = renderer_type_size(text_renderer, tokens.typography.scale.xs);` |
| `ui/src/widget/avatar.rs` | 147 | `let text_size = renderer_type_size(text_renderer, px(rect.width().min(rect.height())) * 0.4);` |
| `ui/src/widget/avatar.rs` | 247 | `let text_size = renderer_type_size(text_renderer, px(rect.width().min(rect.height())) * 0.36);` |
| `ui/src/widget/avatar.rs` | 332 | `let text_size = renderer_type_size(text_renderer, self.size * 0.4);` |
| `ui/src/widget/history_scrubber.rs` | 777 | `let size = renderer_type_size(tr, theme.tokens.typography.scale.xs);` |
| `ui/src/widget/history_scrubber.rs` | 909 | `let size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/history_scrubber.rs` | 937 | `let size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/history_scrubber.rs` | 1071 | `let head_size = renderer_type_size(text_renderer, (theme.tokens.typography.scale.sm - 1.0).max(10.0));` |
| `ui/src/widget/history_scrubber.rs` | 1073 | `let sub_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.xs);` |
| `ui/src/widget/history_scrubber.rs` | 1108 | `let size = renderer_type_size(text_renderer, (theme.tokens.typography.scale.sm - 1.0).max(10.0));` |
| `ui/src/widget/history_scrubber.rs` | 1149 | `let hint_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.xs);` |
| `ui/src/widget/date_picker.rs` | 539 | `let text_size = renderer_type_size(text_renderer, tokens.typography.scale.sm);` |
| `ui/src/widget/date_picker.rs` | 596 | `let text_size = renderer_type_size(text_renderer, tokens.typography.scale.sm);` |
| `ui/src/widget/date_picker.rs` | 626 | `let text_size = renderer_type_size(text_renderer, tokens.typography.scale.sm);` |
| `ui/src/widget/badge.rs` | 135 | `let size = renderer_type_size(text_renderer, theme.tokens.typography.scale.xs);` |
| `ui/src/widget/badge.rs` | 275 | `let text_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.xs);` |
| `ui/src/status_bubble.rs` | 167 | `let ty = centered_text_top(rect.y0, rect.height(), f64::from(th));` |
| `ui/src/widget/shortcut_recorder.rs` | 169 | `let (w, h) = text.measure(label, renderer_type_size(text, theme.tokens.typography.scale.sm), None);` |
| `ui/src/widget/shortcut_recorder.rs` | 173 | `renderer_type_size(text, theme.tokens.typography.scale.sm),` |
| `ui/src/widget/shortcut_recorder.rs` | 206 | `renderer_type_size(text, theme.tokens.typography.scale.sm),` |
| `ui/src/widget/shortcut_recorder.rs` | 225 | `let (w, h) = text.measure("↶", renderer_type_size(text, theme.tokens.typography.scale.base), None);` |
| `ui/src/widget/shortcut_recorder.rs` | 229 | `renderer_type_size(text, theme.tokens.typography.scale.base),` |
| `ui/src/widget/breadcrumb.rs` | 153 | `let size = renderer_type_size(tr, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/breadcrumb.rs` | 191 | `let size = renderer_type_size(tr, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/breadcrumb.rs` | 270 | `let size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/identity_flow/sheet.rs` | 227 | `renderer_type_size(text_renderer, token)` |
| `ui/src/widget/slash_menu.rs` | 532 | `let font_size = renderer_type_size(text_renderer, tokens.typography.scale.sm);` |
| `ui/src/widget/tile_host.rs` | 1751 | `let size = renderer_type_size(tr, resolved);` |
| `ui/src/widget/tile_host.rs` | 1799 | `let text_size = renderer_type_size(tr, theme.tokens.typography.scale.xl);` |
| `ui/src/widget/tile_host.rs` | 1802 | `let y = centered_text_top(rect.y0, rect.height(), f64::from(th));` |
| `ui/src/widget/tile_host.rs` | 1814 | `let sm = renderer_type_size(tr, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/link_popover.rs` | 420 | `let font_size = renderer_type_size(text_renderer, tokens.typography.scale.sm);` |
| `ui/src/widget/input_hint.rs` | 528 | `let size = renderer_type_size(text_renderer, theme.tokens.typography.scale.xs);` |
| `ui/src/widget/input_hint.rs` | 640 | `renderer_type_size(` |
| `ui/src/widget/time_picker.rs` | 501 | `let text_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.lg);` |
| `ui/src/widget/channel_browser.rs` | 412 | `let pill_text_size = renderer_type_size(text_renderer, (theme.tokens.typography.scale.sm - 1.0).max(10.0));` |
| `ui/src/widget/channel_browser.rs` | 526 | `let text_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/channel_browser.rs` | 575 | `let text_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/channel_browser.rs` | 629 | `let text_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/channel_browser.rs` | 713 | `let text_size = renderer_type_size(text_renderer, (theme.tokens.typography.scale.sm - 1.0).max(10.0));` |
| `ui/src/widget/channel_browser.rs` | 753 | `let chip_text_size = renderer_type_size(text_renderer, (theme.tokens.typography.scale.sm - 2.0).max(9.0));` |
| `ui/src/widget/channel_browser.rs` | 1126 | `let size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/color_picker.rs` | 789 | `let text_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/modal_frame.rs` | 362 | `let body_size = renderer_type_size(text_renderer, tokens.typography.scale.sm);` |
| `ui/src/widget/modal_frame.rs` | 505 | `let size = renderer_type_size(text_renderer, tokens.typography.scale.base);` |
| `ui/src/widget/modal_frame.rs` | 560 | `let body_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/chrome/status_bar.rs` | 27 | `renderer_type_size(tr, theme.tokens.typography.scale.sm)` |
| `ui/src/widget/chip.rs` | 150 | `let (w, _) = text_renderer.measure(&self.label, renderer_type_size(text_renderer, theme.tokens.typography.scale.sm), None);` |
| `ui/src/widget/chip.rs` | 288 | `let size = renderer_type_size(text_renderer, Self::label_size_for_height(height, theme));` |
| `ui/src/widget/chip.rs` | 398 | `let size = renderer_type_size(text_renderer, Self::label_size_for_height(rect.height(), theme));` |
| `ui/src/widget/chip.rs` | 558 | `let size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/capture_studio.rs` | 1702 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 1740 | `let detail_top = centered_text_top(` |
| `ui/src/widget/capture_studio.rs` | 1742 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 1749 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 1756 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 2399 | `renderer_type_size(tr, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 2437 | `let label_top = centered_text_top(` |
| `ui/src/widget/capture_studio.rs` | 2439 | `renderer_type_size(tr, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 2446 | `renderer_type_size(tr, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 2453 | `renderer_type_size(tr, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 2597 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.lg),` |
| `ui/src/widget/capture_studio.rs` | 2621 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 2631 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.sm),` |
| `ui/src/widget/capture_studio.rs` | 2730 | `let font_size = renderer_type_size(tr, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/capture_studio.rs` | 2863 | `let count_top = centered_text_top(` |
| `ui/src/widget/capture_studio.rs` | 2865 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.lg),` |
| `ui/src/widget/capture_studio.rs` | 2872 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.lg),` |
| `ui/src/widget/capture_studio.rs` | 3088 | `let starting_top = centered_text_top(` |
| `ui/src/widget/capture_studio.rs` | 3090 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.sm),` |
| `ui/src/widget/capture_studio.rs` | 3097 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.sm),` |
| `ui/src/widget/capture_studio.rs` | 3104 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.sm),` |
| `ui/src/widget/capture_studio.rs` | 3335 | `let intro_y = centered_text_top(` |
| `ui/src/widget/capture_studio.rs` | 3337 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.sm),` |
| `ui/src/widget/capture_studio.rs` | 3341 | `let settled_y = centered_text_top(` |
| `ui/src/widget/capture_studio.rs` | 3343 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 3362 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.sm),` |
| `ui/src/widget/capture_studio.rs` | 3369 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.sm),` |
| `ui/src/widget/capture_studio.rs` | 3401 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 3408 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 3421 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 3428 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 3527 | `let stop_top = centered_text_top(` |
| `ui/src/widget/capture_studio.rs` | 3529 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 3536 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 3644 | `let saved_top = centered_text_top(` |
| `ui/src/widget/capture_studio.rs` | 3646 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 3653 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 3660 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 3861 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 3873 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 3880 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 3890 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 3897 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 4080 | `let replay_top = centered_text_top(` |
| `ui/src/widget/capture_studio.rs` | 4082 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 4089 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 4096 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 4168 | `let save_top = centered_text_top(` |
| `ui/src/widget/capture_studio.rs` | 4170 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 4177 | `renderer_type_size(text_renderer, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 4471 | `let badge_top = centered_text_top(` |
| `ui/src/widget/capture_studio.rs` | 4473 | `renderer_type_size(tr, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 4484 | `renderer_type_size(tr, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 4507 | `renderer_type_size(tr, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 4723 | `let title_top = centered_text_top(` |
| `ui/src/widget/capture_studio.rs` | 4725 | `renderer_type_size(tr, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 4732 | `renderer_type_size(tr, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 4739 | `renderer_type_size(tr, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 4753 | `renderer_type_size(tr, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 4771 | `let context_top = centered_text_top(` |
| `ui/src/widget/capture_studio.rs` | 4773 | `renderer_type_size(tr, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 4780 | `renderer_type_size(tr, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 4962 | `let label_top = centered_text_top(` |
| `ui/src/widget/capture_studio.rs` | 4964 | `renderer_type_size(tr, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 4971 | `renderer_type_size(tr, theme.tokens.typography.scale.xs),` |
| `ui/src/widget/capture_studio.rs` | 6427 | `let top = centered_text_top(&mut tr, size, TextWeight::Semibold, center);` |
| `ui/src/widget/toast.rs` | 623 | `let title_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/toast.rs` | 717 | `let caption_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.xs);` |
| `ui/src/widget/toast.rs` | 718 | `let title_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/toast.rs` | 719 | `let body_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.xs);` |
| `ui/src/widget/permission_prompt.rs` | 315 | `let text_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/permission_prompt.rs` | 411 | `let app_id_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/permission_prompt.rs` | 412 | `let cap_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.base);` |
| `ui/src/widget/permission_prompt.rs` | 417 | `let label_y = centered_text_top(` |
| `ui/src/widget/modal_dialog.rs` | 398 | `let text_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/modal_dialog.rs` | 540 | `let title_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.lg);` |
| `ui/src/widget/modal_dialog.rs` | 544 | `let title_y = centered_text_top(self.rect.y0, TITLE_BAR_HEIGHT, f64::from(title_h));` |
| `ui/src/widget/tabs.rs` | 551 | `let text_size = renderer_type_size(tr, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/tabs.rs` | 909 | `let text_size = renderer_type_size(tr, theme.tokens.typography.scale.sm);` |
| `ui/src/widget/tabs.rs` | 1194 | `let size = renderer_type_size(tr, theme.tokens.typography.scale.sm);` |

## Sites left as they are, and why

| site | why it is correct as-is |
|---|---|
| `command_palette.rs` — every `typography.scale.*` size (1233, 1307, 1436, 1528-1530, 1858, 1870) | `CommandPalette::draw` brackets its whole paint with `text_renderer.set_text_scale(1.0)` (2040-2110), so theme sizes reach the legacy APIs already single-scaled. Only the heading's font-size centring (1235) was a defect; fixed. The `(scale.sm - 1.0).max(10.0)` / `.max(9.0)` off-scale sizes are F4's. |
| `page_palette.rs:366 row_size = 14.0`, `dropdown.rs FONT`, `reader_hud.rs 13.0`, `chrome/toolbar.rs 15.0`, `nav_shell.rs 14.0`, `table.rs 14.0`, `link.rs 14.0` default, `modal_prompt.rs CAPTION_SIZE/LABEL_SIZE`, `number_input.rs FONT_SIZE/STEP_GLYPH_SIZE`, `search_field.rs KBD_LABEL_SIZE`, `toggle_group.rs` face `text_size: 13.0`, `editable_label.rs self.size`, `block_editor.rs kind.font_size()` | Literal or caller-supplied design sizes: the legacy renderer contract (scaled once by the renderer). Not double-scaled; re-tokenising them is the F units' business. |
| `slash_menu.rs:523`, `link_popover.rs:399`, `channel_browser.rs:385-394`, `history_scrubber.rs:1142`, `activity_timeline.rs:2214,2620` — `TextStyle::new(scale.sm)` / `size` into `QueryComposition::draw` or `NameField::draw` | Style path: both shape through `RichTextLayout::shape_plain` on `font_system` and never apply the renderer scale. |
| `context_menu.rs:163,519`, `toggle_group.rs:710`, `toolbar.rs:174,619`, `empty_state.rs`, `button.rs`, `checkbox.rs`, `text_input.rs`, `rich_text_line.rs`, `code_editor.rs`, `nav_shell.rs`, `number_input.rs` | Already bridged via `renderer_input_style` (F1/F2/F4 work), or retained controls with their own resolved-size path. |
| F3's ten (`chart`, `tree_view`, `search_results`, `banner`, `stat_tile`, `icon_grid`, `conflict_*`, `qr`, `data_table`) | Already on `renderer_type_size`; their import now resolves through `data_table`'s re-export of the shared module (no line changed). |
| Theme sizes used as **geometry** (`f64::from(scale.*)`): `stat_tile.rs:78-81,125-126`, `banner.rs:301,304`, `breadcrumb.rs:231` (`sm * 1.5` row height), `floating_window.rs:815` (glyph-width estimate), `widget/toolbar.rs:145`, `tile_host.rs:1755` (name offset, now explicitly `resolved`) | Theme space is right for layout: these must grow with the text scale, and they never reach a renderer call. |
| `avatar.rs` initials `min(w,h) × 0.4` / `self.size × 0.4` | Geometry-derived, **now divided**: the initials fill the disc the app laid out, so they stay put when only the text scale changes (they used to grow out of the disc). The x1 test pins ratio 1. |
| `chip.rs::label_size_for_height` | Semantics kept (theme-scaled `scale.sm × height/28`): `shell/src/shell/sidebar_wheel_disc.rs:4176` pins the 28 px floor to `scale.sm`, and the wheel disc steps chip heights itself. Divided once at the three renderer call sites instead. See open items for the 2× compounding. |
| `status_bubble.rs::pill_size/draw_pill` `text_size` | **Renderer-relative by contract**: 11 of 13 app callers pass design sizes (`12.0`, `11.5`, `12.5`), sheets passes `input(scale.sm)`. A first attempt to divide inside the helper regressed store/notes/backups at 2× (their pills stopped following the text scale) and was reverted; the contract is now documented on both functions. The two callers that pass a raw theme size are app bugs (below). |
| `tooltip.rs::chip_size` box `text_size + 2·pad`, `status_bubble.rs::pill_size` box `text_size + 16` | Font-size boxes (F4-20 class): the box policy is F4's; only the renderer argument was divided (tooltip) or left renderer-relative (pill). |
| `video_player.rs:1152 fs_size * 0.5`, `shortcut_recorder.rs:64 RESET_SIZE / 2`, `command_palette.rs:1654 size / 2` | Icon boxes centred by their own size — not text. |
| `text.rs:58,72`, `breadcrumb.rs:492`, `date_picker.rs:749`, `capture_studio.rs:6421-6423`, `activity_timeline.rs:4829,5097`, `toggle_group.rs:1092` | `#[cfg(test)]` code asserting on the production path; left as the tests they are (all green). |

## App-side sites noticed (for the A units)

| app | file:line | what | owner |
|---|---|---|---|
| file-explorer | `apps/file-explorer/src/files_app.rs:1326-1327,1362` | passes `theme.tokens.typography.scale.sm` into the renderer-relative `ui::status_bubble::pill_size/draw_pill` → the "1 Element" pill paints at 4× at 200 % (`target/ui-review/file-explorer/{before,after}/large.png`, bottom right). Pass `renderer_type_size(tr, …)` or a design size like the other 11 callers. | A9 |
| hex_editor | `apps/hex_editor/src/view.rs:1293-1294` | same raw-theme-size call into `pill_size` | A2 |
| settings | `apps/settings/src/shots.rs` (F3-18) | shots path scales the theme but not the shared renderer; sidebar stays 1× in `settings/before/large.png` | A3 |
| video-player, notes | `ui/src/app.rs:1211-1256` Host branch (F5-24) | app-level `--shots` hosts scale theme and renderer; app code still passing raw theme sizes to legacy APIs shows 4× | A8 / A6 |
| chat, containers, logs, monitor (2), terminal, file-explorer (`framework_history.rs`) | `target/ui-review/app_candidates.txt` | heuristic sweep: files that use `typography.scale.*` **and** legacy size APIs **without** any bridge (`renderer_input_style` / `renderer_type_size` / `text_scale`). Candidates, not confirmed defects. | A4, A1, A2, A9 |
| store, notes, backups, pdf_viewer, code_editor, diffmerge, typst_editor, db_client, ebook, sheets | status pill callers | correct (design sizes / `input(scale.sm)`); listed so nobody "fixes" them to raw theme sizes | — |

## Tests

| command | result |
|---|---|
| `cargo check -p ui --tests` (gate) | passes — no errors, no unused-import/dead-code warnings (the `-C extra-filename` sccache line only) |
| `cargo test -p ui --lib -- text_metrics:: chip:: avatar:: badge:: breadcrumb:: capture_studio:: activity_timeline:: channel_browser:: history_scrubber:: screenshot_markup:: video_player:: gauge:: tabs:: modal_frame:: modal_dialog:: status_bar:: page_sidebar:: identity_flow:: presence:: status_bubble:: command_palette:: permission_prompt:: toast:: tooltip:: input_hint:: tile_host:: floating_window:: date_picker:: time_picker:: window_group:: file_picker:: link_popover:: popover:: color_picker:: native_view:: segmented:: autocomplete:: shortcut_recorder:: slash_menu:: data_table:: text:: name_field::` | **976 passed, 0 failed, 1 ignored** (2525 filtered out); the ignore is pre-existing |
| `cargo test -p ui --test it -- x1_ f1_ f2_ f3_ f4_ f5_ f6_ design_token_audit` | **38 passed, 3 failed** — the 3 pre-existing ratchets below, identical at the base; x1 3/3, f1–f6 and the other `design_token_audit` cases green |

New: `ui/src/text_metrics.rs` module tests (4: divide once and round-trip through the
renderer, no floor, unclamped centring, measured-line centring at 1× and 2×);
`ui/tests/x1_text_scale_once.rs` (3, registered in `ui/tests/it.rs`):
`retained_widget_labels_follow_the_text_scale_once_through_the_harness` (Breadcrumb,
ShortcutRecorder expect ~2×, Avatar expects ~1×; pixel ink via `render_window_png` at 1× and
2× with theme and renderer scaled together, PNGs in `target/ui-review/ui/x1_fixture/current/`),
`immediate_mode_painters_encode_glyphs_at_the_text_scale_once` (`Chip::draw_at`,
`status_bubble::draw_pill`, `Avatar::draw_at`: every encoded glyph run's font size at 2× is
exactly the expected multiple of its 1× size), and the helper/`renderer_input_style` identity.

Module tests adapted to the new signatures (same invariant, production path):
`chrome::status_bar::label_size_divides_the_text_scale_back_out`,
`identity_flow::sheet::roles_divide_the_text_scale_back_out_for_the_renderer`,
`presence::render::pill_and_chip_heights_follow_the_measured_label_at_every_text_scale`.

Pre-existing failures, reproduced **verbatim at the merged base** (`target/ui-review/base-src`,
`git archive 381eff6a8`) and left untouched:

```
design_token_audit::theme_escape_hatches_do_not_grow — 5 baseline entries in tests/design_token_baseline.txt are higher than the tree
  (src/widget/{autocomplete,channel_browser,context_menu,popover,video_player}.rs now 0, baseline still 1)
design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow — apps/browser/src/browser_tabs.rs [literal_color 1, white_or_black 1],
  shell/src/shell/frame.rs [literal_color 1, white_or_black 2], baseline 0
f1_spacing_token_audit::spacing_literals_do_not_grow — apps/word-processor/src/zoom_control.rs [pixel_const] now 1 baseline 2,
  shell/src/shell/editor_app.rs [pixel_const] now 3 baseline 4
```

## Evidence (all under `target/ui-review/`)

- Apps, isolated profiles, dark: `<app>/{before,after}/dark/dev.qos.<app>-{phone,tablet,laptop,desktop}.png`
  and `<app>/{before,after}/large.png` (800×900, `--text-scale 2`) for store, settings, notes,
  file-explorer, music. BEFORE binaries built at `381eff6a8`.
- ui-shots: `ui/{before,after}/{quick-ui-widget-showcase,quick-ui-widget-showcase-scrolled}.png`,
  `ui/{before,after}/{gallery_shots,ctrl_k_palette,table_shots,data_table_shots}/*.png`. The
  scenarios ignore their output-directory argument and write to fixed `/tmp/...` paths (shared
  with the other workers); BEFORE was produced from the base copy and copied out immediately.
- Byte comparison (`compare.sh`, final run): 14 of 33 1× pairs identical (all of ctrl_k_palette, settings
  and music, `data_table-bottom`, `table-after-insert`, `table-delete-row`). The 19 that differ, attributed with
  `pngdiff.py` (pure-Python decoder, per-band clustering) and `pngcrop.py` (`crops/`):
  - store / notes / file-explorer 1× matrices: the status pill label rises ~1.5 px onto the
    pill's centre line (`crops/fe-pill-{before,after}.png`) — defect (b) fixed in
    `status_bubble.rs`; the pill box is unchanged.
  - file-explorer 1× top-right band y98..108: the file's modification **timestamp** ("08:35"
    vs the AFTER run's clock) — not layout (`crops/fe-topright-{before,after}.png`).
  - widget showcase 1× (y288-338) and gallery-dark 1× (y649-726): the indeterminate progress
    bar and the spinner, both time-animated (`crops/gallery-bands-{before,after}.png`); the
    remaining 1-6 px in `showcase-scrolled`, `data_table-top`, `table-base`,
    `table-insert-col` have max channel delta 1 — rasteriser noise (`table-delete-row` differed in the first run and was identical in the second, which confirms it).
- 2×: `file-explorer/after/large.png` is the honest picture of the remaining app-side bug
  (the pill is 4× because the app passes a raw theme size); `store/after/large.png` and
  `notes/after/large.png` keep their correctly single-scaled pills; the x1 fixtures
  (`ui/x1_fixture/current/*-2x.png`) show Breadcrumb/ShortcutRecorder labels at 2×, not 4×.

## Review

`code-review` (high) ran on the diff; fixed: retained `Chip::draw`/`intrinsic_width` and
`tile_host` detail line still fed raw theme sizes; `file_picker.rs:1776` footer
`ButtonFace::text_size` (renderer-relative) got a raw token; `line_height_px` had no caller and
duplicated the renderer's clamp — removed; the `activity_timeline` comment over-claimed.
Refuted by verification: command_palette heading size (1× bracket), `TextStyle::new(scale.sm)`
into `QueryComposition::draw` (font_system path), `floating_window.rs:815` (geometry), the
removed floors (no-ops), `name_field::fit` receiving the style-path size (it only receives the
already-divided sizes; `NameField::draw` shapes via `shape_plain`). Recorded, not changed:
hoisting capture_studio's 51 identical wraps into one local per function (F5's file; the
mechanical form is uniform and each call is the same expression).

## Open items

| item | owner |
|---|---|
| Retained `Chip` is a fixed 28 px pill whose `measure` uses `Theme::dark()` (systemic finding 3's pattern), so at 2× its 24 px label cannot fit; and `label_size_for_height` compounds at 2× (theme-scaled `sm` × `height/28` when the taller pill was itself caused by the larger text). The shell's wheel disc relies on the current semantics (`sidebar_wheel_disc.rs:4176`, 1× only). | F2 (+ S1 for the disc) |
| `activity_timeline` group rows are a fixed `GROUP_ROW_HEIGHT` (52): with line two stacked on the measured title, 2× text overhangs the row bottom instead of overprinting line one. The row height has to follow the text scale; the two `6.0` pads are the literal 27 split honestly, not tokens. | F5 |
| Fixed-height boxes sized from the font size: `Badge` pill (16 px; `Badge::text`/`count` ink is `ink_on_any`, dark on red — unmeasurable by luma, hence not in the x1 table), `tooltip::chip_size` (`text_size + 2·pad`, F4-20), `status_bubble::pill_size` (`text_size + 16`). Labels at 2× overhang. | F2/F3, F4 |
| `status_bubble::pill_size/draw_pill` contract is renderer-relative; two app callers pass raw theme sizes (file-explorer, hex_editor). Either fix the callers or migrate the helper and all 13 callers together to a theme-resolved contract. | A9, A2 |
| The legacy size-based `TextRenderer` APIs are still undeprecated, so every untouched or new site can reintroduce the defect. Proposal: `#[deprecated(note = "pass renderer_type_size(...) or use measure_style/draw_style")]` on `draw_text*`/`measure*`/`elide_to_width*`, or a ratchet test counting `typography.scale.` fed to legacy calls per file. `text_render.rs` is not this unit's file. | text_render owner (F1) |
| `ui/src/math/layout.rs::line_h` — the one renderer-less line-height copy left. | text_render / math owner |
| command_palette's `(scale.sm - 1.0).max(10.0)` / `.max(9.0)` sizes (off the type scale), channel_browser/history_scrubber/activity_timeline's `(sm - 2).max(10)` derived sizes — now single-scaled but still off-scale. | F4, F5 |
| ui-shots scenarios ignore the output-directory argument (write to `/tmp`): make `registry::dispatch` honour it so parallel workers stop overwriting each other's evidence. | tooling |
| App-side candidates from the sweep (`target/ui-review/app_candidates.txt`). | A units |
