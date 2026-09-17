# A5 — Management & install: UI alignment, spacing and layout review

Worker A5 of the 2026-09-16 all-app alignment review. Scope: `firewall`,
`backups`, `remote`, `vpn`, `imager` (built and captured on this macOS host),
plus `windows` and `osk` as source-only (both are Linux-only crates and cannot
be compiled or captured here).

Base: `codex/macos-shell` @ `7c0191dc5` plus the one-line `Cargo.lock` fix
(`ui-review/lockfix`). Branch: `ui-review/a5-management-install`.

## Method

1. Read `ui/HIG.md` §1/§7/§7a, `ui/MANUAL_LAYOUT.md`, `ui/FORM_COMPOSITION.md`,
   `docs/hand-rolled-ui-audit.md`, the 2026-09-15 reviews (`review-1..3.md`,
   `closure-register.md`, `management-disclosure-preflight.md`) and the
   composition / text-geometry plans for the tasks that name these apps.
2. Source pass over every layout file: private spacing consts and literals,
   fudge offsets, text centred on font size instead of measured line height,
   duplicated geometry between paint and hit-test, sibling gap mismatches.
3. BEFORE captures for every buildable app: `--responsive-matrix` (dark and
   light), `--shots` at 800×900 @2×, 480×900, 800×700, and every `--scenario`
   listed for the unit at 1200×800. Every PNG was opened and inspected.
   Remote's scenarios only populate with `QOS_REMOTE_SAMPLE=1`; those live in
   `remote/before-sample/`.
4. Fixes with measured text, one geometry description per screen, and shared
   `ui` faces; a regression per fix on the production layout/hit path.
5. AFTER captures with the same recipe; comparison recorded per finding.

Capture root: `target/ui-review/<app>/{before,after}/` (matrix PNGs in
`dark/` and `light/`; scenario PNGs at the top level). Helper scripts:
`target/ui-review/cap.sh`, `batch1.sh`, `batch2.sh`, `remote-sample.sh`.

Build note for this host: `/Users/xaver/dev/qos/.cargo/config.toml` (the parent
checkout's config, which cargo merges into every worktree beneath it) adds
`-C target-cpu=generic` for non-Linux aarch64, which makes `ring 0.17.14` fail
its static CPU-feature assertion. Every cargo command here ran with
`RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=."` (the
worktree config's own macOS flags) to override the merged list.

## Conventions observed (all five apps)

- Every colour comes from `Theme`; no new literals were added. Font sizes are
  passed to the legacy base-size `TextRenderer` API (`draw_text(size)`), which
  applies the accessibility scale internally, so they are not resolved
  typography tokens; migrating them means moving to `TextStyle` +
  `renderer_input_style` + `draw_style` (recorded as a convention gap, not
  changed here).
- The systematic defect class across firewall, backups, vpn, remote and osk was
  vertical centring on the nominal font size (`(h - size) / 2`) or on a
  `size * 1.3` approximation. Both under-read the measured line at 1× (13 px
  type measures ~17 px, so labels sat ~2 px low) and by half a line at 2×.
  All such helpers now centre on `TextRenderer::measure*` heights.
- Private spacing consts kept deliberately (named, documented): firewall
  `RULE_TEXT_INSET = 14` (status dot + gap, off scale on purpose), backups
  `PILL_PAD_Y = 3` and `MIN_TAB_W = 44`, imager `CONFIRM_ROW_PITCH = 34` and
  `STEP_SUB_GAP = 2`, vpn `HEADER_CONTROL_H = 36` (1× floor, measured live).
  The imager heading-to-content gap reads `theme.tokens.spacing.s3` (cached in
  `build_layout` for the theme-less renderer hook) rather than a literal.
- Framework gap surfaced by this pass (open — needs F2): the measured
  vertical-centring helper now exists four times (`firewall::paint::centered_top`,
  `backups::paint::text_vc`, vpn `text_vc`/`text_center`, osk inline) and
  `grep 'f64::from(size)) / 2.0' apps/*/src` still hits ebook, file-explorer,
  git_client, hex_editor, maps, notes, pdf_viewer, sheets and themes. One
  weight-aware `TextRenderer` helper (measure + draw, vertically centred) would
  let every app delete its copy.
- Framework gap (open — needs F2): no outlined/ghost danger `ButtonStyle`, so
  remote's edit-mode Delete stays hand-painted (RM-8).

## Findings

Status legend: **fixed**, **open** (with owner), **unverified on macOS**.

### firewall (`apps/firewall/src/{app,paint}.rs`)

| id | surface | defect | viewport/theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| FW-1 | rules table header | `TARGET`/`RULE` caption started at the status-dot x (243) while row text starts 14 px later (257); "No rules match" note likewise | all widths, dark+light | `app.rs` `rule_columns` (tuple), `draw_table_header`, `draw_table_row_*` | `firewall/before/dark/dev.qos.firewall-desktop.png` → `firewall/after/dark/dev.qos.firewall-desktop.png` | fixed — one `RuleColumns { dot, text, … }` read by header, rows and the empty note; captions centred in the 30 px header band |
| FW-2 | every centred label/chip | `paint::label_v`, `label_right_v`, `chip` centred on `size * 1.3`; scale-blind and ~1–2 px off at 1× | all | `paint.rs` `line_height` | `firewall/before/large-text.png` → `firewall/after/large-text.png` | fixed — `paint::centered_top` uses the measured line; test `centered_top_tracks_the_renderer_text_scale` |
| FW-3 | services panel header | `SERVICES` caption (top at y0+14) and `3 enabled` count (centred at y0+14) on different lines (~7 px) | ≥1000 wide and compact route | `app.rs` `draw_services` | desktop before → after; `firewall/after/services-narrow.png` (compact route) | fixed — both centred in the 48 px (docked) / 38 px (compact) header band that `layout_center` reserves above the first row |
| FW-4 | filter row | a second `Rules` caption painted beside the Rules/Services segmented; at 390 wide it ran into the `All` filter segment | <1000 wide (tablet, narrow, phone) | `app.rs` `draw_filter` | `firewall/before/dark/dev.qos.firewall-phone.png` → `firewall/after/dark/dev.qos.firewall-phone.png` | fixed — caption only when no route control names the section |
| FW-5 | summary stat chips | caption/value stack top-padded 6, bottom 4 | all | `app.rs` `stat_chip` | desktop before → after | fixed — stack centred from measured heights |
| FW-6 | summary title | `ACTIVE ZONE` chip centred at a nominal y0+26 rather than the title's measured centre | all | `app.rs` `draw_summary_title` | — | fixed |
| FW-7 | summary card, narrow | `SUMMARY_H_NARROW = 188` reserves ~60 px of blank band when the chips fit one row | 480×900 | `app.rs:52`, `layout_center` | `firewall/before/narrow.png` | open — needs the summary height measured from the chip rows in `adjust_layout_with_measure`; composition plan task 19 (disclosure) covers the same region |
| FW-8 | whole centre column @2× | fixed `HEADER_H/SUMMARY_H/FORM_H/RULE_ROW_H/CTRL_H` bands collide once type doubles (header wraps into the rail, chips spill into the filter row, form controls overlap) | 800×900 @2× | `app.rs:45-65` | `firewall/before/large-text.png` | open — app-owned measured geometry (prior review-2 P1); FW-2 removes the label-centring half of it |
| FW-9 | rules table, compact/narrow | permanent add-rule form leaves 4–5 visible rules and no scroll path | 480×900, 800×700 | `layout_center`, `build_table_rows` | `firewall/before/compact.png` | open — composition plan task 19 (rule-form disclosure + rule scroll), not an alignment fix |

### backups (`apps/backups/src/{app,paint}.rs`)

| id | surface | defect | viewport/theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| BK-1 | `paint::text_vc`, `pill`, `tag_pill` | centred on the nominal size; type pill (`size+8`) and retention tag (`size+7`) had different heights on one row | all | `paint.rs` | `backups/before/dark/dev.qos.backups-desktop.png` → after | fixed — measured line height, one `PILL_PAD_Y` |
| BK-2 | timeline header | `Snapshot timeline` painted bold but measured regular, so `All sources` crowded it; scope/`newest first` placed by +1/+2 px nudges | all | `app.rs` `draw_timeline_view` (~1110) | desktop before → after | fixed — `measure_weighted(Bold)`; scope and sort note share the title's measured centre (composition plan task 5, backups) |
| BK-3 | top bar, phone | tab strip's 58 px minimum overran its band; the fourth tab (`Policy`) was painted underneath `Back up now` | 390×844 | `app.rs` `draw_top_bar` (`action_width = 122`), `draw_top_bar_tabs` (`clamp(58.0, TAB_W)`) | `backups/before/dark/dev.qos.backups-phone.png` → `backups/after/dark/dev.qos.backups-phone.png` | fixed — below `NARROW_BREAKPOINT` the primary action is the icon-only shared face (`Icon::Archive`, accessible name unchanged), as the breakpoint comment promised, and the tab width now derives from the band (`MIN_TAB_W = 44` floor) instead of a fixed 58; test `phone_width_keeps_every_tab_clear_of_the_primary_action` at 390 and 340 |
| BK-3b | top bar, 340 px | at 340 wide (below the matrix's phone width) the four 48 px tabs are disjoint and clear of the action, but the `Timeline` label clips inside its tab | 340×844 | `draw_top_bar_tabs` (abbreviations start below 96 px) | `backups/after/phone-340.png` | open — shorter abbreviations or the shared face eliding below ~56 px |
| BK-4 | expanded snapshot detail, phone | metadata line ran underneath the `Verify` button | 390×844 | `app.rs` `draw_snapshot_detail` | phone before → after | fixed — both lines elided to the space left of the buttons |
| BK-5 | snapshot rows @2× | fixed `ROW_H=46`/`DETAIL_H`/`DAY_H` rows: time overlaps the type pill, size and meta overlap, tabs and tiles clip | 800×900 @2× | `app.rs:49-56`, `draw_snapshot_row` | `backups/before/large-text.png` | open — measured row heights (prior review-1 P1); BK-1 fixes the pill/label centring part |
| BK-6 | restore dialog | hand-rolled scrim/card frame (buttons already shared faces) | all | `draw_restore_modal_frame` | `backups/before/restore.png` | open — `ModalDialog` adoption (hand-rolled-ui audit) |
| BK-7 | `Auto` type pill, light theme | `lift(0.72)` fill is almost invisible on light; label reads as bare text | light | `draw_snapshot_row` | `backups/before/light/dev.qos.backups-desktop.png` | open — token choice, not geometry |

### vpn (`apps/vpn/src/app.rs`)

| id | surface | defect | viewport/theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| VP-1 | `text_vc`, `text_center` | centred on nominal size (tabs, Tor pill, notice band ~2 px low at 1×; pill label at the pill's bottom edge at 2×) | all; 800×900 @2× | `app.rs:1489-1525` | `vpn/before/large-text.png` → `vpn/after/large-text.png` | fixed — measured at the painted weight |
| VP-2 | header tally | `N saved · N active` top-anchored at +15/+25 px, ~2 px above the controls' centre line | all | `draw_header` | desktop before → after | fixed — centred on the control band (wide) or its own row (compact) |
| VP-3 | header control band @2× | fixed 36 px band; 24 px labels sink to the edge; the wide header (`HEADER_H = 72`) did not grow either | 800×900 @2×; wide @3–4× | `layout` (`controls_top + 36.0`, `base_header_h = HEADER_H`) | large-text before → after | fixed — `control_h` measured from the Import label in `adjust_layout_with_measure`; both header forms end below the band; test `header_control_band_grows_with_the_text_scale` covers compact 2× and wide 2×/4× (text-geometry plan task 22, VPN) |
| VP-4 | import sheet protocol selector | hand-rolled two-button segmented; geometry duplicated in `draw_add_kind_selector` and `add_modal_hit` | all | `app.rs:2753`, `3038` | `vpn/before/add.png` → `vpn/after/add.png` | fixed — one `add_modal_kind_rects` for paint and hit (contiguous strip), painted by the shared `SegmentedFace`; test `add_modal_kind_segments_hit_where_they_are_painted` |
| VP-5 | list rows, notice band | character-count `ellipsize` (18/28 chars, `/6.5` px per char) instead of measured width | all | `draw_list_row`, `draw_header` | `vpn/before/unprovisioned.png` → after | fixed — `elide_to_width*` against the real remaining width; the mono log lines keep the char clamp (measured elision in the default family would under-read the monospace run) |
| VP-6 | compact header (144 px reservation) and 2× EmptyState | prior review-3 P2 and P1 | 480×900; 800×900 @2× | — | `vpn/before/narrow.png`, `vpn/before/large-text.png` | verified closed — header ends after the control band; EmptyState wraps and centres |
| VP-7 | list row `tor` flag | 10 px bold mark floats at the row's top-right above the toggle, aligned to nothing | populated list | `draw_list_row` | `vpn/before/detail.png` | open — minor; would belong beside the kind badge |

### remote (`apps/remote/src/app.rs`)

| id | surface | defect | viewport/theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| RM-1 | kind badge, readiness badge, last-used, Delete label, segment labels, session toolbar name/mode, tool labels | nine different hand offsets (`-1`, `-7`, `-8`, `-9`) standing in for measured centring | all | `draw_kind_badge_at`, `draw_readiness_badge`, `draw_connection_row_meta_and_actions`, `draw_form_footer`, `draw_segment`, `draw_session_toolbar`, `draw_tool` | `remote/before-sample/roster-wide.png` → `remote/after-sample/roster-wide.png`; `session-*.png` | fixed — measured heights; badge height derived from its label |
| RM-2 | form protocol/authentication selectors | hand-rolled segmented (`draw_segment`) | form | `app.rs:5481` | `remote/before-sample/form.png` → `remote/after-sample/form.png` | fixed — `draw_segments` paints the shared `SegmentedFace` over the rects already pushed as hits; hits, actions and keyboard routes unchanged; a draft whose value is not on offer (a legacy RDP kind) paints no selection instead of falsely highlighting the first segment |
| RM-4b | roster keyboard scrolling | `scroll_connection_into_view` sized its viewport from the `HEADER_H` constant, so once the header is measured (RM-4) arrow-key scrolling under-scrolled by the header growth | all | `app.rs` `scroll_connection_into_view` | — | fixed — the draw records `list_top_offset`; keyboard scrolling reads the same value (one geometry for paint and keyboard) |
| RM-3 | session toolbar tool buttons | hand-rolled `draw_tool` face beside the shared `draw_button` | session | `app.rs:6220` | `session-vnc.png` before → after | fixed — shared neutral face, one centre line with Disconnect |
| RM-4 | roster header @2× | title overlapped the tally; `+ New connection` overflowed its fixed 150 px button; header stayed 64 px | 800×900 @2× | `draw_list` (~4206) | `remote/before-sample/roster-large.png` → `remote/after-sample/roster-large.png` | fixed — tally placed below the measured title; header grows to fit the pair; button sized from its label (text-geometry plan task 20) |
| RM-5 | empty state @2× | heading/subtitle/button stacked by fixed +30/+34 px; button 168 px | 800×900 @2× | `draw_empty_state` (~4365) | `remote/before/large-text.png` → `remote/after/large-text.png` | fixed — each line advances by its measured height; button sized from its label |
| RM-6 | form labels | `draw_form_label` returned `y + 22` regardless of the label's height | all | `app.rs:5458` | `form-large.png` before → after | fixed — measured height + 6 |
| RM-7 | roster rows @2× | fixed `ROW_H=72`/`COMPACT_ROW_H=116`: name collides with the badge line, buttons overflow | 800×900 @2× | `app.rs:197-200` | `remote/before-sample/roster-large.png` | open — measured row heights (rest of review-2 P1) |
| RM-8 | Delete (edit mode) | outlined danger button hand-painted (no shared outlined-danger face exists) | form (edit) | `draw_form_footer` | — | open — needs F2 (outlined/ghost danger `ButtonStyle`) |

### imager (`apps/imager/src/app.rs`, `setup/view.rs`, `setup/import_view.rs`)

| id | surface | defect | viewport/theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| IM-1 | Source and Confirm step headings | content started at a fixed `44`/`64` (Source) and `46`/`64` (Confirm) px: the description's glyphs ended ~1 px above the drop zone's dashed border | ≥520 content width | `layout_source:1336`, `draw_source_dropzone:1784`, `draw_confirm_review_card:2638`, `draw_step_title` | `imager/before/source.png` → `imager/after/source.png`; `confirm.png` | fixed — heading group measured (title + wrapped description + the theme's `spacing.s3`, cached from `build_layout`) in `adjust_layout_with_measure`, consumed by layout and paint through one `step_header_h`; `content_extent` grows by the same delta so the bottom-anchored confirmation field scrolls instead of riding into the review card; the 1× floors the compact test pins are kept as minimums; test `step_content_starts_below_the_measured_heading_group` |
| IM-2 | Confirm review card | rows at 20/58/92/122 px (pitch 38/34/30); value text nudged `-1` against its key | ≥520 | `draw_confirm_review_rows`, `draw_confirm_capacity_and_action` | `imager/before/confirm.png` → `imager/after/confirm.png` | fixed — `CONFIRM_ROW_PITCH`; key/value share a measured centre |
| IM-3 | setup experience card; import continuation card | `Recheck Desktop support` / `Choose`, `Review destination` / `Choose another destination…`, `Copy reviewed folders` / `Decide later for all` each stacked as one-button lines | 1200×800 | `setup/view.rs` `experience_body`; `setup/import_view.rs` `continued_review_controls`, `continuation_card` | `imager/before/setup-experience.png` → `imager/after/setup-experience.png`; `import-first-login.png` | fixed — each pair is one wrapping `Row` (declaration/keyboard order unchanged, IDs unchanged) |
| IM-4 | Source step @2× | fixed bands: selected card, release rows and recents overlap | 800×900 @2× | `layout_source`, `draw_source_*` | `imager/before/large-text.png` | open — text-geometry plan task 12 (measured information layout); IM-1 fixes the heading→content part |
| IM-5 | Confirm step | ~260 px void between the danger banner and the bottom-pinned confirmation field | 1200×800 | `draw_confirm_field` | `imager/before/confirm.png` | open — deliberate (confirmation beside the primary action); recorded, not changed |
| IM-6 | step indicator, drive cards, customize form, progress/done, setup-folders, setup-review, import-* | no alignment defect found at 1× (labels/inputs, dropdown rows, centred hero blocks, folder rows) | 1200×800, 480×900, 800×700 | — | `imager/before/*.png` | no change |

### windows (`apps/windows/src/app.rs`) — source only

| id | surface | defect | file:line | status |
|---|---|---|---|---|
| WN-1 | sidebar / detail | every text and control placed by fixed offsets (`sidebar.y0 + 26/53/70/102/120/136/144/176…`); `flow_button_rect` rows fixed at 32 px with a 40 px stride | `app.rs:1485-1560`, `1239` | open — measured heading/row geometry (review-3 P1); composition plan task 20 owns this file |
| WN-2 | value column | `compact_detail_value` / `compact_portal_reason` truncate by character count (34/52) | `app.rs:35-60` | open — measured elision |
| WN-3 | consts | `PAD = 20` (off scale; `s4`=16, `s6`=24), `ROW_H = 72`, `SIDEBAR_W = 296` | `app.rs:27-29` | recorded; no change (cannot verify here) |

### osk (`apps/osk/src/widget.rs`) — source only

| id | surface | defect | file:line | status |
|---|---|---|---|---|
| OK-1 | key cell and suggestion chip labels | centred on the nominal `font` size although the measured height was already computed and discarded (`_th`); at 2× letters sat below the key centre (review-2 P1) | `widget.rs` `draw_cell`, `draw_suggestion_chip` | fixed in source (measured `th`) — **unverified on macOS**; text-geometry plan task 6 |
| OK-2 | row heights | `DEFAULT_KEY_HEIGHT = 56`, `DEFAULT_SUGGESTION_HEIGHT = 40`, toolbar 48 are fixed; `key_rects`/`toolbar_rects`/`suggestion_rects` divide the area evenly, so hit rects equal painted rects | `widget.rs:79-83`, `layout.rs:515`, `toolbar.rs:89` | open — scale-aware minima (task 6); hit == paint holds |

## Tests

Every command below ran with `RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G
CARGO_BUILD_JOBS=1 RUSTFLAGS="-C force-frame-pointers=yes
--remap-path-prefix=/build=."` (see the build note above; without the
`RUSTFLAGS` override `ring` fails its const-assert and nothing compiles). Final
run after the code-review fixes, from `target/ui-review/final-all.sh`:

| package | `cargo check --locked -p <pkg> --tests` | `cargo test --locked -p <pkg>` |
|---|---|---|
| firewall | ok | 51 passed, 0 failed |
| backups | ok | 36 passed, 0 failed |
| vpn | ok | 56 passed, **1 failed** — `net::tests::real_loopback_sys_bytes_are_readable_and_monotonic` (`apps/vpn/src/net.rs:285`) reads `/sys/class/net/lo`; Linux-only and pre-existing, file untouched |
| remote | ok | 132 passed, 0 failed |
| imager | ok | 379 passed, 1 ignored (pre-existing), **1 failed once** — `setup::desktop_download::tests::desktop_download_inconsistent_range_restarts_cleanly` (`apps/imager/src/setup/desktop_download.rs:462/482`), an untouched file; it passed in the earlier full run of this branch and 3/3 when re-run alone (`cargo test -p imager -- desktop_download_inconsistent_range_restarts_cleanly`), so it is an order/timing flake, recorded, not touched |

New regressions (all on the production layout/hit path, all passing):

- firewall `paint::tests::centered_top_tracks_the_renderer_text_scale`,
  `app::tests::table_header_columns_follow_the_built_rows`
- backups `app::tests::phone_width_keeps_every_tab_clear_of_the_primary_action`
- vpn `app::tests::add_modal_kind_segments_hit_where_they_are_painted`,
  `app::tests::header_control_band_grows_with_the_text_scale`
- imager `app::tests::step_content_starts_below_the_measured_heading_group`

windows and osk: not compiled here (Linux-only crates); the osk edit is a
substitution of an already-measured value and is marked unverified.

## Code review

`/code-review high` on the diff returned nine findings; eight were fixed before
the final run (remote keyboard-scroll viewport = measured header; vpn wide
header grows with the band; imager `content_extent` follows the measured
heading; remote segments paint no selection for an RDP draft; backups tab width
derives from the band; firewall caption-band arithmetic made explicit; vpn dead
`x` parameter removed; imager heading gap reads `spacing.s3`). The ninth (one
shared `TextRenderer` centring helper for every app) is a `ui` change and is
recorded above as needing F2.

## Limits

- `windows` and `osk` cannot be built or captured on this host (Linux-only
  `libc::memfd_create`, Wayland input-method). The OK-1 edit is a substitution
  of an already-computed measured value for the nominal size; it is marked
  unverified.
- `vpn`'s `/sys/class/net/lo` test is Linux-only and pre-existing (see run log).
- Remote's `--scenario` set only populates with `QOS_REMOTE_SAMPLE=1`; the
  plain `before/` scenario PNGs therefore all show the empty roster and the
  populated evidence lives in `before-sample/` / `after-sample/`.
