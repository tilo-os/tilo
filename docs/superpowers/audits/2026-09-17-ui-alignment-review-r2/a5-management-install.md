# A5 — Management & install: UI alignment review, round two

Worker A5 of the 2026-09-17 round-two alignment/spacing/layout review. Scope:
`firewall`, `backups`, `remote`, `vpn`, `imager` (built and captured on this
macOS host), plus `windows` and `osk` source-only (Linux-only crates).

Base: `381eff6a8` (merge of `ui-review/integration`, containing round one).
Branch: `ui-review/r2-a5-management-install`. Round-one report:
`../2026-09-16-ui-alignment-review/a5-management-install.md`.

## Round one survived the merge

Checked before touching anything: `step_header_h` / `measure_step_heading`
(IM-1), `CONFIRM_ROW_PITCH` (IM-2) and the wrapped `Row` action pairs in
`setup/view.rs` / `setup/import_view.rs` (IM-3) are all present after the
guided-setup commits `087d5a62c` / `0b1f83679` that landed since round one's
base; the firewall `RuleColumns`, backups tab-width derivation, vpn measured
header band / shared `SegmentedFace`, and remote measured header all hold. The
BEFORE captures for this round (`target/ui-review/<app>/before/`) are the
merged tip, and every round-one suite was green on it (firewall 51, backups 36,
vpn 56 + the Linux-only loopback test, remote 132, imager 379 + 1 ignored).

## Method

1. BEFORE captures of the five buildable apps with the isolated helper
   (`target/ui-review/cap.sh`, `cap-imager.sh`): `--responsive-matrix` dark +
   light, `--shots` at 800×900 @2×, 480×900, 800×700, and every listed
   `--scenario` at 1200×800; remote also with `QOS_REMOTE_SAMPLE=1`
   (`remote/before-sample/`). Every PNG opened.
2. One probe of the renderer's line heights: `TextRenderer::measure` returns
   exactly `1.25 × size × text_scale` for every size these apps use (9–22 px),
   weight-independent. Every "1× exact" floor below was derived from that.
3. The systematic fix this round: each app grows a small measured-geometry
   struct (`Metrics` / `RowMetrics` / `SourceMetrics`) whose `Default` is the
   1× layout the app already had and whose `measure(tr)` is each band's
   measured line plus the padding the 1× constant implied, floored at that
   constant. Layout and paint read the same struct, so hit rect == painted
   rect at every text scale, and the 1× frame is pixel-identical (asserted).
   Breakpoints multiply by the text scale (an 800 px window at 2× has the room
   of a 400 px one), which is what turns the collisions in every round-one
   `large-text.png` into the app's own narrow layout.
4. AFTER captures with the same recipe; regressions per fix on the production
   layout/hit path; `code-review` skill; fixes; recapture.

## Findings

Status legend: **fixed**, **open** (owner), **decided**, **unverified on macOS**.

### firewall (`apps/firewall/src/app.rs`)

| id | surface | defect | viewport | before → after | status |
|---|---|---|---|---|---|
| FW-7 | summary card, narrow | `SUMMARY_H_NARROW = 188` reserved a second chip row that never came at 480 px (~60 px blank band) | 480×900 | `firewall/before/narrow.png` → `firewall/after/narrow.png` | fixed — chips are wrapped by the layout into `Layout::stat_chips` from per-zone measured widths; the card is `SUMMARY_H` (134) for one row and grows by exactly one chip row when they wrap (390 / 360 px); the table below gained the released rows. Test `summary_card_height_follows_the_wrapped_chip_rows` |
| FW-8 | every fixed band @2× | header, control band, summary, filter segments, table caption band, rule rows, services rows, rail rows, form title/status bands all fixed; controls overlapped and spilled | 800×900 @2× | `firewall/before/large-text.png` → `firewall/after/large-text.png` | fixed — `Metrics` (measured in `adjust_layout_with_measure` and at the top of `draw`, skipped while text scale and config are unchanged) with 1× floors equal to the old constants; `NARROW_BREAKPOINT` / `SERVICES_BREAKPOINT` and the rail drawer's breakpoint scale with the text; header buttons, form segments and filter/route segments size from their labels; the two-line row's action chip right-aligns to the toggle. Tests `bands_hold_their_1x_floors_and_grow_with_the_text_scale`, `breakpoints_follow_the_text_scale` |
| FW-9 | permanent add-rule form | composition task 19 (disclosure + rule scroll) | 480×900, 800×700 | `firewall/after/compact.png` | alignment part done: the form is bounded by the sum of its measured bands (154 / 226 px at 1×, exactly the old constants) and the table fills every pixel above it; the stacked form's title band now reserves the status line (40 → 50 px at 1×) so a validation message no longer paints across the title. Disclosure/scroll stays **open** (composition task 19) |
| FW-10 | header status line | `paint::label` with a max width *wraps*, so at 2× the status ran onto a second line across the zone rail | 800×900 @2× | before/after `large-text.png` | fixed — elided to the room before the controls, centred on the controls' line (moved ~4 px at 1× onto the same centre line as the buttons) |
| FW-11 | narrow header buttons | narrow form deducted a fixed 12/26/16 px from the measured widths, so a scaled `Panic ON` outgrew its button (code review) | ≥1.6× narrow | — | fixed — separate narrow floors (80/78/76) with the same label padding |

### backups (`apps/backups/src/app.rs`)

| id | surface | defect | viewport | before → after | status |
|---|---|---|---|---|---|
| BK-3b | top-bar tabs at 340 px | `Timeline` clipped inside its tab | 340×844, 2× | `backups/after/large-text.png` (`Destinati…`, `Schedul…`) | fixed — labels elide against the pill's inner width |
| BK-5 | snapshot rows / day headers / detail @2× | `ROW_H=46`, `DAY_H=30`, `DETAIL_H=70` fixed: time under the pill, size over meta, `Restore…` clipped, tabs under the action | 800×900 @2× | `backups/before/large-text.png` → `backups/after/large-text.png` | fixed — rows carry the measured size+meta stack (centred on the row instead of crowding its bottom edge), the day header its bold label (rule on the label's centre), the detail its two lines; pills start after the measured bold time; `Verify`/`Restore…` sized from their 14 px labels; tiles' minimum width and band scale; `BAR_H`/`TAB_H` measured (`chrome_metrics`); `NARROW_BREAKPOINT` and `RAIL_BREAKPOINT` scale with the text. Test `timeline_rows_follow_the_text_scale_without_overlapping` |
| BK-6 | restore sheet | hand-rolled scrim / card / title / footer | all | `backups/before/restore.png` → `backups/after/restore.png` | fixed — `ModalFrame::layout` / `paint_backdrop` / `paint_actions` (`max_width(760)`, `closable(false)` because Cancel + Escape are the visible way out and the sheet's two-control contract is asserted, `dismiss_on_scrim(false)`); the app paints only the body; the hit rects are the frame's own action rects; `Hit::RestoreConfirm` / `RestoreCancel`, Escape and the a11y names unchanged |
| BK-8 | restore sheet scrim | a press outside the sheet fell through to the rows/tabs beneath it (pre-existing) | all | — | fixed — `hit_at` ignores everything but the sheet's controls while it is open; the press is swallowed. Test `restore_sheet_uses_the_shared_modal_frame_and_swallows_the_scrim` |
| BK-7 | `Auto` type pill, light | `lift(0.72)` fill invisible on light | light | `backups/before/light/dev.qos.backups-desktop.png` → `after/light/…` | fixed — dim-ink wash (`text_dim` at 0.18), the same construction as the `Manual` pill's accent wash |
| BK-9 | source path in a row | ran under the right-hand size stack at phone width | 390×844 | `backups/before/dark/dev.qos.backups-phone.png` → after | fixed — elided against the measured stack |

### vpn (`apps/vpn/src/app.rs`)

| id | surface | defect | viewport | before → after | status |
|---|---|---|---|---|---|
| VP-7 | list row `tor` mark | floated at the row's top-right above the toggle, aligned to nothing | populated list | `vpn/before/detail.png` → `vpn/after/detail.png` | fixed — beside the protocol badge on the badge's centre line; the name elides against the measured badge + mark + toggle |
| VP-8 | list rows @2× | `ROW_H = 74` fixed; the 15 px name and 12 px endpoint lines collide | 800×900 @2× | — (the 2× shot shows the empty state) | fixed — `row_h` / `row_line2_top` / `row_name_cy` measured in `adjust_layout_with_measure` (74 / 44 / 23.375 at 1×), read by `layout()` and the painter. Test `connection_rows_follow_the_text_scale` |

### remote (`apps/remote/src/app.rs`)

| id | surface | defect | viewport | before → after | status |
|---|---|---|---|---|---|
| RM-7 | roster rows @2× | `ROW_H=72` / `COMPACT_ROW_H=116`: name over the badge line, buttons overflow, `Needs access` clipped | 800×900 @2× | `remote/before-sample/large-text.png` → `remote/after-sample/large-text.png` | fixed — `RowMetrics` (line2, badge, button height/widths from the widest label, actions line; 1× floors exact); the compact-rows breakpoint (620) scales with the text so 800 px at 2× takes the two-line form; the keyboard scroller reads the same metrics with compactness decided from the current rect, not the last painted frame. Test `roster_rows_follow_the_text_scale_without_overlapping` |
| RM-8 | Delete (edit mode) | hand-painted outlined danger button | form (edit) | — | **open — F2**: `ButtonStyle` is still `Neutral / Primary / Danger / Ghost`; no outlined or ghost-danger face exists in this tree |

### imager (`apps/imager/src/app.rs`)

| id | surface | defect | viewport | before → after | status |
|---|---|---|---|---|---|
| IM-4 | Source step @2× | drop zone, selected card, release rows, recent rows, section captions fixed; everything overlapped | 800×900 @2× | `imager/before/large-text.png` → `imager/after/large-text.png` | fixed — `SourceMetrics` (measured in `adjust_layout_with_measure`; 1× floors exact) read by `layout_source` (hits) and the painters; badges size from their labels; the card summary elides instead of wrapping into the tag line; the Source extent is the measured layout (`source_extent`, floor 650). Test `source_blocks_follow_the_text_scale_without_overlapping` |
| IM-5 | Confirm step void | ~260 px between the danger banner and the bottom-pinned confirmation field | 1200×800 | `imager/before/confirm.png` → `imager/after/confirm.png` | **decided** — the field follows the banner (block gap + its caption line), like every other step's content is top-aligned, so reading order is review → warning → type → footer action and the void is gone; `confirm_field_rect` is the one rect for hit and paint (`ConfirmBands`). Test `confirm_field_follows_the_danger_banner` |
| IM-6 | compact breakpoint | `compact_content` (`width < 520`) was the one breakpoint that did not scale with the text (code review) | 1000 px @2× | — | fixed — scaled by the cached text scale; 17 call sites moved onto `&self` |
| IM-7 | Confirm review card @2× | `CONFIRM_ROW_PITCH = 34` and the compact card's 54/98/132 offsets are fixed; a 14 px value line is 35 px tall at 2× | @2× | — | **open** — measure the card rows the way `SourceMetrics` measures the Source blocks (text-geometry plan task 12, Confirm) |

### windows (`apps/windows/src/app.rs`) — source only

WN-1 (fixed offsets), WN-2 (char-count truncation `compact_detail_value` /
`compact_portal_reason`), WN-3 (`PAD = 20` off scale, `ROW_H`, `SIDEBAR_W`)
re-read; no trivially safe substitution exists — the truncations need the
renderer's `elide_to_width` at call sites whose geometry cannot be verified on
this host. Unchanged; composition plan task 20 owns the file. (The file already
uses `ModalFrame` for its delete confirmation, the same pattern BK-6 adopts.)

### osk (`apps/osk/src/widget.rs`) — source only

| id | surface | defect | status |
|---|---|---|---|
| OK-3 | suggestion-bar hint | centred on the nominal font size while the measured height of the same `measure` call was discarded (`(width, _)`); at 2× the hint sank below the bar's centre | fixed in source (measured height) — **unverified on macOS**; OK-1's already-computed-value substitution, same shape |

## Fresh eyes at 800×700, 480×900 and 200 %

Every AFTER frame was opened. 800×700: all five apps fit their chrome; firewall
shows five rules above the permanent form (FW-9 open), backups keeps the wide
chrome (≥ 720), imager scrolls the Source step. 480×900: firewall's card is one
chip row and the table shows five rules; backups' rows elide their path;
remote's rows are compact; vpn's header stacks; imager's compact card. 200 %
(800×900): every app now takes its own narrow layout instead of colliding —
that is the round-two change in one sentence. Remaining at 2×: firewall's rail
zone rows are measured but the rail width is not (`RAIL_W = 214`; long zone
names elide), imager's Confirm card rows (IM-7), remote's row name does not
elide against the star / last-used (pre-existing, not a fixed-band defect).

## Tests

Every command ran with `RUSTFLAGS="-C force-frame-pointers=yes
--remap-path-prefix=/build=." RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G
CARGO_BUILD_JOBS=1 PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig
cargo … --locked --no-default-features` (the parent checkout's cargo config
otherwise breaks `ring`). Final run after the code-review fixes:

| package | `cargo test -p <pkg> --lib` | new regressions (all on the production layout/hit path) |
|---|---|---|
| firewall | 54 passed (51 + 3) | `summary_card_height_follows_the_wrapped_chip_rows`, `bands_hold_their_1x_floors_and_grow_with_the_text_scale` (also asserts the unmeasured first frame equals the measured 1× frame), `breakpoints_follow_the_text_scale` |
| backups | 38 passed (36 + 2) | `timeline_rows_follow_the_text_scale_without_overlapping`, `restore_sheet_uses_the_shared_modal_frame_and_swallows_the_scrim` |
| vpn | 57 passed + **1 failed**: `net::tests::real_loopback_sys_bytes_are_readable_and_monotonic` reads `/sys/class/net/lo` (Linux-only, pre-existing, file untouched) | `connection_rows_follow_the_text_scale` |
| remote | 133 passed (132 + 1) | `roster_rows_follow_the_text_scale_without_overlapping` |
| imager | 388 passed, 1 ignored (pre-existing), **1 failed in the full run**: `setup::desktop_download::tests::desktop_download_same_origin_redirects_only` — the same `Os { code: 35, WouldBlock }` at `desktop_download.rs:462` round one recorded for its sibling `…inconsistent_range_restarts_cleanly`; both pass alone (`-- desktop_download`: 12/12) and the file is untouched — an order/timing flake, recorded | `source_blocks_follow_the_text_scale_without_overlapping`, `confirm_field_follows_the_danger_banner` |

Gate: `cargo check -p firewall -p backups -p remote -p vpn -p imager --tests`
— ok.

`cargo test -p ui --test it f1_spacing` (the F1 spacing-literal ratchet): no
file **grew** (the review's first draft added 15 module-level pixel constants;
they are now fields of the measured structs' `Default`, the single source of
each 1× floor). The ratchet still fails because it also demands baselines be
*lowered* for files whose count dropped: `apps/firewall/src/app.rs` 18 → 17
(this branch removed `SUMMARY_H_NARROW`) plus two entries that are already
stale at the base (`apps/word-processor/src/zoom_control.rs` 2 → 1,
`shell/src/shell/editor_app.rs` 4 → 3, both untouched here). Lowering
`ui/tests/f1_spacing_token_baseline.txt` is a `ui` edit for the integration
owner (three one-line changes).

## Code review

`code-review high` returned ten findings. Eight fixed before the final run:
the snapshot time label had lost its bold weight (`text_vc` has no weight —
measured and painted bold again); narrow header buttons could outgrow their
label (FW-11); vpn reserved a fixed 72 px for a measured badge (name now elides
against badge + tor slot + toggle); remote's keyboard scroller read the last
frame's compactness (now decided from the current rect and the cached scale);
firewall relayouted a zone-change click against the previous zone's chip
widths (every zone is measured; the measurement is skipped while text scale and
config are unchanged, which also answers the per-frame re-measure finding —
remote and vpn skip theirs the same way); `Metrics::default` diverged from the
measured 1× frame in two fields (now equal, asserted); imager's compact
breakpoint did not scale (IM-6); `RowMetrics` is `Copy`. Two recorded, both
`ui`-owned: the ratchet baseline (above) and a `TextRenderer::line_height(size
[, weight])` helper — five apps now probe `measure("Hg", size)` to get the
line the renderer will paint; one helper (F2/F3) would delete every probe and
the residue pads (`4.625`, `7.625`, `11.25`) that make the floors exact.

## Framework gaps surfaced (open — owners)

- F1: lower the three `f1_spacing_token_baseline.txt` entries above.
- F1: breakpoint × text scale is now re-implemented in firewall, backups,
  remote and imager (and A6's apps before them); one `Theme`/`Metrics` helper.
- F2: outlined / ghost-danger `ButtonStyle` (RM-8).
- F2/F3: `TextRenderer::line_height` (see code review).
- Pre-existing: user-facing literals instead of fluent keys in all five apps.

## Limits

- `windows` and `osk` cannot be built or captured here (Linux-only); OK-3 is a
  substitution of an already-measured value and is marked unverified.
- `vpn`'s loopback test and imager's `desktop_download` timing flake are
  pre-existing and untouched (see run log).
- Remote's scenarios only populate with `QOS_REMOTE_SAMPLE=1`
  (`remote/{before,after}-sample/`); the plain sets show the empty roster.
- Captures live in `target/ui-review/<app>/{before,after}/` (matrix PNGs in
  `dark/` and `light/`, scenario PNGs at the top level).
