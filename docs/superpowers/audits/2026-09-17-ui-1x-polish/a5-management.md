# A5 — firewall, backups, remote, vpn: 1× polish (edges, touching, rhythm)

Branch `ux1x/a5-management`, base `fd1f84391`. Files edited: `apps/{firewall,backups,remote,vpn}/**`
only (`apps/firewall/src/{app,main,paint}.rs`, `apps/backups/src/{app,main,paint}.rs`,
`apps/remote/src/app.rs`, `apps/vpn/src/{app,throughput}.rs`). Evidence root:
`target/ux1x/a5/<app>/{before,after}/{dark,light}/` (PNG + `.inspection.json` + `.targets.json`
per scenario, plus the `--responsive-matrix` phone/tablet/laptop/desktop PNGs). All captures at
`QOS_SHOTS_TEXT_SCALE=1.0`, 1280×800 unless named otherwise.

This unit was resumed from a WIP checkpoint (`5d7f0a1d5`) after a machine reboot; the binding
findings of that checkpoint's code review (a–f below) were all still open and are closed here.

## Six-rule checklist

| # | Rule | Status | Evidence |
|---|---|---|---|
| 1 | Nothing touches or overlaps; every neighbouring pair ≥ `s1` | Done | Pair tables below (from `*.inspection.json` via `target/ux1x/a5/gaps.py`); `geometry_audit` over all 90 AFTER inspections: 0 `semantic_bounds_overlap_unresolved`, 0 `touching_peers`. Fixed: backups view tabs 0 → 8 px, rail toggle running 6 px into the first row → 8 px, rail rows 4/6/2 → 8 px, snapshot rows 4 → 8 px, firewall service stepper `<`/value/`>` touching → 4 px painted gap (hit rects contiguous), remote list rows 10 → 8 px (S2) with one stride for painter and keyboard scroller. |
| 2 | Every surface has a derived edge or contrast step; no `border_standard`/`border_strong`/`*_white_*`/`neutral[]` on raised/overlay grounds | Done | Every app card/row/tile/pill goes through `ui::surface::{card, pill, separator, edge_on, edge_on_with, edge_outline}`; Bars use `ui::chrome::chrome_hairline`, floating rails/modals `panel_ground`+`panel_border`; selection is a lift/wash + derived ring (`selection_tier` on `base`, `selected_on(ground)` on raised/panel grounds — see Open). `grep -n 'border_standard\|neutral\[' apps/{firewall,backups,remote,vpn}/src/*.rs` → 0 hits in owned files. Visible in every AFTER PNG in both themes (surface table). |
| 3 | One rhythm per screen | Done | Each app carries `const S1..S6 = Spacing::BASE.sN` and a documented rhythm: S1 label↔control / row↔rule, S2 related rows, S3 siblings, S4 padding+sections, S6 next group. Duplicate steps for one relationship removed (e.g. firewall 14/13/18 → S3/S3/S4, backups 18/22/10 → S6/S6/S4, remote 56-px form stride → 40 + S4, vpn tile gap 8/12 → S3). |
| 4 | Content centred on measured line boxes | Done | firewall chip caption/value stack centred (`draw_summary_chip`), rail trust dot on the row centre line; backups rail header caption + collapse toggle on one centre line, destination icon centred on the measured name line, view header subtitle on the measured title line (`view_body_top`); remote header tally on the measured title line, `last_used` centred on the action line; vpn Status kv rows derived from `status_geometry`. |
| 5 | No "authored pixel" exemption | Done | Rhythm offsets are token expressions (`PAD = S4`, `SEARCH_H = Metrics::BASE.min_pointer_target + S1`, `TAB_PILL_INSET = S1`, `ROW_GAP = S2`, `SUMMARY_H`/`FORM_H` composed from S-steps + measured lines). f1 ratchet: no `grew`; `stale` (count went down) for backups 11→10, firewall 17→14, vpn 5→4. Remaining fixed sizes are control dimensions (button widths, 30 px kv row, 30 px collapse toggle) — listed under Limits. |
| 6 | Visual evidence, dark and light | Done | 61 AFTER scenes × 2 themes (+ 2 extra viewports) opened with the Read tool; BEFORE defaults + representative scenes opened for comparison. Per-PNG notes in the surface table. |

## Binding findings from the checkpoint review — all closed

| id | finding | fix | evidence |
|---|---|---|---|
| a | firewall selected rule row used `selection_tier(..).fill` (= `lift(1.0)` = `raised` on light) on the raised table card → invisible | `selected_on(ground) -> (fill, ring)`: fill = `ui::surface::edge_on(ground)` used as a wash (climbs the alpha ladder until Δ ≥ `HAIRLINE_MIN_DELTA`, pure-black high-contrast included), ring = `edge_on_with(ground, PANEL_BORDER_ALPHA)`; the docked rail (ground `base`) keeps `selection_tier`, the overlay rail derives against `panel_ground`. New `selected` shots scenario. | `firewall/after/{dark,light}/selected.png`; test `selected_rows_register_on_the_raised_card_on_every_preset` (dark, light, high-contrast; also asserts the tier's lift does *not* register on light). |
| b | remote `scroll_connection_into_view` stride `row_h + 10.0` vs painter `row_h + S2`; `list_top_offset` seed `HEADER_H + 56.0` | `list_stride(row_h) = row_h + S2` for both; seed `HEADER_H + S2 + SEARCH_H + S4` (the rhythm `draw_list` paints) | test `keyboard_scroller_and_painter_share_the_list_stride` (draws at 1280×400, asserts the painter's `list_scroll_max` equals the scroller's). |
| c | vpn `stack_header` (< 560 px) pushed the Status Tor row below a 640-px window with no scroll outside compact mode; `handle_detail_scroll` hard-coded 410 | detail pane clips and scrolls whenever shown (docked too); Status extent from `status_content_height` = same geometry the painter (`status_kv_table`) and the Tor hit rect use; `apply_layout` clamps the offset on resize | test `docked_detail_scrolls_the_tor_row_into_view_at_1024x640`; `vpn/after/{dark,light}/detail-1024x640.png` (stacked header, table clipped at the pane, reachable by wheel). |
| d | firewall `Add rule` lacked `.hovered`; panic ring stroked on the raw card rect; `service_sel` inset left 4-px dead zones; `draw_rail` > 100 lines | `.hovered(l.add_btn.contains(self.last_cursor))`; ring on `ui::surface::edge_outline(card, CARD_R)`; hit rects contiguous, painted well inset `S1`; `draw_rail_zone_row` split out (draw_rail 62 lines) | test `service_selector_hit_rects_are_contiguous` (wide + stacked); `firewall/after/*/default.png` (form line 2). |
| e | backups rail: measured destination rows grew ~12 px in a fixed non-scrolling clip; 30-px toggle band reserved without a toggle (overlay); separator/hover derived against `raised` while the overlay rail is `panel_ground` | rows scroll under a fixed header band (`rail_scroll`, wheel over the rail, hit rects clipped to the band, thumb); band is `cap_h` when no toggle; `rail_ground()` (raised or `panel_ground`) drives separator, hover wash and selected face | tests `short_rail_scrolls_its_rows_under_a_fixed_header` (1280×480) and `tall_rail_does_not_scroll`; `backups/after/{dark,light}/default-1280x480.png` (thumb, clipped last row). |
| f | `const S1..S6 = Spacing::BASE.*` do not follow `theme.tokens.spacing` under `ui_scale` | accepted per plan for compile-time rhythm consts; recorded under Limits | — |

## Surface table (before → after, dark and light)

Paths are relative to `target/ux1x/a5/`. "Before" is the base commit (`fd1f84391`) binary; the
backups `sources`/`destinations`/`schedules` before-shots come from a base binary with only the
three scenario names added (the checkpoint's `target-build-base-views.log`).

### firewall (`firewall/{before,after}/{dark,light}/`)

| scene | before (dark / light) | after (dark / light) | defects fixed |
|---|---|---|---|
| `default.png` | cards stroked `border_standard` on `raised` (invisible on dark), stat chips `neutral[1]` bare fills, header `neutral[0]` + `border_standard` rule, Panic `neutral[2]` pill, hand-painted Apply/Add/Set-active buttons with `Color::WHITE` ink, table caption rule `border_standard`, row rules `alpha(border_standard,120)`, services pane `border_standard` divider, enabled services `lift(0.35)` bare | summary/table/form cards `paint::card` (derived edge), chips `card(base)` wells, header Bar `raised` + `chrome_hairline`, Panic/Apply/Set-active/Add/Retry `Button::paint` faces, table caption + row rules `separator` inset S3 / centred in the S1 gap, services pane edge `edge_on(base)`, enabled tiles `card(raised)`, status pill `ui::surface::pill` with tone edge, rail divider `edge_on(base)` / `panel_border` | 2, 3 (PAD 16/18/14 → S4; title↔desc S1, desc↔chips S3, chips↔bottom S4; filter↔table S3; form lines S3), 4 |
| `selected.png` (new scenario) | — (base: `lift(0.72)` + accent ring; checkpoint: `lift(1.0)` = card colour on light) | selected `8080/tcp` row: derived wash + panel-strength ring, clearly visible on both palettes | a |
| `empty.png` | as default, `Set as active` `neutral[2]` fill + accent ring | `Set as active` Neutral button face; empty table card edged | 2 |
| `error.png`, `confirmation.png`, `active-change.png`, `services.png` | as default | as default; delete-armed ✕ keeps its semantic ring on top of the tinted face | 2 |
| `service-error.png` | unavailable card `border_standard`, Retry hand-painted | `paint::card` + primary `Button::paint` with hover | 2 |
| `dev.qos.firewall-{phone,tablet,laptop,desktop}.png` | — | compact: stat chips wrap with S3, service selector well inset S1, form stacked; tablet: Rules/Services toggle + edged cards | 1, 3 |

Every AFTER PNG opened: dark and light `default`, `selected`, `empty`, `error`, `confirmation`,
`active-change`, `service-error`, `services`, `phone`, `tablet`; before `default` dark + light.
One-line notes: all cards/tiles/rows show a 1 px derived edge on both palettes; no pair touches;
the header, table and form share the S3/S4 rhythm; light did not regress (chips, pills and the
Panic face all keep a visible edge on white).

### backups (`backups/{before,after}/{dark,light}/`)

| scene | before | after | defects fixed |
|---|---|---|---|
| `default.png` / `status.png` | rail `raised.with_alpha(0.45)` + `hairline_on(raised,26)`, "All sources" accent wash + accent ring, collapse toggle (30 px) overlapping the first row by 6 px, source rows 6/4 px apart, destination rows 48 px fixed (usage caption clipped 1–3 px into the next name), tiles `hairline_on` edges but 12 px / 18 px / 22 px mixed gaps, snapshot rows `raised.with_alpha(0.55)` bare, selected row accent wash, detail `neutral[0].with_alpha(0.5)`, timeline day rule `hairline_on(raised)` on `base`, tab pills touching (0 px) | rail `card(raised)` docked / `panel_ground`+`panel_border` overlay, rows S2 apart under a header band that reserves the toggle only when drawn, measured destination rows (name/bar/usage S1 apart), selected row = wash + ring measured against the rail ground, hover half wash; tiles `card(raised)` S3 apart, S6 to the timeline header; snapshot rows `card(lift(0.5))` at rest, `card(raised)` on hover, `card(raised)` + `selected_ring` selected, detail attached at S1; day rule `separator` against `base`; tab pills inset S1 (8 px apart, hit cells contiguous, a11y bounds = pill) | 1, 2, 3, 4, e |
| `progress.png` | banner `fill_rrect(raised)` bare, track `neutral[4]` | banner `card(raised)` S2 under the bar, track `base` | 2 |
| `failure.png`, `restore.png` | toast/modal from shared widgets; tree well `neutral[0]` | tree well `card(base)`, size-summary rule `separator` | 2 |
| `sources.png` | selected card accent ring; recent-activity `raised.with_alpha(0.45)`; header 50 px fixed | `card(raised)` + `selected_ring`; activity `card(raised)` S6 under the cards, row rules `separator` inset S3, rows S2 under the measured title | 2, 3 |
| `destinations.png` | cards 16 px apart, aggregate 10 px, bars/rings `neutral[2]` | cards S3, aggregate S6, bars `base`/`lift(1.0)` | 2, 3 |
| `schedules.png` | focused row accent wash + accent ring, rows 64-px stride, side/ruler 18/16/22 px | focused row `lift(0.5)` + `border_focus` 2 px, rows 56 + S2, editor↔summary S4, ruler S6 | 2, 3 |
| `empty.png` | tabs track `raised` + hairline | track `card(base)`; centred empty state | 2 |
| `default-1280x480.png` (extra, after only) | — | rail rows scroll: thumb at the rail's right edge, last row clipped at the band, "All sources" wash + ring | e |
| `dev.qos.backups-{phone,tablet,laptop,desktop}.png` | — | phone: 2×2 tiles S3, abbreviated tabs 8 px apart; tablet: overlay rail closed, 4 tiles; laptop as default | 1, 3 |

Every AFTER PNG opened: dark and light `default`, `empty`, `status`, `progress`, `failure`,
`restore`, `sources`, `destinations`, `schedules`, `phone`, `tablet`, `default-1280x480`, dark
`laptop`; before `default` dark + light. Notes: every rail row, tile, snapshot row, card and
modal well carries a visible derived edge in both palettes; the rail's header no longer collides
with the first row; the destination usage caption sits a full step above the next name.

### remote (`remote/{before,after}/{dark,light}/`)

| scene | before | after | defects fixed |
|---|---|---|---|
| `default.png` | nav rail `neutral[1]` + `border_standard` seam, active item accent wash, rows `neutral[1]` + `border_standard`, selected row accent tint + 1.5 px accent ring, rows 10 px apart, search `neutral[0]` + `border_standard`, header 14/2/10 px | rail Bar `raised` + `chrome_hairline`, active item wash + ring (`selected_on(raised)`), rows `card(raised)` S2 apart, selected = wash + panel ring, hover = ring, search `card(base)`, header S4/S1/S3, search S2 under the header, list S4 under the search, Edit↔Connect S3 | 1, 2, 3, b |
| `narrow.png`, `narrow-open.png`, `empty.png` | as default (hamburger `neutral[1]` + `border_standard`) | hamburger `card(raised)` + panel ring on hover; empty state unchanged shared widget | 2 |
| `form.png`, `form-qos.png` | fields `neutral[0]` + `border_standard`, picked device accent tint + accent ring, Delete hand-painted danger outline, 56/66/44-px strides | fields `card(base)` (focus `border_focus` 2 px), device chips `card(base)`, picked `card(raised)` + panel ring, Delete `danger_outline` face, groups 40 + S4, labels S1 above, chips S2 apart | 2, 3 |
| `session-{ssh,vnc,rdp,qos,connecting,failed}.png` | card `hairline_on(raised,51)` | `card(raised)`; state pills `ui::surface::pill` with tone edge; Disconnect `danger_outline` | 2 |
| `confirm-{connect,delete,ssh-trust}.png` | dialog `raised` + `border_focus` ring | Panel role: `panel_ground` + `panel_border` on `edge_outline` | 2 |
| `dev.qos.remote-{phone,tablet,laptop,desktop}.png` | — | phone: stacked rows with actions on their own line, `last_used` dropped when it would run under Edit | 1 |

Every AFTER PNG opened: dark and light `default`, `form`, `form-qos`, `session-ssh`,
`session-qos`, `session-connecting`, `session-failed`, `confirm-connect`, `confirm-delete`,
`confirm-ssh-trust`, `empty`, `phone`; dark `narrow`, `narrow-open`, `session-vnc`,
`session-rdp`; light `session-vnc`, `session-rdp`; before `default` dark + light.

### vpn (`vpn/{before,after}/{dark,light}/`)

| scene | before | after | defects fixed |
|---|---|---|---|
| `default.png` / `detail.png` | selected row accent tint + accent ring, list pane edge `hairline_on(raised)` on `base`, tiles/chart `hairline_on(raised,26)` (chart on `base`), kv rules half-alpha hairlines, header hairline, Tor pill `border_standard`, tiles 8/12 px gaps | selected row `selection_tier` lift + panel ring (rows sit on `base`), hover `lift(0.5)`; pane edge `edge_on(base)`; tiles and chart `card(raised)` S3/S4; kv rules `separator` against `base`; header `chrome_hairline`; Tor pill `pill` with `edge_on`/accent edge; kind badges and status pill `pill` with tone edges | 1, 2, 3 |
| `config.png`, `logs.png` | `neutral[0]` card + `hairline_on(raised)` | `card(raised)` | 2 |
| `add.png`, `import.png`, `confirm.png` | modal `raised` + `hairline_on(raised,51)`; command well `neutral[0]` | Panel `panel_ground` + `panel_border`; well `card(base)` | 2 |
| `error.png`, `loading.png`, `connecting.png`, `disconnected.png`, `unprovisioned.png`, `empty.png`, `setup.png` | as default | as default (notice band + shared buttons) | 2 |
| `detail-1024x640.png` (extra, after only) | — | stacked header (pane < 560 px), Status table clipped at the pane and scrollable (test) | c |
| `dev.qos.vpn-{phone,tablet,laptop,desktop}.png` | — | phone: list only; tablet: full-width action, 2×2 tiles S3, compact chart | 1, 3 |

Every AFTER PNG opened: dark and light `default`, `detail`, `error`, `add`, `config`,
`disconnected`, `detail-1024x640`; dark `logs`, `unprovisioned`, `loading`, `phone`, `tablet`;
before `default` dark + light.

## Pair tables (px, before → after; from the inspection JSON, dark; light geometry is identical)

Before columns marked "—" have no geometry: the base binaries exported far fewer bounded
semantic nodes (firewall 2, remote 5) than the AFTER binaries (14, 31); those "before" values are
taken from the base code.

| surface (PNG) | element A | element B | axis | before → after | relationship / step | OK |
|---|---|---|---|---|---|---|
| firewall `default` | zone row | next zone row | y | 4 → 4 | rail rows (existing S1) | yes |
| firewall `default` | Firewall service pill | Panic | x | 8 → 8 | header siblings (S2, existing) | yes |
| firewall `default` | Panic | Apply | x | 8 → 8 | header siblings | yes |
| firewall `default` | summary title | description | y | 4.5 → 4 | label ↔ explanation S1 | yes |
| firewall `default` | description | chip row | y | 13 → 12 | next sibling group S3 | yes |
| firewall `default` | last chip row | card bottom | y | 20 → 16 | card padding S4 | yes |
| firewall `default` | filter row | rules table | y | 10 → 12 | siblings S3 | yes |
| firewall `default` | form line 1 | form line 2 | y | 14 → 12 | siblings S3 | yes |
| firewall `default` | `<` stepper | value well | x | 0 → 4 (hit rects contiguous) | control ↔ control S1 | yes |
| firewall `default` | value well | `>` stepper | x | 0 → 4 | S1 | yes |
| firewall `default` | source field | Add rule | x | 16 → 16 | section S4 | yes |
| firewall `default` | service tile | next tile (docked / compact) | y | 6 / 2 → 8 / 4 | related rows S2 / dense S1 | yes |
| backups `default` | Timeline tab pill | Sources tab pill | x | 0 → 8 | siblings (S2 between pills, cells contiguous) | yes |
| backups `default` | collapse toggle | "All sources" row | y | −6 (overlap) → 8 | header band ↔ rows S2 | yes |
| backups `progress` | source row | next source row | y | 4 → 8 | related rows S2 | yes |
| backups `progress` | destination row | next destination row | y | 2 → 8 | related rows S2 (rows now measured) | yes |
| backups `progress` | snapshot row | next snapshot row | y | 4 → 8 | related rows S2 | yes |
| backups `default` | selected snapshot row | its detail | y | 2 → 4 | row ↔ attached detail S1 | yes |
| backups `default` | tile label | tile value | y | 7.6 → 8 | related lines S2 | yes |
| backups `default` | tile block | timeline header | y | 18 → 24 | next group S6 | yes |
| backups `sources` | source card | next card | y | 12 → 12 | siblings S3 | yes |
| backups `sources` | last card | Recent activity | y | 10 → 24 | next group S6 | yes |
| backups `destinations` | destination card | next card | x | 16 → 12 | siblings S3 | yes |
| backups `schedules` | retention row | next row | y | 8 → 8 (stride 64 → 56 + S2) | related rows S2 | yes |
| backups `schedules` | editor panel | summary panel | x | 18 → 16 | sections S4 | yes |
| remote `default` | title | tally | y | 2 → 4 | label ↔ explanation S1 | yes |
| remote `default` | header | search field | y | 4 → 8 | related row S2 | yes |
| remote `default` | search field | first row | y | 16 → 16 | section S4 | yes |
| remote `default` | row | next row | y | 10 → 8 | related rows S2 (one stride) | yes |
| remote `default` | Edit | Connect | x | 10 → 12 | sibling buttons S3 | yes |
| remote `default` | last_used | Edit | x | 20 → 16 (dropped when it would collide on a phone row) | S4 | yes |
| remote `form` | field label | field | y | 6 → 4 | label ↔ control S1 | yes |
| remote `form` | field group | next group | y | 37 → 35 (40 + S4 stride) | section S4 | yes |
| remote `form-qos` | device chip | next chip | y | 8 → 8 | related rows S2 | yes |
| remote `form` | SSH | VNC | x | 0 → 0 | segments of one segmented control share a track edge by design | n/a |
| vpn `default` | connection row | next row | y | 8 → 8 | related rows S2 (`ROW_GAP = S2`) | yes |
| vpn `default` | Tor pill | Import | x | 12 → 12 | header siblings S3 | yes |
| vpn `default` | stat tile | next tile (wide / compact) | x | 12 / 8 → 12 / 12 | siblings S3 | yes |
| vpn `default` | tiles | chart | y | 16 → 16 | sections S4 | yes |
| vpn `default` | chart | kv table | y | 16 → 16 | sections S4 | yes |

Full nearest-neighbour dumps: `target/ux1x/a5/gaps.py <inspection.json>`; the before/after
comparison used for this table is reproduced by `python3 /tmp/a5/pairs.py` in the session log and
is derivable from the committed inspection JSONs.

## Geometry lint

`python3 -c 'import sys,json; sys.path.insert(0,"scripts/ux-bench"); import usability; ...'` over
all 90 AFTER `*.inspection.json` (firewall 16, backups 16 (+2 partially clipped rows in
`progress` — the last visible snapshot row is intentionally cut by the timeline clip), remote 26,
vpn 28 + `detail`): **0 failures** (`semantic_bounds_overlap_unresolved` 0, `touching_peers` 0).
Remote's 24 list/form/session inspections report `measured_controls: 0` (see Limits).

## Ratchets (`cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit`)

```
test design_token_audit::declared_borders_in_chrome_do_not_grow ... ok
test design_token_audit::theme_escape_hatches_do_not_grow ... ok
test design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow ... FAILED
test f1_spacing_token_audit::spacing_literals_do_not_grow ... FAILED
apps/browser/src/browser_tabs.rs [literal_color]: 1 occurrence(s), baseline 0
apps/browser/src/browser_tabs.rs [white_or_black]: 1 occurrence(s), baseline 0
shell/src/shell/frame.rs [literal_color]: 1 occurrence(s), baseline 0
shell/src/shell/frame.rs [white_or_black]: 2 occurrence(s), baseline 0
apps/backups/src/app.rs [pixel_const]: now 10, baseline still 11 — lower it to 10
apps/firewall/src/app.rs [pixel_const]: now 14, baseline still 17 — lower it to 14
apps/vpn/src/app.rs [pixel_const]: now 4, baseline still 5 — lower it to 4
```

- No `grew` line. The three `stale` lines are this unit's files (counts went down); baselines are
  the coordinator's to lower.
- `app_and_shell_theme_escape_hatches_do_not_grow` fails on `apps/browser/src/browser_tabs.rs` and
  `shell/src/shell/frame.rs`, which this branch does not touch (`git diff fd1f84391 --stat` lists
  only the nine A5 files) — red at the base commit, not introduced here.
- `apps/firewall/src/app.rs white_or_black` went 5 → 0 (`Color::WHITE` inks replaced by
  `on_surface(accent).ink` / shared button faces); `apps/remote/src/app.rs` `literal_color`/
  `white_or_black` unchanged at 1/1 (not in the painted surfaces this unit touched).

## Tests run

| command | result |
|---|---|
| `cargo test -p firewall --lib` | 56 passed, 0 failed (new: `service_selector_hit_rects_are_contiguous`, `selected_rows_register_on_the_raised_card_on_every_preset`) |
| `cargo test -p backups --lib` | 40 passed, 0 failed (new: `short_rail_scrolls_its_rows_under_a_fixed_header`, `tall_rail_does_not_scroll`; `Frame::wheel` helper) |
| `cargo test -p remote --lib` | 134 passed, 0 failed (new: `keyboard_scroller_and_painter_share_the_list_stride`) |
| `cargo test -p vpn --lib` | 58 passed, 1 failed: `net::tests::real_loopback_sys_bytes_are_readable_and_monotonic` reads `/sys/class/net/lo/statistics` (Linux only) — `apps/vpn/src/net.rs` is untouched by this branch; environment-red on this Mac at base. New: `docked_detail_scrolls_the_tor_row_into_view_at_1024x640` passes. |
| `cargo check -p {firewall,backups,remote,vpn} --tests` | all four finished, no errors |
| `cargo build -p firewall -p backups -p remote -p vpn` | finished |
| `cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit` | 7 passed, 2 failed as pasted above (no `grew`) |

All cargo invocations used the inline `RUSTFLAGS="-C force-frame-pointers=yes
--remap-path-prefix=/build=." RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1
PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig` prefix with `--no-default-features
--locked`.

## Code review

The `code-review` skill (medium) was invoked after the production edits; its forked run returned
only an interim status (its finder agents did not report back within the session), so a manual
regression pass over `git diff fd1f84391` followed. Findings fixed during the session: the
`selected_on` wash at a fixed `HAIRLINE_ALPHA` did not register on the high-contrast preset (caught
by the new firewall test; now `edge_on(ground)`); remote/backups/vpn selected rows only registered
through their ring on dark (fill Δ ≈ 0.007 < `HAIRLINE_MIN_DELTA`) — now the same ground-measured
face / `selection_tier` on `base`; an unused `let c` in backups `row_bg`; rustfmt applied to the
hunks this branch changed (the four files were not fmt-clean at base — untouched lines were left
as they were).

## Open — needs another unit

- **Open — needs coordinator (closed on `ux1x/integration`)**: replace the three identical
  `selected_on(ground)` helpers (`apps/firewall/src/app.rs`, `apps/backups/src/app.rs`,
  `apps/remote/src/app.rs`) and vpn's `selection_tier` + `PANEL_BORDER_ALPHA` ring with
  `ui::chrome::selection_tier_on(theme, state, ground)`; the rail/overlay cards (`backups`
  `draw_rail`, `firewall` `draw_rail`) can use `ui::surface::card_on(.., fill, ground)`.
- **Open — needs coordinator (closed on `ux1x/integration`, F1/F2/F3)**: the shared controls in
  these captures (Toggle off-track, Segmented/ToggleGroup tracks, TextInput/SearchField wells,
  Neutral Button rest edge, ModalFrame/Toast, EmptyState) were rendered against the base toolkit;
  F1–F3 rest edges land at merge and these PNGs should be re-taken then (e.g. firewall service
  toggles, remote form segments, backups toast/modal frame).
- **Open — needs coordinator (tooling, `scripts/ux-bench/usability.py`)**: `geometry_audit` only
  descends into semantic children that carry `bounds`; remote's rows sit under `List`/`Group`/
  `Toolbar` nodes without bounds, so its list/form/session inspections measure 0 controls (the
  confirm dialogs, whose buttons are direct children, measure 3). Either give container nodes
  bounds in the app exports or let the walker descend through unbounded containers.
- **Open — pre-existing red at base (not A5 files)**: `design_token_audit::
  app_and_shell_theme_escape_hatches_do_not_grow` (`apps/browser/src/browser_tabs.rs`,
  `shell/src/shell/frame.rs`); `vpn::net::tests::real_loopback_sys_bytes_are_readable_and_monotonic`
  (Linux sysfs).

## Limits

- `const S1..S6 = Spacing::BASE.sN` (all four apps) are compile-time and do not follow
  `theme.tokens.spacing` under `ui_scale` / a custom `spacing_px` (accepted per plan, finding f).
- Fixed control sizes remain as authored dimensions, not rhythm: firewall `sel_w = 188`,
  `step_w = 30`, chip/segment widths; backups 30-px collapse toggle, 56-px retention rows, 250/116-px
  destination cards, 200-px ruler; remote 40-px fields, 150-px add button; vpn 30-px kv rows
  (`status_kv_table`), 72/56-px tiles, 176/96-px chart, 560-px `stack_header` breakpoint.
- The backups transient status bubble (shared status-bubble primitive) floats over the bottom-right
  of the content by design and can cover the last activity/snapshot row; the vpn notice band and
  the remote confirm scrims are likewise overlays, not layout neighbours.
- Segments of one segmented control (remote form Protocol/Authentication, vpn import kinds) share a
  track edge; that is the control's design, not a touching defect.
- `firewall selected`, `backups default-1280x480` and `vpn detail-1024x640` have no BEFORE PNG (the
  base binary had no `selected` scenario; the two viewports were added for findings e/c) — the
  before state is described from the base code in the tables.
- The `--responsive-matrix` desktop (1920×1080) PNGs and a few duplicate-state scenes were captured
  and byte-compared but not individually opened; every distinct surface was opened in dark and
  light as listed per app.
- Capture harness: `--headless-drive` was run with `--scenario <name>` as a CLI flag beside it
  (works at this base); the light-theme drive honours `QOS_SHOTS_THEME` (Unit 0).
- Wheel input cannot be scripted through `--headless-drive` (touch swipe only), so the vpn and
  backups scroll fixes are proven by unit tests plus the short-window PNGs rather than a scrolled
  screenshot.
